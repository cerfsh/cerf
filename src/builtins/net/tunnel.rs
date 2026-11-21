use crate::builtins::net::{addr, runtime};
use crate::engine::{ExecutionResult, ShellState};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TCP_CONNECTIONS: usize = 256;
const MAX_UDP_SESSIONS: usize = 1024;
const DATAGRAM_BUFFER: usize = 65_535;

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.tunnel",
        description: "Forwards a local TCP or UDP port to another host.",
        usage: "net.tunnel LOCAL_PORT TARGET_HOST TARGET_PORT [--bind IP] [--udp] [--idle-timeout SECONDS]",
        run: run_tunnel,
    };

#[derive(Debug, Clone)]
struct TunnelOptions {
    bind: SocketAddr,
    target_host: String,
    target_port: u16,
    udp: bool,
    idle_timeout: Duration,
}

fn parse_args(args: &[String]) -> Result<TunnelOptions, String> {
    let mut positional = Vec::new();
    let mut bind_ip = "127.0.0.1".to_string();
    let mut udp = false;
    let mut idle_timeout = DEFAULT_IDLE_TIMEOUT;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--bind" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--bind requires an IP address".to_string());
            };
            bind_ip = value.clone();
        } else if parse_options && argument == "--udp" {
            if udp {
                return Err("--udp was provided more than once".to_string());
            }
            udp = true;
        } else if parse_options && argument == "--idle-timeout" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--idle-timeout requires a value".to_string());
            };
            idle_timeout = addr::parse_seconds(value, "--idle-timeout")?;
        } else if parse_options && argument.starts_with('-') {
            return Err(format!("unknown option: {}", argument));
        } else {
            positional.push(argument.clone());
        }
        index += 1;
    }

    if positional.len() != 3 {
        return Err("expected LOCAL_PORT, TARGET_HOST, and TARGET_PORT".to_string());
    }

    Ok(TunnelOptions {
        bind: addr::bind_address(&bind_ip, addr::parse_port(&positional[0], true)?)?,
        target_host: positional[1].clone(),
        target_port: addr::parse_port(&positional[2], false)?,
        udp,
        idle_timeout,
    })
}

fn run_tunnel(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("net.tunnel: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    let result = runtime::block_on(async {
        if options.udp {
            tunnel_udp(&options).await
        } else {
            tunnel_tcp(&options).await
        }
    });

    match result {
        Ok(()) => (ExecutionResult::KeepRunning, 0),
        Err(error) => {
            eprintln!("net.tunnel: {}", error);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

async fn tunnel_tcp(options: &TunnelOptions) -> Result<(), io::Error> {
    let listener = TcpListener::bind(options.bind).await?;
    eprintln!("net.tunnel: listening on {}", listener.local_addr()?);

    let limit = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            task = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = task {
                    eprintln!("net.tunnel: forwarding task failed: {}", error);
                }
            }
            accepted = listener.accept() => {
                let (client, peer) = accepted?;
                let permit = Arc::clone(&limit).acquire_owned().await.map_err(io::Error::other)?;
                let host = options.target_host.clone();
                let port = options.target_port;
                tasks.spawn(async move {
                    let _permit = permit;
                    let upstream = match addr::connect_tcp(&host, port, CONNECT_TIMEOUT).await {
                        Ok(upstream) => upstream,
                        Err(error) => {
                            eprintln!("net.tunnel: {}: {}", peer, error);
                            return;
                        }
                    };
                    let mut client = client;
                    let mut upstream = upstream;
                    if let Err(error) =
                        tokio::io::copy_bidirectional(&mut client, &mut upstream).await
                    {
                        eprintln!("net.tunnel: {}: {}", peer, error);
                    }
                });
            }
        }
    }
}

struct UdpSession {
    id: usize,
    sender: mpsc::UnboundedSender<Vec<u8>>,
}

async fn tunnel_udp(options: &TunnelOptions) -> Result<(), io::Error> {
    let local = Arc::new(UdpSocket::bind(options.bind).await?);
    eprintln!("net.tunnel: listening on {}", local.local_addr()?);

    let (event_sender, mut event_receiver) = mpsc::unbounded_channel::<(SocketAddr, usize)>();
    let mut sessions: HashMap<SocketAddr, UdpSession> = HashMap::new();
    let mut next_id = 0_usize;
    let mut buffer = vec![0_u8; DATAGRAM_BUFFER];

    loop {
        tokio::select! {
            received = local.recv_from(&mut buffer) => {
                let (length, peer) = received?;
                let payload = buffer[..length].to_vec();

                if let Some(session) = sessions.get(&peer) {
                    let _ = session.sender.send(payload);
                } else if sessions.len() >= MAX_UDP_SESSIONS {
                    eprintln!("net.tunnel: UDP session limit reached; dropping datagram from {}", peer);
                } else {
                    let target = match addr::resolve(&options.target_host, options.target_port).await {
                        Ok(target) => target[0],
                        Err(error) => {
                            eprintln!("net.tunnel: {}: {}", peer, error);
                            continue;
                        }
                    };
                    let bind_address = if target.is_ipv4() {
                        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
                    } else {
                        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
                    };
                    let upstream = UdpSocket::bind(bind_address).await?;
                    if let Err(error) = upstream.connect(target).await {
                        eprintln!("net.tunnel: {}: {}", peer, error);
                        continue;
                    }

                    let (sender, receiver) = mpsc::unbounded_channel();
                    let id = next_id;
                    next_id += 1;
                    if sender.send(payload).is_err() {
                        continue;
                    }
                    sessions.insert(peer, UdpSession { id, sender });
                    let local_socket = Arc::clone(&local);
                    let event_sender = event_sender.clone();
                    let idle_timeout = options.idle_timeout;

                    tokio::spawn(async move {
                        if let Err(error) =
                            run_udp_session(local_socket, upstream, peer, receiver, idle_timeout)
                                .await
                        {
                            eprintln!("net.tunnel: {}: {}", peer, error);
                        }
                        let _ = event_sender.send((peer, id));
                    });
                }
            }
            event = event_receiver.recv() => {
                if let Some((peer, id)) = event {
                    if sessions.get(&peer).is_some_and(|session| session.id == id) {
                        sessions.remove(&peer);
                    }
                }
            }
        }
    }
}

async fn run_udp_session(
    local: Arc<UdpSocket>,
    upstream: UdpSocket,
    peer: SocketAddr,
    mut receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    idle_timeout: Duration,
) -> Result<(), io::Error> {
    let mut buffer = vec![0_u8; DATAGRAM_BUFFER];

    loop {
        tokio::select! {
            payload = receiver.recv() => {
                let Some(payload) = payload else { break };
                upstream.send(&payload).await?;
            }
            received = upstream.recv(&mut buffer) => {
                let length = received?;
                local.send_to(&buffer[..length], peer).await?;
            }
            _ = tokio::time::sleep(idle_timeout) => break,
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_and_udp_tunnels() {
        let tcp = parse_args(&[
            "8080".to_string(),
            "example.com".to_string(),
            "80".to_string(),
        ])
        .ok()
        .expect("parse");
        assert_eq!(tcp.bind, SocketAddr::from(([127, 0, 0, 1], 8080)));
        assert_eq!(tcp.target_port, 80);

        let udp = parse_args(&[
            "8080".to_string(),
            "example.com".to_string(),
            "53".to_string(),
            "--udp".to_string(),
            "--bind".to_string(),
            "::1".to_string(),
            "--idle-timeout".to_string(),
            "5".to_string(),
        ])
        .ok()
        .expect("parse");
        assert!(udp.udp);
        assert_eq!(udp.idle_timeout, Duration::from_secs(5));
    }

    #[test]
    fn rejects_incomplete_targets() {
        assert!(parse_args(&["8080".to_string(), "example.com".to_string()]).is_err());
        assert!(
            parse_args(&[
                "8080".to_string(),
                "example.com".to_string(),
                "0".to_string()
            ])
            .is_err()
        );
    }
}

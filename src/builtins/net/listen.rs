use crate::builtins::net::{addr, runtime, stream};
use crate::engine::{ExecutionResult, ShellState};
use std::io;
use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};

const DATAGRAM_BUFFER: usize = 65_535;

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.listen",
        description: "Listens for TCP or UDP traffic on standard input and output.",
        usage: "net.listen PORT [--bind IP] [--udp]\n\nTCP handles one client at a time. UDP uses one line of stdin per datagram and newline-frames received datagrams.",
        run: run_listen,
    };

#[derive(Debug, Clone)]
struct ListenOptions {
    bind: SocketAddr,
    udp: bool,
}

fn parse_args(args: &[String]) -> Result<ListenOptions, String> {
    let mut port = None;
    let mut bind_ip = "127.0.0.1".to_string();
    let mut udp = false;
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
        } else if parse_options && argument.starts_with('-') {
            return Err(format!("unknown option: {}", argument));
        } else if port.replace(addr::parse_port(argument, true)?).is_some() {
            return Err("expected exactly one PORT".to_string());
        }
        index += 1;
    }

    let Some(port) = port else {
        return Err("missing PORT".to_string());
    };

    Ok(ListenOptions {
        bind: addr::bind_address(&bind_ip, port)?,
        udp,
    })
}

fn run_listen(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("net.listen: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    let result = runtime::block_on(async {
        if options.udp {
            listen_udp(options.bind).await
        } else {
            listen_tcp(options.bind).await
        }
    });

    match result {
        Ok(()) => (ExecutionResult::KeepRunning, 0),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            (ExecutionResult::KeepRunning, 0)
        }
        Err(error) => {
            eprintln!("net.listen: {}", error);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

async fn listen_tcp(address: SocketAddr) -> Result<(), io::Error> {
    let listener = TcpListener::bind(address).await?;
    eprintln!("net.listen: listening on {}", listener.local_addr()?);

    loop {
        let (connection, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("net.listen: accept failed: {}", error);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                continue;
            }
        };
        eprintln!("net.listen: accepted {}", peer);

        if let Err(error) =
            stream::bridge_tcp(connection, tokio::io::stdin(), tokio::io::stdout()).await
        {
            if error.kind() == io::ErrorKind::BrokenPipe {
                return Err(error);
            }
            eprintln!("net.listen: connection from {} failed: {}", peer, error);
        }
    }
}

async fn listen_udp(address: SocketAddr) -> Result<(), io::Error> {
    let socket = UdpSocket::bind(address).await?;
    eprintln!("net.listen: listening on {}", socket.local_addr()?);

    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut buffer = vec![0_u8; DATAGRAM_BUFFER];
    let mut stdin_open = true;
    let mut last_peer: Option<SocketAddr> = None;
    let mut warned_without_peer = false;

    loop {
        tokio::select! {
            line = lines.next_line(), if stdin_open => {
                match line? {
                    Some(line) => match last_peer {
                        Some(peer) => {
                            socket.send_to(line.as_bytes(), peer).await?;
                        }
                        None if !warned_without_peer => {
                            eprintln!("net.listen: input ignored until a datagram identifies a peer");
                            warned_without_peer = true;
                        }
                        None => {}
                    },
                    None => stdin_open = false,
                }
            }
            received = socket.recv_from(&mut buffer) => {
                let (length, peer) = received?;
                last_peer = Some(peer);
                eprintln!("net.listen: {} bytes from {}", length, peer);
                stdout.write_all(&buffer[..length]).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_and_udp_listeners() {
        let tcp = parse_args(&["9000".to_string()]).expect("parse");
        assert_eq!(tcp.bind, SocketAddr::from(([127, 0, 0, 1], 9000)));
        assert!(!tcp.udp);

        let udp = parse_args(&[
            "--udp".to_string(),
            "--bind".to_string(),
            "0.0.0.0".to_string(),
            "9000".to_string(),
        ])
        .expect("parse");
        assert!(udp.udp);
        assert_eq!(udp.bind, SocketAddr::from(([0, 0, 0, 0], 9000)));
    }

    #[test]
    fn rejects_bad_port_and_bind_address() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["1".to_string(), "2".to_string()]).is_err());
        assert!(
            parse_args(&[
                "--bind".to_string(),
                "localhost".to_string(),
                "80".to_string()
            ])
            .is_err()
        );
    }
}

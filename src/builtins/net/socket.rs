use crate::builtins::net::{addr, runtime, stream};
use crate::engine::{ExecutionResult, ShellState};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;

const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const DATAGRAM_BUFFER: usize = 65_535;

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.socket",
        description: "Connects a TCP or UDP socket to standard input and output.",
        usage: "net.socket HOST PORT [--udp] [--connect-timeout SECONDS] [--idle-timeout SECONDS]\n\nTCP is a raw byte stream. UDP uses one line of stdin per datagram and newline-frames received datagrams. --idle-timeout applies to UDP.",
        run: run_socket,
    };

#[derive(Debug, Clone)]
struct SocketOptions {
    host: String,
    port: u16,
    udp: bool,
    connect_timeout: Duration,
    idle_timeout: Duration,
}

fn parse_args(args: &[String]) -> Result<SocketOptions, String> {
    let mut positional = Vec::new();
    let mut udp = false;
    let mut connect_timeout = DEFAULT_CONNECT_TIMEOUT;
    let mut idle_timeout = DEFAULT_IDLE_TIMEOUT;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--udp" {
            if udp {
                return Err("--udp was provided more than once".to_string());
            }
            udp = true;
        } else if parse_options && argument == "--connect-timeout" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--connect-timeout requires a value".to_string());
            };
            connect_timeout = addr::parse_seconds(value, "--connect-timeout")?;
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

    if positional.len() != 2 {
        return Err("expected HOST and PORT".to_string());
    }

    Ok(SocketOptions {
        host: positional.remove(0),
        port: addr::parse_port(&positional[0], false)?,
        udp,
        connect_timeout,
        idle_timeout,
    })
}

fn run_socket(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("net.socket: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    let result = runtime::block_on(async {
        if options.udp {
            run_udp(&options).await
        } else {
            run_tcp(&options).await
        }
    });

    match result {
        Ok(()) => (ExecutionResult::KeepRunning, 0),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            (ExecutionResult::KeepRunning, 0)
        }
        Err(error) => {
            eprintln!("net.socket: {}", error);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

async fn run_tcp(options: &SocketOptions) -> Result<(), io::Error> {
    let stream = addr::connect_tcp(&options.host, options.port, options.connect_timeout)
        .await
        .map_err(io::Error::other)?;
    if let Ok(address) = stream.peer_addr() {
        eprintln!("net.socket: connected to {}", address);
    }

    stream::bridge_tcp(stream, tokio::io::stdin(), tokio::io::stdout()).await
}

async fn run_udp(options: &SocketOptions) -> Result<(), io::Error> {
    let addresses = addr::resolve(&options.host, options.port)
        .await
        .map_err(io::Error::other)?;
    let target = addresses[0];
    let bind_address = if target.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let socket = UdpSocket::bind(bind_address).await?;
    socket.connect(target).await?;
    eprintln!("net.socket: connected to {}", target);

    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut buffer = vec![0_u8; DATAGRAM_BUFFER];
    let mut stdin_open = true;
    let mut idle_deadline = None;

    loop {
        let idle = async {
            if let Some(deadline) = idle_deadline {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        };

        tokio::select! {
            line = lines.next_line(), if stdin_open => {
                match line? {
                    Some(line) => {
                        socket.send(line.as_bytes()).await?;
                    }
                    None => {
                        stdin_open = false;
                        idle_deadline = Some(tokio::time::Instant::now() + options.idle_timeout);
                    }
                }
            }
            received = socket.recv(&mut buffer) => {
                let length = received?;
                stdout.write_all(&buffer[..length]).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            _ = idle, if !stdin_open => break,
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tcp_and_udp_options() {
        let tcp = parse_args(&["localhost".to_string(), "9000".to_string()]).expect("parse");
        assert!(!tcp.udp);
        assert_eq!(tcp.connect_timeout, DEFAULT_CONNECT_TIMEOUT);

        let udp = parse_args(&[
            "--udp".to_string(),
            "localhost".to_string(),
            "9000".to_string(),
            "--idle-timeout".to_string(),
            "2".to_string(),
        ])
        .expect("parse");
        assert!(udp.udp);
        assert_eq!(udp.idle_timeout, Duration::from_secs(2));
    }

    #[test]
    fn rejects_invalid_targets() {
        assert!(parse_args(&["localhost".to_string()]).is_err());
        assert!(parse_args(&["localhost".to_string(), "0".to_string()]).is_err());
        assert!(
            parse_args(&[
                "--tcp".to_string(),
                "localhost".to_string(),
                "80".to_string()
            ])
            .is_err()
        );
    }
}

use crate::builtins::net::client::{self, IpFamily};
use crate::builtins::net::runtime;
use crate::engine::{ExecutionResult, ShellState};

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.ip",
        description: "Prints the public IP address.",
        usage: "net.ip [-4 | -6]",
        run: run_ip,
    };

fn parse_family(args: &[String]) -> Result<IpFamily, String> {
    let mut family = None;
    for argument in args {
        let selected = match argument.as_str() {
            "-4" | "--ipv4" => IpFamily::V4,
            "-6" | "--ipv6" => IpFamily::V6,
            _ => return Err(format!("unknown argument: {}", argument)),
        };
        if family.replace(selected).is_some() {
            return Err("only one address family may be selected".to_string());
        }
    }
    Ok(family.unwrap_or(IpFamily::Any))
}

fn run_ip(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let family = match parse_family(args) {
        Ok(family) => family,
        Err(error) => {
            eprintln!("net.ip: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    match runtime::block_on(client::public_ip(family)) {
        Ok(address) => {
            println!("{}", address);
            (ExecutionResult::KeepRunning, 0)
        }
        Err(error) => {
            eprintln!("net.ip: {}", error);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn ip_service(body: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let address = listener.local_addr().expect("server address");
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.expect("write");
            stream.shutdown().await.expect("close");
        });
        address
    }

    #[test]
    fn parses_address_family_flags() {
        assert_eq!(parse_family(&[]).ok(), Some(IpFamily::Any));
        assert_eq!(parse_family(&["-4".to_string()]).ok(), Some(IpFamily::V4));
        assert_eq!(
            parse_family(&["--ipv6".to_string()]).ok(),
            Some(IpFamily::V6)
        );
        assert!(parse_family(&["-4".to_string(), "-6".to_string()]).is_err());
        assert!(parse_family(&["--all".to_string()]).is_err());
    }

    #[tokio::test]
    async fn parses_service_response() {
        let address = ip_service("203.0.113.9\n").await;
        let endpoint = format!("http://{address}/");
        let result = client::public_ip_from(&endpoint, IpFamily::V4).await;
        assert_eq!(result.ok(), Some("203.0.113.9".parse().expect("address")));
    }

    #[tokio::test]
    async fn rejects_invalid_service_response() {
        let address = ip_service("not-an-ip\n").await;
        let endpoint = format!("http://{address}/");
        assert!(
            client::public_ip_from(&endpoint, IpFamily::Any)
                .await
                .is_err()
        );
    }
}

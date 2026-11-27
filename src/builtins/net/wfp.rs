use crate::builtins::net::{addr, runtime};
use crate::engine::{ExecutionResult, ShellState};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_INTERVAL: Duration = Duration::from_millis(250);
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(1);

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.wfp",
        description: "Waits until a TCP port accepts connections.",
        usage: "net.wfp HOST PORT [--timeout SECONDS] [--interval-ms MILLISECONDS]",
        run: run_wfp,
    };

#[derive(Debug, Clone)]
struct WaitOptions {
    host: String,
    port: u16,
    timeout: Duration,
    interval: Duration,
}

fn parse_args(args: &[String]) -> Result<WaitOptions, String> {
    let mut positional = Vec::new();
    let mut timeout = DEFAULT_TIMEOUT;
    let mut interval = DEFAULT_INTERVAL;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--timeout" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--timeout requires a value".to_string());
            };
            timeout = addr::parse_seconds(value, "--timeout")?;
        } else if parse_options && argument == "--interval-ms" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--interval-ms requires a value".to_string());
            };
            interval = Duration::from_millis(addr::parse_milliseconds(value, "--interval-ms")?);
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

    Ok(WaitOptions {
        host: positional.remove(0),
        port: addr::parse_port(&positional[0], false)?,
        timeout,
        interval,
    })
}

fn run_wfp(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("net.wfp: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    match runtime::block_on(wait_for_port(&options)) {
        Ok(()) => (ExecutionResult::KeepRunning, 0),
        Err(error) => {
            eprintln!("net.wfp: {}", error);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

async fn wait_for_port(options: &WaitOptions) -> Result<(), String> {
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .ok_or_else(|| "--timeout is too large".to_string())?;
    let mut last_error = format!("{}:{} is not reachable", options.host, options.port);

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            match tokio::time::timeout(remaining, addr::resolve(&options.host, options.port)).await
            {
                Ok(Ok(addresses)) => {
                    for address in addresses {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        let attempt_timeout = remaining.min(ATTEMPT_TIMEOUT);
                        match tokio::time::timeout(attempt_timeout, TcpStream::connect(address))
                            .await
                        {
                            Ok(Ok(_)) => return Ok(()),
                            Ok(Err(error)) => last_error = format!("{}: {}", address, error),
                            Err(_) => last_error = format!("{}: connection timed out", address),
                        }
                    }
                }
                Ok(Err(_)) => {}
                Err(_) => last_error = format!("{}: resolution timed out", options.host),
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timed out after {} seconds waiting for {}:{} ({})",
                options.timeout.as_secs(),
                options.host,
                options.port,
                last_error
            ));
        }
        tokio::time::sleep(options.interval.min(remaining)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    fn options(address: SocketAddr, timeout: Duration) -> WaitOptions {
        WaitOptions {
            host: address.ip().to_string(),
            port: address.port(),
            timeout,
            interval: Duration::from_millis(10),
        }
    }

    #[tokio::test]
    async fn succeeds_against_open_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        assert!(
            wait_for_port(&options(address, Duration::from_secs(1)))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn times_out_against_closed_port() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        drop(listener);

        assert!(
            wait_for_port(&options(address, Duration::from_millis(150)))
                .await
                .is_err()
        );
    }

    #[test]
    fn parses_positional_and_options() {
        let options = parse_args(&[
            "localhost".to_string(),
            "443".to_string(),
            "--timeout".to_string(),
            "2".to_string(),
            "--interval-ms=50".to_string(),
        ]);
        assert!(
            options.is_err(),
            "unsupported equals syntax should be rejected"
        );

        let options = parse_args(&[
            "--timeout".to_string(),
            "2".to_string(),
            "localhost".to_string(),
            "443".to_string(),
            "--interval-ms".to_string(),
            "50".to_string(),
        ])
        .expect("parse");
        assert_eq!(options.port, 443);
        assert_eq!(options.timeout, Duration::from_secs(2));
    }
}

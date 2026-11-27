use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

pub fn parse_port(value: &str, allow_zero: bool) -> Result<u16, String> {
    let port: u16 = value
        .parse()
        .map_err(|_| format!("invalid port: {}", value))?;
    if !allow_zero && port == 0 {
        return Err("port must be between 1 and 65535".to_string());
    }
    Ok(port)
}

pub fn parse_seconds(value: &str, option: &str) -> Result<Duration, String> {
    let seconds: u64 = value
        .parse()
        .map_err(|_| format!("{} expects a whole number of seconds", option))?;
    if seconds == 0 {
        return Err(format!("{} must be greater than zero", option));
    }
    let duration = Duration::from_secs(seconds);
    // Reject durations that cannot be represented as a deadline instead of
    // panicking later when `Instant + duration` overflows.
    if std::time::Instant::now().checked_add(duration).is_none() {
        return Err(format!("{} is too large", option));
    }
    Ok(duration)
}

pub fn parse_milliseconds(value: &str, option: &str) -> Result<u64, String> {
    let milliseconds: u64 = value
        .parse()
        .map_err(|_| format!("{} expects a whole number of milliseconds", option))?;
    if milliseconds == 0 {
        return Err(format!("{} must be greater than zero", option));
    }
    Ok(milliseconds)
}

pub fn bind_address(value: &str, port: u16) -> Result<SocketAddr, String> {
    let ip: IpAddr = value
        .parse()
        .map_err(|_| format!("invalid bind address: {}", value))?;
    Ok(SocketAddr::new(ip, port))
}

pub async fn resolve(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("cannot resolve {}: {}", host, error))?;
    let mut unique = Vec::new();
    for address in addresses {
        if !unique.contains(&address) {
            unique.push(address);
        }
    }
    if unique.is_empty() {
        return Err(format!("{} resolved to no addresses", host));
    }
    Ok(unique)
}

pub async fn connect_tcp(host: &str, port: u16, timeout: Duration) -> Result<TcpStream, String> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| "connection timeout is too large".to_string())?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(format!("cannot resolve {}: resolution timed out", host));
    }
    let addresses = match tokio::time::timeout(remaining, resolve(host, port)).await {
        Ok(result) => result?,
        Err(_) => return Err(format!("cannot resolve {}: resolution timed out", host)),
    };
    let mut last_error = format!("could not connect to {}:{}", host, port);

    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, TcpStream::connect(address)).await {
            Ok(Ok(stream)) => return Ok(stream),
            Ok(Err(error)) => last_error = format!("{}: {}", address, error),
            Err(_) => last_error = format!("{}: connection timed out", address),
        }
    }

    Err(last_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ports_and_durations() {
        assert_eq!(parse_port("0", true).ok(), Some(0));
        assert!(parse_port("0", false).is_err());
        assert!(parse_port("65536", true).is_err());
        assert!(parse_port("http", true).is_err());
        assert!(parse_seconds("0", "--timeout").is_err());
        assert!(parse_seconds("18446744073709551615", "--timeout").is_err());
        assert!(parse_milliseconds("0", "--interval-ms").is_err());
        assert_eq!(
            parse_seconds("5", "--timeout").ok(),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn parses_ipv4_and_ipv6_bind_addresses() {
        assert_eq!(
            bind_address("127.0.0.1", 8080).ok(),
            Some(SocketAddr::from(([127, 0, 0, 1], 8080)))
        );
        assert_eq!(
            bind_address("::1", 8080).ok(),
            Some(SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], 8080)))
        );
        assert!(bind_address("localhost", 8080).is_err());
    }
}

use crate::builtins::net::{addr, runtime};
use crate::engine::{ExecutionResult, ShellState};
use hickory_resolver::TokioResolver;
use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig};
use hickory_resolver::name_server::TokioConnectionProvider;
use hickory_resolver::proto::rr::{Name, RecordType};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.dns",
        description: "Resolves DNS records.",
        usage: "net.dns NAME [--type TYPE] [--server IP[:PORT]] [--timeout SECONDS]\n\nTYPE is one of A, AAAA, CNAME, MX, NS, TXT, or PTR.",
        run: run_dns,
    };

#[derive(Debug, Clone)]
struct DnsOptions {
    name: String,
    record_types: Vec<RecordType>,
    server: Option<SocketAddr>,
    timeout: Duration,
}

fn parse_record_type(value: &str) -> Result<RecordType, String> {
    match value.to_ascii_uppercase().as_str() {
        "A" => Ok(RecordType::A),
        "AAAA" => Ok(RecordType::AAAA),
        "CNAME" => Ok(RecordType::CNAME),
        "MX" => Ok(RecordType::MX),
        "NS" => Ok(RecordType::NS),
        "TXT" => Ok(RecordType::TXT),
        "PTR" => Ok(RecordType::PTR),
        _ => Err(format!("unsupported DNS record type: {}", value)),
    }
}

fn parse_server(value: &str) -> Result<SocketAddr, String> {
    if let Ok(address) = value.parse::<SocketAddr>() {
        return Ok(address);
    }
    let ip: IpAddr = value
        .parse()
        .map_err(|_| format!("invalid DNS server: {}", value))?;
    Ok(SocketAddr::new(ip, 53))
}

fn parse_args(args: &[String]) -> Result<DnsOptions, String> {
    let mut name = None;
    let mut record_types = None;
    let mut server = None;
    let mut timeout = DEFAULT_TIMEOUT;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && (argument == "--type" || argument == "-t") {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--type requires a value".to_string());
            };
            if record_types.is_some() {
                return Err("--type was provided more than once".to_string());
            }
            record_types = Some(vec![parse_record_type(value)?]);
        } else if parse_options && argument == "--server" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--server requires an address".to_string());
            };
            if server.replace(parse_server(value)?).is_some() {
                return Err("--server was provided more than once".to_string());
            }
        } else if parse_options && argument == "--timeout" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--timeout requires a value".to_string());
            };
            timeout = addr::parse_seconds(value, "--timeout")?;
        } else if parse_options && argument.starts_with('-') {
            return Err(format!("unknown option: {}", argument));
        } else if name.replace(argument.clone()).is_some() {
            return Err("expected exactly one NAME".to_string());
        }
        index += 1;
    }

    let Some(name) = name else {
        return Err("missing NAME".to_string());
    };

    let record_types = match record_types {
        Some(types) => types,
        None => {
            if name.parse::<IpAddr>().is_ok() {
                vec![RecordType::PTR]
            } else {
                vec![RecordType::A, RecordType::AAAA]
            }
        }
    };

    Ok(DnsOptions {
        name,
        record_types,
        server,
        timeout,
    })
}

fn run_dns(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("net.dns: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    match runtime::block_on(resolve(&options)) {
        Ok((records, errors)) => {
            for record in records {
                println!("{}", record);
            }
            for error in &errors {
                eprintln!("net.dns: {}", error);
            }
            (
                ExecutionResult::KeepRunning,
                if errors.is_empty() { 0 } else { 1 },
            )
        }
        Err(error) => {
            eprintln!("net.dns: {}", error);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

async fn resolve(options: &DnsOptions) -> Result<(Vec<String>, Vec<String>), String> {
    let resolver = build_resolver(options)?;
    let name = lookup_name(&options.name)?;
    let mut records = Vec::new();
    let mut errors = Vec::new();

    for record_type in &options.record_types {
        match resolver.lookup(name.clone(), *record_type).await {
            Ok(lookup) => {
                records.extend(
                    lookup
                        .iter()
                        .map(|data| format!("{}\t{}", data.record_type(), data)),
                );
            }
            Err(error) => errors.push(format!("{}: {}", record_type, error)),
        }
    }

    records.sort();
    records.dedup();
    if records.is_empty() && !errors.is_empty() {
        return Err(errors.join("; "));
    }

    Ok((records, errors))
}

fn build_resolver(options: &DnsOptions) -> Result<TokioResolver, String> {
    let mut builder = match options.server {
        Some(server) => {
            let config = ResolverConfig::from_parts(
                None,
                Vec::new(),
                NameServerConfigGroup::from_ips_clear(&[server.ip()], server.port(), true),
            );
            TokioResolver::builder_with_config(config, TokioConnectionProvider::default())
        }
        None => TokioResolver::builder_tokio()
            .map_err(|error| format!("cannot read system DNS configuration: {}", error))?,
    };
    builder.options_mut().timeout = options.timeout;
    builder.options_mut().attempts = 2;
    Ok(builder.build())
}

fn lookup_name(value: &str) -> Result<Name, String> {
    if let Ok(address) = value.parse::<IpAddr>() {
        let name = match address {
            IpAddr::V4(address) => {
                let octets = address.octets();
                format!(
                    "{}.{}.{}.{}.in-addr.arpa.",
                    octets[3], octets[2], octets[1], octets[0]
                )
            }
            IpAddr::V6(address) => {
                let mut labels = String::new();
                for byte in address.octets().iter().rev() {
                    labels.push_str(&format!("{:x}.{:x}.", byte & 0x0f, byte >> 4));
                }
                format!("{}ip6.arpa.", labels)
            }
        };
        Name::from_ascii(name).map_err(|error| format!("invalid reverse lookup name: {}", error))
    } else {
        Name::from_utf8(value).map_err(|error| format!("invalid DNS name: {}", error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_record_types_and_defaults() {
        let options = parse_args(&["example.com".to_string()]).expect("parse");
        assert_eq!(options.record_types, vec![RecordType::A, RecordType::AAAA]);

        let options = parse_args(&[
            "192.0.2.1".to_string(),
            "--type".to_string(),
            "mx".to_string(),
            "--server".to_string(),
            "127.0.0.1:5353".to_string(),
        ])
        .expect("parse");
        assert_eq!(options.record_types, vec![RecordType::MX]);
        assert_eq!(
            options.server,
            Some(SocketAddr::from(([127, 0, 0, 1], 5353)))
        );
    }

    #[test]
    fn builds_reverse_names() {
        assert_eq!(
            lookup_name("192.0.2.1").ok().map(|name| name.to_utf8()),
            Some("1.2.0.192.in-addr.arpa.".to_string())
        );
        assert!(
            lookup_name("2001:db8::1")
                .expect("reverse name")
                .to_utf8()
                .ends_with("ip6.arpa.")
        );
    }

    #[test]
    fn rejects_invalid_options() {
        assert!(parse_args(&[]).is_err());
        assert!(parse_args(&["example.com".to_string(), "--type".to_string()]).is_err());
        assert!(
            parse_args(&[
                "example.com".to_string(),
                "--type".to_string(),
                "SRV".to_string()
            ])
            .is_err()
        );
    }
}

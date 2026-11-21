use crate::builtins::net::client::{self, IpFamily};
use crate::builtins::net::runtime;
use crate::engine::{ExecutionResult, ShellState};
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use sysinfo::Networks;

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.whoami",
        description: "Shows public and local network addresses.",
        usage: "net.whoami",
        run: run_whoami,
    };

#[derive(Debug, Clone, PartialEq, Eq)]
struct InterfaceInfo {
    name: String,
    addresses: Vec<(IpAddr, u8)>,
}

fn route_primary(ipv6: bool) -> Option<IpAddr> {
    let (bind, target) = if ipv6 {
        (
            SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
            SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1), 9)),
        )
    } else {
        (
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 9)),
        )
    };
    let socket = UdpSocket::bind(bind).ok()?;
    socket.connect(target).ok()?;
    Some(socket.local_addr().ok()?.ip())
}

fn local_interfaces() -> Vec<InterfaceInfo> {
    let networks = Networks::new_with_refreshed_list();
    let mut interfaces = BTreeMap::new();

    for (name, data) in &networks {
        let mut addresses: Vec<(IpAddr, u8)> = data
            .ip_networks()
            .iter()
            .map(|network| (network.addr, network.prefix))
            .collect();
        addresses.sort();
        addresses.dedup();
        if !addresses.is_empty() {
            interfaces.insert(name.clone(), addresses);
        }
    }

    interfaces
        .into_iter()
        .map(|(name, addresses)| InterfaceInfo { name, addresses })
        .collect()
}

fn format_whoami(
    public: Option<IpAddr>,
    primary_v4: Option<IpAddr>,
    primary_v6: Option<IpAddr>,
    interfaces: &[InterfaceInfo],
) -> String {
    let mut output = String::new();
    if let Some(address) = public {
        output.push_str(&format!("public: {}\n", address));
    }
    if let Some(address) = primary_v4 {
        output.push_str(&format!("primary IPv4: {}\n", address));
    }
    if let Some(address) = primary_v6 {
        output.push_str(&format!("primary IPv6: {}\n", address));
    }
    for interface in interfaces {
        let addresses = interface
            .addresses
            .iter()
            .map(|(address, prefix)| format!("{}/{}", address, prefix))
            .collect::<Vec<_>>()
            .join(", ");
        output.push_str(&format!("interface {}: {}\n", interface.name, addresses));
    }
    output
}

fn run_whoami(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    if !args.is_empty() {
        eprintln!("Usage: {}", COMMAND_INFO.usage);
        return (ExecutionResult::KeepRunning, 1);
    }

    let public = runtime::block_on(client::public_ip(IpFamily::Any));
    let interfaces = local_interfaces();
    let primary_v4 = route_primary(false);
    let primary_v6 = route_primary(true);

    match &public {
        Ok(address) => print!(
            "{}",
            format_whoami(Some(*address), primary_v4, primary_v6, &interfaces)
        ),
        Err(error) => {
            eprintln!("net.whoami: {}", error);
            print!(
                "{}",
                format_whoami(None, primary_v4, primary_v6, &interfaces)
            );
        }
    }

    if public.is_err() {
        (ExecutionResult::KeepRunning, 1)
    } else {
        (ExecutionResult::KeepRunning, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_output_in_stable_order() {
        let interfaces = vec![
            InterfaceInfo {
                name: "eth0".to_string(),
                addresses: vec![
                    ("192.0.2.10".parse().expect("IPv4"), 24),
                    ("2001:db8::1".parse().expect("IPv6"), 64),
                ],
            },
            InterfaceInfo {
                name: "loopback".to_string(),
                addresses: vec![("127.0.0.1".parse().expect("loopback"), 8)],
            },
        ];

        let output = format_whoami(
            Some("203.0.113.5".parse().expect("public")),
            Some("192.0.2.10".parse().expect("primary")),
            None,
            &interfaces,
        );

        assert_eq!(
            output,
            "public: 203.0.113.5\n\
             primary IPv4: 192.0.2.10\n\
             interface eth0: 192.0.2.10/24, 2001:db8::1/64\n\
             interface loopback: 127.0.0.1/8\n"
        );
    }
}

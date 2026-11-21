use reqwest::Client;
use std::net::IpAddr;
use std::sync::LazyLock;
use std::time::Duration;

const PUBLIC_IP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PUBLIC_IP_RESPONSE: usize = 128;
const IPV4_ENDPOINT: &str = "https://api.ipify.org";
const IPV6_ENDPOINT: &str = "https://api6.ipify.org";
const DUAL_STACK_ENDPOINT: &str = "https://api64.ipify.org";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpFamily {
    Any,
    V4,
    V6,
}

impl IpFamily {
    fn endpoint(self) -> &'static str {
        match self {
            Self::Any => DUAL_STACK_ENDPOINT,
            Self::V4 => IPV4_ENDPOINT,
            Self::V6 => IPV6_ENDPOINT,
        }
    }

    fn accepts(self, address: IpAddr) -> bool {
        match self {
            Self::Any => true,
            Self::V4 => address.is_ipv4(),
            Self::V6 => address.is_ipv6(),
        }
    }
}

static CLIENT: LazyLock<Result<Client, String>> = LazyLock::new(|| {
    Client::builder()
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|error| format!("could not initialize HTTP client: {}", error.without_url()))
});

/// Shared asynchronous HTTP client used by all network builtins.
pub fn client() -> Result<&'static Client, String> {
    CLIENT.as_ref().map_err(Clone::clone)
}

pub async fn public_ip(family: IpFamily) -> Result<IpAddr, String> {
    public_ip_from(family.endpoint(), family).await
}

pub(crate) async fn public_ip_from(endpoint: &str, family: IpFamily) -> Result<IpAddr, String> {
    let lookup = async {
        let mut response = client()?
            .get(endpoint)
            .send()
            .await
            .map_err(|error| format!("public IP request failed: {}", error.without_url()))?;
        if !response.status().is_success() {
            return Err(format!(
                "public IP service returned status {}",
                response.status()
            ));
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            format!("could not read public IP response: {}", error.without_url())
        })? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_PUBLIC_IP_RESPONSE {
                return Err("public IP response was too large".to_string());
            }
        }

        let value = std::str::from_utf8(&body)
            .map_err(|_| "public IP response was not valid UTF-8".to_string())?
            .trim_end_matches(['\r', '\n']);
        let address: IpAddr = value
            .parse()
            .map_err(|_| "public IP service returned an invalid address".to_string())?;
        if !family.accepts(address) {
            return Err("public IP service returned the wrong address family".to_string());
        }
        Ok(address)
    };

    match tokio::time::timeout(PUBLIC_IP_TIMEOUT, lookup).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "public IP lookup timed out after {} seconds",
            PUBLIC_IP_TIMEOUT.as_secs()
        )),
    }
}

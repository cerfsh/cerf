use reqwest::Client;
use std::sync::LazyLock;

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

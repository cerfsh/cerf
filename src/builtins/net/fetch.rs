use crate::engine::{ExecutionResult, ShellState};
use reqwest::blocking::Client;
use std::io::{self, Write};
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo = crate::builtins::registry::CommandInfo {
    name: "net.fetch",
    description: "Fetches the contents of a URL.",
    usage: "net.fetch <url>",
    run: run_fetch,
};

// Reuse a single HTTP client for all requests
static HTTP_CLIENT: LazyLock<Result<Client, reqwest::Error>> = LazyLock::new(|| {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("CerfSh/1.0")
        .build()
});

fn run_fetch(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    if args.len() != 1 {
        eprintln!("Usage: {}", COMMAND_INFO.usage);
        return (ExecutionResult::Failure, 1);
    }

    if let Err(e) = fetch_url(&args[0]) {
        eprintln!("Error: {}", e);
        return (ExecutionResult::Failure, 1);
    }

    (ExecutionResult::Success, 0)
}

fn fetch_url(raw_url: &str) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Validate URL
    let parsed_url = Url::parse(raw_url)?;
    if parsed_url.scheme() != "http" && parsed_url.scheme() != "https" {
        return Err("Only 'http' and 'https' protocols are supported.".into());
    }

    // 2. Get client from Lazy Client Pool
    let client = HTTP_CLIENT.as_ref().map_err(|e| format!("HTTP Client init failed: {}", e))?;

    // 3. Do Request
    let mut response = client.get(parsed_url).send()?;

    if !response.status().is_success() {
        return Err(format!("Request failed with status: {}", response.status()).into());
    }

    // 4. Lock Stdout to increase speed of stream
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    
    io::copy(&mut response, &mut handle)?;
    handle.flush()?;

    Ok(())
}
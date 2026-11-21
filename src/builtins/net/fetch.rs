use crate::builtins::net::{client, runtime};
use crate::engine::path::expand_home;
use crate::engine::{ExecutionResult, ShellState};
use reqwest::Response;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use url::Url;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.fetch",
        description: "Fetches the contents of a URL.",
        usage: "net.fetch [--output PATH] [--] URL\n\nWrite the response body to stdout, or to PATH when -o/--output is used. Use -o - for stdout.",
        run: run_fetch,
    };

struct FetchOptions {
    url: Url,
    output: Option<PathBuf>,
}

enum FetchError {
    BrokenPipe,
    Message(String),
}

impl FetchError {
    fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

impl From<io::Error> for FetchError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::BrokenPipe {
            Self::BrokenPipe
        } else {
            Self::Message(error.to_string())
        }
    }
}

fn parse_args(args: &[String]) -> Result<FetchOptions, String> {
    let mut url = None;
    let mut output = None;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && (argument == "-o" || argument == "--output") {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err(format!("{} requires a path", argument));
            };
            if value.is_empty() {
                return Err("output path must not be empty".to_string());
            }
            if output.replace(expand_home(value)).is_some() {
                return Err("output path was provided more than once".to_string());
            }
        } else if parse_options {
            if let Some(value) = argument.strip_prefix("--output=") {
                if value.is_empty() {
                    return Err("output path must not be empty".to_string());
                }
                if output.replace(expand_home(value)).is_some() {
                    return Err("output path was provided more than once".to_string());
                }
            } else if argument.starts_with('-') {
                return Err(format!("unknown option: {}", argument));
            } else if url.is_some() {
                return Err("expected exactly one URL".to_string());
            } else {
                url = Some(argument.clone());
            }
        } else if url.is_some() {
            return Err("expected exactly one URL".to_string());
        } else {
            url = Some(argument.clone());
        }
        index += 1;
    }

    let Some(raw_url) = url else {
        return Err("missing URL".to_string());
    };
    let url = Url::parse(&raw_url).map_err(|_| "invalid URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs are supported".to_string());
    }

    Ok(FetchOptions { url, output })
}

fn run_fetch(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("net.fetch: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    let result =
        runtime::block_on(
            async move { tokio::time::timeout(REQUEST_TIMEOUT, fetch(options)).await },
        );

    match result {
        Ok(Ok(())) => (ExecutionResult::KeepRunning, 0),
        Ok(Err(FetchError::BrokenPipe)) => (ExecutionResult::KeepRunning, 0),
        Ok(Err(FetchError::Message(message))) => {
            eprintln!("net.fetch: {}", message);
            (ExecutionResult::KeepRunning, 1)
        }
        Err(_) => {
            eprintln!(
                "net.fetch: request timed out after {} seconds",
                REQUEST_TIMEOUT.as_secs()
            );
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

async fn fetch(options: FetchOptions) -> Result<(), FetchError> {
    let client = client::client().map_err(FetchError::Message)?;
    let mut response =
        client.get(options.url).send().await.map_err(|error| {
            FetchError::message(format!("request failed: {}", error.without_url()))
        })?;

    if !response.status().is_success() {
        return Err(FetchError::message(format!(
            "request failed with status {}",
            response.status()
        )));
    }

    match options.output {
        Some(path) if path.as_os_str() != "-" => write_file(&mut response, &path).await,
        _ => {
            let mut stdout = tokio::io::stdout();
            copy_response(&mut response, &mut stdout).await?;
            stdout.flush().await?;
            Ok(())
        }
    }
}

async fn write_file(response: &mut Response, path: &Path) -> Result<(), FetchError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        FetchError::message(format!(
            "cannot create temporary file in {}: {}",
            parent.display(),
            error
        ))
    })?;
    let file = temporary.reopen().map_err(|error| {
        FetchError::message(format!("cannot open temporary output file: {}", error))
    })?;
    let mut file = tokio::fs::File::from_std(file);

    copy_response(response, &mut file).await?;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);

    temporary.persist(path).map_err(|error| {
        FetchError::message(format!(
            "cannot replace {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(())
}

async fn copy_response<W: AsyncWrite + Unpin>(
    response: &mut Response,
    writer: &mut W,
) -> Result<(), FetchError> {
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        FetchError::message(format!("could not read response: {}", error.without_url()))
    })? {
        writer.write_all(&chunk).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve_once(status: &str, body: &[u8]) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let address = listener.local_addr().expect("server address");
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        let body = body.to_vec();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await;
            stream.write_all(&response).await.expect("write headers");
            stream.write_all(&body).await.expect("write body");
            stream.shutdown().await.expect("close response");
        });

        address
    }

    fn options(address: SocketAddr, output: Option<PathBuf>) -> FetchOptions {
        FetchOptions {
            url: Url::parse(&format!("http://{address}/asset")).expect("test URL"),
            output,
        }
    }

    #[tokio::test]
    async fn writes_binary_body_to_file() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("asset.bin");
        let address = serve_once("200 OK", b"a\0b\xff").await;

        assert!(fetch(options(address, Some(path.clone()))).await.is_ok());

        assert_eq!(std::fs::read(path).expect("read output"), b"a\0b\xff");
    }

    #[tokio::test]
    async fn failed_response_does_not_replace_destination() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("existing.bin");
        std::fs::write(&path, b"keep").expect("seed destination");
        let address = serve_once("404 Not Found", b"ignored").await;

        assert!(fetch(options(address, Some(path.clone()))).await.is_err());
        assert_eq!(std::fs::read(path).expect("read output"), b"keep");
    }

    #[test]
    fn parses_output_before_or_after_url() {
        let before = parse_args(&[
            "--output".to_string(),
            "download.bin".to_string(),
            "https://example.com/file".to_string(),
        ])
        .expect("parse");
        assert_eq!(before.output, Some(PathBuf::from("download.bin")));

        let after = parse_args(&[
            "https://example.com/file".to_string(),
            "-o".to_string(),
            "download.bin".to_string(),
        ])
        .expect("parse");
        assert_eq!(after.output, Some(PathBuf::from("download.bin")));
    }

    #[test]
    fn rejects_non_http_schemes_and_unknown_options() {
        assert!(parse_args(&["file:///etc/passwd".to_string()]).is_err());
        assert!(
            parse_args(&["--insecure".to_string(), "https://example.com".to_string()]).is_err()
        );
        assert!(parse_args(&["-o".to_string()]).is_err());
        assert!(parse_args(&["--output=".to_string(), "https://example.com".to_string()]).is_err());
    }

    #[test]
    fn command_creates_its_own_runtime_context() {
        let mut state = ShellState::new();
        let (_, code) = run_fetch(&["http://127.0.0.1:9/".to_string()], &mut state);
        assert_eq!(code, 1);
    }
}

use crate::builtins::net::{addr, runtime};
use crate::engine::{ExecutionResult, ShellState};
use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

pub const COMMAND_INFO: crate::builtins::registry::CommandInfo =
    crate::builtins::registry::CommandInfo {
        name: "net.http",
        description: "Serves a directory over HTTP.",
        usage: "net.http [ROOT] [--bind IP] [--port PORT]",
        run: run_http,
    };

#[derive(Debug, Clone)]
struct HttpOptions {
    root: PathBuf,
    bind: SocketAddr,
}

fn parse_args(args: &[String]) -> Result<HttpOptions, String> {
    let mut root = None;
    let mut bind_ip = "127.0.0.1".to_string();
    let mut port = 8000;
    let mut parse_options = true;
    let mut index = 0;

    while index < args.len() {
        let argument = &args[index];
        if parse_options && argument == "--" {
            parse_options = false;
        } else if parse_options && argument == "--bind" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--bind requires an IP address".to_string());
            };
            bind_ip = value.clone();
        } else if parse_options && argument == "--port" {
            index += 1;
            let Some(value) = args.get(index) else {
                return Err("--port requires a value".to_string());
            };
            port = addr::parse_port(value, true)?;
        } else if parse_options && argument.starts_with('-') {
            return Err(format!("unknown option: {}", argument));
        } else if root
            .replace(crate::engine::path::expand_home(argument))
            .is_some()
        {
            return Err("expected at most one ROOT directory".to_string());
        }
        index += 1;
    }

    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot open {}: {}", root.display(), error))?;
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    Ok(HttpOptions {
        root,
        bind: addr::bind_address(&bind_ip, port)?,
    })
}

fn run_http(args: &[String], _state: &mut ShellState) -> (ExecutionResult, i32) {
    let options = match parse_args(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("net.http: {}", error);
            eprintln!("Usage: {}", COMMAND_INFO.usage);
            return (ExecutionResult::KeepRunning, 1);
        }
    };

    if !options.bind.ip().is_loopback() {
        eprintln!(
            "net.http: warning: serving {} on a non-loopback address",
            options.root.display()
        );
    }

    match runtime::block_on(serve(options)) {
        Ok(()) => (ExecutionResult::KeepRunning, 0),
        Err(error) => {
            eprintln!("net.http: {}", error);
            (ExecutionResult::KeepRunning, 1)
        }
    }
}

async fn serve(options: HttpOptions) -> Result<(), String> {
    let listener = TcpListener::bind(options.bind)
        .await
        .map_err(|error| format!("cannot bind {}: {}", options.bind, error))?;
    let bound = listener
        .local_addr()
        .map_err(|error| format!("cannot read listener address: {}", error))?;
    eprintln!(
        "net.http: serving {} on http://{}",
        options.root.display(),
        bound
    );

    let root = Arc::new(options.root);
    let listing = Router::new()
        .fallback(get(list_directory))
        .with_state(Arc::clone(&root));
    let service = ServeDir::new(root.as_path()).not_found_service(listing);
    let app = Router::new().fallback_service(service);

    axum::serve(listener, app)
        .await
        .map_err(|error| format!("server failed: {}", error))
}

async fn list_directory(
    State(root): State<Arc<PathBuf>>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let Some(directory) = resolve_path(&root, uri.path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !directory.is_dir() {
        return StatusCode::NOT_FOUND.into_response();
    }

    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("net.http: cannot list {}: {}", directory.display(), error);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let mut names = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let is_directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        names.push((is_directory, name));
    }
    names.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let mut html = String::from(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Directory listing</title></head><body><h1>Directory listing</h1><ul>",
    );
    if uri.path() != "/" {
        html.push_str("<li><a href=\"../\">../</a></li>");
    }
    for (is_directory, name) in names {
        let encoded = percent_encode(&name);
        let suffix = if is_directory { "/" } else { "" };
        html.push_str(&format!(
            "<li><a href=\"{}{}\">{}{}</a></li>",
            encoded,
            suffix,
            escape_html(&name),
            suffix
        ));
    }
    html.push_str("</ul></body></html>");

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        Body::from(html),
    )
        .into_response()
}

fn resolve_path(root: &Path, request_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(request_path)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }

    let mut candidate = root.to_path_buf();
    for segment in decoded.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains(':') {
            return None;
        }
        candidate.push(segment);
    }

    let canonical = candidate.canonicalize().ok()?;
    canonical.starts_with(root).then_some(canonical)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex(high)? * 16 + hex(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{:02X}", byte));
        }
    }
    encoded
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn resolves_paths_inside_root_only() {
        let directory = tempdir().expect("temporary directory");
        std::fs::create_dir(directory.path().join("sub")).expect("subdirectory");
        let root = directory.path().canonicalize().expect("canonical root");

        assert_eq!(
            resolve_path(&root, "/sub"),
            Some(
                root.join("sub")
                    .canonicalize()
                    .expect("canonical subdirectory")
            )
        );
        assert_eq!(resolve_path(&root, "/../"), None);
        assert_eq!(resolve_path(&root, "/%2e%2e/"), None);
        assert_eq!(resolve_path(&root, "/%5c..%5c"), None);
        assert_eq!(resolve_path(&root, "/missing"), None);
    }

    #[test]
    fn escapes_listing_names_and_links() {
        assert_eq!(escape_html("<a&b>"), "&lt;a&amp;b&gt;");
        assert_eq!(percent_encode("a b#c"), "a%20b%23c");
    }

    #[test]
    fn parses_root_bind_and_port() {
        let directory = tempdir().expect("temporary directory");
        let options = parse_args(&[
            directory.path().to_string_lossy().into_owned(),
            "--port".to_string(),
            "0".to_string(),
        ])
        .ok()
        .expect("parse");
        assert_eq!(options.bind, SocketAddr::from(([127, 0, 0, 1], 0)));

        assert!(
            parse_args(&[
                "--bind".to_string(),
                "localhost".to_string(),
                "--port".to_string(),
                "8080".to_string()
            ])
            .is_err()
        );
    }
}

use crate::builtins::net::{addr, runtime};
use crate::engine::{ExecutionResult, ShellState};
use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use std::fs::File;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

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

    axum::serve(listener, router(Arc::new(options.root)))
        .await
        .map_err(|error| format!("server failed: {}", error))
}

fn router(root: Arc<PathBuf>) -> Router {
    Router::new().fallback(get(serve_request)).with_state(root)
}

/// Resolve and open the target exactly once.
///
/// The request path is never re-resolved after this point: files are streamed
/// from the already-open handle, so replacing the path with a symlink while
/// the request is in flight cannot redirect the response outside ROOT.
async fn serve_request(
    State(root): State<Arc<PathBuf>>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let request_path = uri.path().to_string();
    let root = Arc::clone(&root);
    match tokio::task::spawn_blocking(move || open_request(&root, &request_path)).await {
        Ok(Ok(Served::File { file, path, len })) => {
            let content_type = content_type(&path);
            let stream = ReaderStream::new(tokio::fs::File::from_std(file));
            let mut response = (StatusCode::OK, Body::from_stream(stream)).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static(content_type),
            );
            if let Ok(length) = header::HeaderValue::from_str(&len.to_string()) {
                response
                    .headers_mut()
                    .insert(header::CONTENT_LENGTH, length);
            }
            response
        }
        Ok(Ok(Served::Listing(html))) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            Body::from(html),
        )
            .into_response(),
        Ok(Ok(Served::Redirect(location))) => Redirect::permanent(&location).into_response(),
        Ok(Err(status)) => status.into_response(),
        Err(error) => {
            eprintln!("net.http: request task failed: {}", error);
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

enum Served {
    File { file: File, path: PathBuf, len: u64 },
    Listing(String),
    Redirect(String),
}

fn open_request(root: &Path, request_path: &str) -> Result<Served, StatusCode> {
    let candidate = request_candidate(root, request_path).ok_or(StatusCode::NOT_FOUND)?;
    let file = open_verified(root, &candidate).map_err(status_for_error)?;
    let metadata = file.metadata().map_err(status_for_error)?;

    if !metadata.is_dir() {
        return Ok(Served::File {
            file,
            path: candidate,
            len: metadata.len(),
        });
    }

    if !request_path.ends_with('/') {
        return Ok(Served::Redirect(format!("{}/", request_path)));
    }

    // ServeDir used to append index.html to directory requests. Open it
    // through the same confinement check so a symlinked index cannot escape.
    let index = candidate.join("index.html");
    match std::fs::symlink_metadata(&index) {
        Ok(_) => {
            let index_file = match open_verified(root, &index) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // The index disappeared between the check and the open;
                    // fall back to a listing.
                    let html = directory_listing(&file, &candidate, request_path)
                        .map_err(status_for_error)?;
                    return Ok(Served::Listing(html));
                }
                Err(_) => {
                    // An index that resolves outside ROOT must not fall back
                    // to a listing, so the request fails as a whole.
                    return Err(StatusCode::NOT_FOUND);
                }
            };
            let index_metadata = index_file.metadata().map_err(status_for_error)?;
            if index_metadata.is_dir() {
                return Err(StatusCode::NOT_FOUND);
            }
            return Ok(Served::File {
                file: index_file,
                path: index,
                len: index_metadata.len(),
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(StatusCode::NOT_FOUND),
    }

    let html = directory_listing(&file, &candidate, request_path).map_err(status_for_error)?;
    Ok(Served::Listing(html))
}

fn status_for_error(error: io::Error) -> StatusCode {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Open a candidate path and verify the *opened handle* still resolves inside
/// ROOT. Checking the handle rather than the path removes the race between
/// validation and use.
fn open_verified(root: &Path, candidate: &Path) -> io::Result<File> {
    let file = open_candidate(candidate)?;
    let resolved = opened_path(&file, candidate).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cannot determine the opened file's path",
        )
    })?;
    if path_is_beneath(root, &resolved) {
        Ok(file)
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "path escapes the served root",
        ))
    }
}

#[cfg(unix)]
fn open_candidate(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(windows)]
fn open_candidate(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_BACKUP_SEMANTICS lets us open directories as well as files;
    // the directory or file is then pinned by the returned handle.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_candidate(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// Path of an open handle, used to verify confinement after opening.
fn opened_path(file: &File, candidate: &Path) -> Option<PathBuf> {
    platform_opened_path(file).or_else(|| candidate.canonicalize().ok())
}

#[cfg(target_os = "linux")]
fn platform_opened_path(file: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    let link = format!("/proc/self/fd/{}", file.as_raw_fd());
    let mut path = std::fs::read_link(link).ok()?;
    // A file that was unlinked after opening is still servable; drop the
    // kernel's marker before comparing it with ROOT.
    if let Some(stripped) = path.to_string_lossy().strip_suffix(" (deleted)") {
        path = PathBuf::from(stripped);
    }
    Some(path)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn platform_opened_path(file: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    let mut buffer = [0i8; nix::libc::PATH_MAX as usize];
    let result =
        unsafe { nix::libc::fcntl(file.as_raw_fd(), nix::libc::F_GETPATH, buffer.as_mut_ptr()) };
    if result == -1 {
        return None;
    }
    let bytes: Vec<u8> = buffer
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect();
    String::from_utf8(bytes).ok().map(PathBuf::from)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "macos", target_os = "ios"))
))]
fn platform_opened_path(file: &File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    std::fs::read_link(format!("/dev/fd/{}", file.as_raw_fd())).ok()
}

#[cfg(windows)]
fn platform_opened_path(file: &File) -> Option<PathBuf> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    let handle = file.as_raw_handle() as _;
    let needed = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    let written =
        unsafe { GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0) };
    if written == 0 || written as usize >= buffer.len() {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..written as usize]);
    let path = path.strip_prefix(r"\\?\").unwrap_or(&path);
    Some(PathBuf::from(path))
}

#[cfg(not(any(unix, windows)))]
fn platform_opened_path(_file: &File) -> Option<PathBuf> {
    None
}

/// True when `path` is `root` itself or lies beneath it.
#[cfg(windows)]
fn path_is_beneath(root: &Path, path: &Path) -> bool {
    let root = root.to_string_lossy().to_lowercase();
    let path = path.to_string_lossy().to_lowercase();
    if !path.starts_with(&root) {
        return false;
    }
    path.len() == root.len()
        || root.ends_with('\\')
        || path.as_bytes().get(root.len()) == Some(&b'\\')
}

#[cfg(not(windows))]
fn path_is_beneath(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
}

/// Build the path for a request, rejecting traversal and platform-sensitive
/// separators. The result is not canonicalized; confinement is enforced on
/// the opened handle instead.
fn request_candidate(root: &Path, request_path: &str) -> Option<PathBuf> {
    let mut candidate = root.to_path_buf();
    for segment in request_segments(request_path)? {
        candidate.push(segment);
    }
    Some(candidate)
}

fn request_segments(request_path: &str) -> Option<Vec<String>> {
    let decoded = percent_decode(request_path)?;
    if decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }

    let mut segments = Vec::new();
    for segment in decoded.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." || segment.contains(':') {
            return None;
        }
        segments.push(segment.to_string());
    }
    Some(segments)
}

/// List directory entries from an already-open directory handle.
fn directory_listing(file: &File, candidate: &Path, request_path: &str) -> io::Result<String> {
    let read_path = directory_read_path(file, candidate);
    let entries = std::fs::read_dir(read_path)?;

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
    if request_path != "/" {
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
    Ok(html)
}

/// A path that refers to the already-open directory itself, when the platform
/// offers one. Falls back to the validated candidate path.
#[cfg(target_os = "linux")]
fn directory_read_path(file: &File, _candidate: &Path) -> PathBuf {
    use std::os::fd::AsRawFd;
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn directory_read_path(file: &File, candidate: &Path) -> PathBuf {
    use std::os::fd::AsRawFd;
    let path = PathBuf::from(format!("/dev/fd/{}", file.as_raw_fd()));
    if path.exists() {
        path
    } else {
        candidate.to_path_buf()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
fn directory_read_path(_file: &File, candidate: &Path) -> PathBuf {
    candidate.to_path_buf()
}

fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "wasm" => "application/wasm",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
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
    use std::io::Read;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    #[cfg(test)]
    fn resolve_path(root: &Path, request_path: &str) -> Option<PathBuf> {
        let candidate = request_candidate(root, request_path)?;
        let canonical = candidate.canonicalize().ok()?;
        canonical.starts_with(root).then_some(canonical)
    }

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

    #[cfg(unix)]
    #[test]
    fn opened_handle_stays_inside_root_after_a_path_swap() {
        let directory = tempdir().expect("temporary directory");
        let outside = tempdir().expect("outside directory");
        let root = directory.path().canonicalize().expect("canonical root");
        let safe = root.join("safe.txt");
        let secret = outside.path().join("secret.txt");
        std::fs::write(&safe, b"safe").expect("safe file");
        std::fs::write(&secret, b"secret").expect("secret file");

        let mut file = open_verified(&root, &safe).expect("open safe file");
        std::fs::remove_file(&safe).expect("remove safe file");
        std::os::unix::fs::symlink(&secret, &safe).expect("swap with symlink");

        let mut contents = String::new();
        file.read_to_string(&mut contents).expect("read handle");
        assert_eq!(contents, "safe");
        assert!(open_verified(&root, &safe).is_err());
    }

    #[tokio::test]
    async fn directory_listing_returns_ok_and_rejects_traversal() {
        let directory = tempdir().expect("temporary directory");
        std::fs::create_dir(directory.path().join("sub")).expect("subdirectory");
        std::fs::write(directory.path().join("sub/file.txt"), b"file").expect("file");
        let root = Arc::new(directory.path().canonicalize().expect("canonical root"));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server_root = Arc::clone(&root);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(server_root)).await;
        });

        let listing = request(address, "/sub/").await;
        assert!(listing.starts_with("HTTP/1.1 200"), "{listing}");
        assert!(listing.contains("file.txt"), "{listing}");

        let traversal = request(address, "/%2e%2e/").await;
        assert!(traversal.starts_with("HTTP/1.1 404"), "{traversal}");

        #[cfg(unix)]
        {
            let outside = tempdir().expect("outside directory");
            std::fs::write(outside.path().join("secret.txt"), b"secret").expect("secret file");
            std::os::unix::fs::symlink(outside.path(), root.join("escape"))
                .expect("symlink outside root");
            let escaped = request(address, "/escape/secret.txt").await;
            assert!(escaped.starts_with("HTTP/1.1 404"), "{escaped}");
        }

        server.abort();
    }

    #[tokio::test]
    async fn serves_directory_index_inside_root() {
        let directory = tempdir().expect("temporary directory");
        std::fs::write(directory.path().join("index.html"), b"inside root").expect("index");
        let root = Arc::new(directory.path().canonicalize().expect("canonical root"));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server_root = Arc::clone(&root);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(server_root)).await;
        });

        let response = request(address, "/").await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("inside root"), "{response}");

        server.abort();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_directory_index_symlinked_outside_root() {
        let directory = tempdir().expect("temporary directory");
        let outside = tempdir().expect("outside directory");
        std::fs::write(outside.path().join("index.html"), b"secret").expect("outside index");
        let root = Arc::new(directory.path().canonicalize().expect("canonical root"));
        std::os::unix::fs::symlink(outside.path().join("index.html"), root.join("index.html"))
            .expect("symlink outside root");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("address");
        let server_root = Arc::clone(&root);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(server_root)).await;
        });

        let response = request(address, "/").await;
        assert!(response.starts_with("HTTP/1.1 404"), "{response}");
        assert!(!response.contains("secret"), "{response}");

        server.abort();
    }

    async fn request(address: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(address).await.expect("connect");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.expect("write");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.expect("read");
        String::from_utf8_lossy(&response).into_owned()
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

//! The local page server (spec 0007, Pinned pages).
//!
//! Serves `perf/pages/` over HTTP/1.1 from a small `std::net` server (no
//! new dependency), so the network path is part of what the runtime
//! metrics measure. It binds `127.0.0.1` only, serves only that directory,
//! and rejects paths with `..` (spec 0007, Security model).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;

/// The largest request head the server reads, so a broken client cannot
/// make it buffer forever.
const MAX_HEAD: usize = 8 * 1024;

/// A running page server. It lives until the gate process ends; dropping
/// it detaches the accept thread, which is fine for a short lived gate.
pub(crate) struct PageServer {
    /// The base URL every page hangs off, `http://127.0.0.1:<port>`.
    pub(crate) base_url: String,
}

impl PageServer {
    /// Starts the server for `root` (the `perf/pages` directory) on an OS
    /// chosen port (spec 0007, Failure handling: port in use).
    pub(crate) fn start(root: &Path) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let root = root.to_path_buf();
        thread::Builder::new()
            .name("perf-pages".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let root = root.clone();
                            // One thread per connection: a page loads its
                            // subresources in parallel.
                            let _ = thread::Builder::new()
                                .name("perf-page-conn".into())
                                .spawn(move || serve(stream, &root));
                        }
                        Err(_) => return,
                    }
                }
            })?;
        Ok(Self {
            base_url: format!("http://127.0.0.1:{port}"),
        })
    }
}

/// Serves one connection: one GET, one file, then close.
fn serve(mut stream: TcpStream, root: &Path) {
    let Ok(path) = request_path(&mut stream) else {
        return;
    };
    let Some(path) = safe_path(&path) else {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let mut full = root.join(path);
    // A path ending in `/` names a directory: serve its index (the gate's
    // URLs all end in one, spec 0007, Scenarios).
    if full.is_dir() {
        full = full.join("index.html");
    }
    let Ok(body) = std::fs::read(&full) else {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let kind = content_type(&full);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// Reads the request head and returns the path from its `GET` line.
fn request_path(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < MAX_HEAD && !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte)? == 1 {
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    let line = head.lines().next().unwrap_or_default();
    // "GET /article/ HTTP/1.1"
    let mut parts = line.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("GET"), Some(path)) => Ok(path.to_owned()),
        _ => Ok(String::new()),
    }
}

/// Maps a request path to a file path under the root, rejecting anything
/// that tries to climb out (spec 0007, Security model).
fn safe_path(path: &str) -> Option<PathBuf> {
    let path = path.split('?').next().unwrap_or(path);
    let path = path.strip_prefix('/')?;
    if path.is_empty() {
        return None;
    }
    let mut out = PathBuf::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." | "\0" => return None,
            name => out.push(name),
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The Content-Type for a file, by extension.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_paths_stay_under_the_root() {
        assert_eq!(
            safe_path("/article/index.html"),
            Some(PathBuf::from("article/index.html"))
        );
        assert_eq!(safe_path("/article/"), Some(PathBuf::from("article")));
        assert_eq!(safe_path("/a//b/./c.css"), Some(PathBuf::from("a/b/c.css")));
    }

    #[test]
    fn climbing_paths_are_rejected() {
        assert_eq!(safe_path("/../etc/passwd"), None);
        assert_eq!(safe_path("/article/../../etc/passwd"), None);
        assert_eq!(
            safe_path("/..%2fpasswd"),
            Some(PathBuf::from("..%2fpasswd"))
        );
        assert_eq!(safe_path("/"), None);
        assert_eq!(safe_path(""), None);
    }

    #[test]
    fn query_strings_are_dropped() {
        assert_eq!(
            safe_path("/script/index.html?v=1"),
            Some(PathBuf::from("script/index.html"))
        );
    }

    #[test]
    fn content_types_cover_the_pages() {
        assert_eq!(
            content_type(Path::new("a/index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("a/kernels.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("a/img.png")), "image/png");
    }

    #[test]
    fn the_server_serves_a_file_and_404s_a_missing_one() {
        let dir = std::env::temp_dir().join("perf-pages-test");
        std::fs::create_dir_all(dir.join("hello")).unwrap();
        std::fs::write(dir.join("hello/index.html"), b"<p>hi</p>").unwrap();
        let server = PageServer::start(&dir).unwrap();

        let mut stream = TcpStream::connect(server.base_url.trim_start_matches("http://")).unwrap();
        stream.write_all(b"GET /hello/ HTTP/1.1\r\n\r\n").unwrap();
        let mut body = Vec::new();
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let _ = stream.read_to_end(&mut body);
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("HTTP/1.1 200 OK"));
        assert!(text.contains("<p>hi</p>"));

        let mut stream = TcpStream::connect(server.base_url.trim_start_matches("http://")).unwrap();
        stream
            .write_all(b"GET /nope.html HTTP/1.1\r\n\r\n")
            .unwrap();
        let mut body = Vec::new();
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
        let _ = stream.read_to_end(&mut body);
        assert!(String::from_utf8_lossy(&body).starts_with("HTTP/1.1 404"));
    }
}

//! The plain HTTP side of the server's port: the web app's files (so a link
//! `http://host/s/<session>` opens CADCraft in the browser, joined to that session) and the
//! session's drawing as DWG or DXF (`/s/<session>.dwg`, handles renumbered to fit DWG).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::{Rooms, session_name};

/// Most bytes in a request head.
const MAX_HEAD: usize = 16 * 1024;
/// How long a client may take to send its request head.
const HEAD_WAIT: Duration = Duration::from_secs(10);

/// Read up to the end of the request head (the blank line). Bytes after it stay in the result.
pub(crate) fn read_head(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    stream.set_read_timeout(Some(HEAD_WAIT))?;
    let mut head = Vec::new();
    let mut buf = [0u8; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() > MAX_HEAD {
            return Err(std::io::Error::other("the request head is too large"));
        }
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Err(std::io::Error::other("the connection closed before a request arrived"));
        }
        head.extend_from_slice(buf.get(..n).unwrap_or_default());
    }
    Ok(head)
}

/// Whether the request asks to become a WebSocket.
pub(crate) fn is_websocket(head: &[u8]) -> bool {
    String::from_utf8_lossy(head).lines().any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("upgrade:") && l.contains("websocket")
    })
}

/// What a page request gets.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// The web app's page.
    App,
    /// One of the web app's files.
    File(String),
    /// Session `name`'s drawing as DWG (`false`) or DXF (`true`).
    Export {
        name: String,
        dxf: bool,
    },
    NotFound,
}

/// Route a request path (query string removed).
pub fn route(path: &str) -> Route {
    let path = path.split('?').next().unwrap_or_default();
    if let Some((stem, ext)) = path.rsplit_once('.')
        && let Some(name) = session_name(stem)
        && matches!(ext, "dwg" | "dxf")
    {
        return Route::Export { name, dxf: ext == "dxf" };
    }
    if path == "/" || session_name(path.trim_end_matches('/')).is_some() {
        return Route::App;
    }
    // trunk's page asks for its files next to itself, so `/s/app-1a2b.js` is `app-1a2b.js`. No
    // segment may be empty or start with a dot, so nothing outside the web folder is reachable.
    let file = path.strip_prefix("/s/").or_else(|| path.strip_prefix('/')).unwrap_or_default();
    let segment_ok =
        |seg: &str| !seg.is_empty() && !seg.starts_with('.') && seg.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    let ok = file.split('/').all(segment_ok);
    if ok { Route::File(file.to_string()) } else { Route::NotFound }
}

fn content_type(file: &str) -> &'static str {
    match file.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

pub(crate) fn respond(stream: &mut TcpStream, head: &[u8], rooms: &Rooms) -> std::io::Result<()> {
    let text = String::from_utf8_lossy(head);
    let mut first = text.lines().next().unwrap_or_default().split(' ');
    let (method, path) = (first.next().unwrap_or_default(), first.next().unwrap_or_default());
    if method != "GET" {
        return send(stream, "405 Method Not Allowed", "text/plain", &[], b"GET only\n");
    }
    match route(path) {
        Route::Export { name, dxf } => {
            let file = format!("{name}.{}", if dxf { "dxf" } else { "dwg" });
            let drawing = match rooms.drawing(&name) {
                Ok(Some(d)) => d,
                Ok(None) => return send(stream, "404 Not Found", "text/plain", &[], b"no such session\n"),
                Err(e) => return send(stream, "500 Internal Server Error", "text/plain", &[], format!("{e}\n").as_bytes()),
            };
            match cadcraft_io::write_shared(&drawing, &file, &cadcraft_io::SaveOptions::default()) {
                Ok(bytes) => {
                    let disposition = format!("Content-Disposition: attachment; filename=\"{file}\"");
                    send(stream, "200 OK", "application/octet-stream", &[&disposition], &bytes)
                }
                Err(e) => send(stream, "500 Internal Server Error", "text/plain", &[], format!("can't write {file}: {e}\n").as_bytes()),
            }
        }
        route => {
            let file = match route {
                Route::App => "index.html".to_string(),
                Route::File(f) => f,
                _ => String::new(),
            };
            let found = rooms.config.web.as_ref().filter(|_| !file.is_empty()).map(|dir| dir.join(&file)).and_then(|p| std::fs::read(p).ok());
            match found {
                Some(bytes) => send(stream, "200 OK", content_type(&file), &["Cache-Control: no-cache"], &bytes),
                None => send(stream, "404 Not Found", "text/plain", &[], b"not found\n"),
            }
        }
    }
}

fn send(stream: &mut TcpStream, status: &str, kind: &str, extra: &[&str], body: &[u8]) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for h in extra {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::{Route, route};

    #[test]
    fn requests_are_routed() {
        assert_eq!(route("/"), Route::App);
        assert_eq!(route("/s/plan"), Route::App);
        assert_eq!(route("/s/plan/?name=Bo"), Route::App);
        assert_eq!(route("/s/plan.dwg"), Route::Export { name: "plan".into(), dxf: false });
        assert_eq!(route("/s/plan.dxf"), Route::Export { name: "plan".into(), dxf: true });
        assert_eq!(route("/s/cadcraft-web-1a2b_bg.wasm"), Route::File("cadcraft-web-1a2b_bg.wasm".into()));
        assert_eq!(route("/index.html"), Route::File("index.html".into()));
        assert_eq!(route("/s/snippets/eframe-1a/inline0.js"), Route::File("snippets/eframe-1a/inline0.js".into()));
        for bad in ["/s/../x", "/s/snippets/../../x", "/.env", "/a b", "/s//x"] {
            assert_eq!(route(bad), Route::NotFound, "{bad}");
        }
    }
}

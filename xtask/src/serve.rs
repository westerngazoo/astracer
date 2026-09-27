//! A static file server, in the standard library.
//!
//! The wasm bundle cannot be loaded from `file://` — module instantiation is a
//! fetch, and fetch refuses that scheme — so browser dev mode needs a server.
//! The shell script used `python3 -m http.server`, which is `python` or `py` on
//! Windows and absent from plenty of machines; a hundred lines of `std` removes
//! that dependency without adding a crate.
//!
//! Content types are the part that has to be right rather than merely present:
//! `WebAssembly.instantiateStreaming` rejects a response whose type is not
//! `application/wasm` outright, so a server that answers
//! `application/octet-stream` for everything produces a blank page and a
//! console error, not a fallback.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

pub fn listen(root: &Path, port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    serve(listener, root.to_path_buf());
    Ok(())
}

fn serve(listener: TcpListener, root: PathBuf) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let root = root.clone();
        // A thread per connection: the browser opens several at once for the
        // page, the wasm and the fixture, and a serial loop would deadlock
        // against its own connection reuse.
        std::thread::spawn(move || {
            let _ = handle(stream, &root);
        });
    }
}

fn handle(stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let result = answer(&stream, root);
    close_without_reset(&stream);
    result
}

/// Read one request and write its response. Reads and writes go through the
/// one handle: `&TcpStream` is both `Read` and `Write`, so there is no need for
/// `try_clone`, whose duplicated descriptor makes the close depend on the order
/// two handles are dropped in — on Windows, a `WSADuplicateSocket` pair.
fn answer(stream: &TcpStream, root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    // Drain the headers; nothing here needs them, but leaving them unread can
    // surface as a reset on the client side.
    let mut header = String::new();
    while reader.read_line(&mut header)? > 0 && header.trim() != "" {
        header.clear();
    }

    let target = request_target(&line);
    match target.and_then(|t| safe_join(root, t)) {
        Some(path) => match std::fs::File::open(&path) {
            Ok(mut f) => {
                let mut body = Vec::new();
                f.read_to_end(&mut body)?;
                respond(stream, "200 OK", mime_for(&path), &body)
            }
            Err(_) => respond(stream, "404 Not Found", "text/plain", b"not found\n"),
        },
        None => respond(stream, "400 Bad Request", "text/plain", b"bad request\n"),
    }
}

/// Close so the client receives every byte of the response.
///
/// Closing a socket that still has unread input makes the stack send a reset
/// rather than a normal shutdown, and a reset entitles the *peer's* stack to
/// discard response data it has not yet handed to the application. A browser
/// fetching a multi-megabyte wasm module can then get a truncated one — or,
/// as the Windows CI runner showed, `ConnectionReset` mid-read. Input can be
/// left unread whenever the client sends anything after the headers we
/// consumed: a pipelined request, a body, a keep-alive probe.
///
/// So: announce end-of-response with a half-close, then read and discard
/// whatever the client still sends until it closes its side, so nothing is
/// unread when this socket goes. Bounded in time and size, so a client that
/// never closes costs one short-lived thread, not a hung one.
fn close_without_reset(mut stream: &TcpStream) {
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut sink = [0u8; 8192];
    let mut budget: usize = 1 << 20;
    while budget > 0 {
        match stream.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => budget = budget.saturating_sub(n),
        }
    }
}

fn respond(mut stream: &TcpStream, status: &str, mime: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         Content-Type: {mime}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

/// The path out of a request line, without its query string. `None` for
/// anything that is not a `GET`.
fn request_target(line: &str) -> Option<&str> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    Some(target.split(['?', '#']).next().unwrap_or(target))
}

/// Resolve a request target under `root`, or `None` if it tries to leave.
///
/// Every component must be `Normal`: that rejects `..` and absolute paths, and
/// because it inspects parsed components rather than the raw string it is not
/// fooled by a separator the host platform accepts but the check did not think
/// to look for — on Windows `\` is one.
fn safe_join(root: &Path, target: &str) -> Option<PathBuf> {
    let rel = target.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    let candidate = Path::new(rel);
    if !candidate
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(candidate))
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        // Not negotiable: instantiateStreaming rejects anything else.
        Some("wasm") => "application/wasm",
        Some("json") => "application/json",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wasm_gets_the_type_the_browser_insists_on() {
        assert_eq!(mime_for(Path::new("a/b.wasm")), "application/wasm");
        assert_eq!(mime_for(Path::new("fixture.json")), "application/json");
        assert!(mime_for(Path::new("index.html")).starts_with("text/html"));
        assert_eq!(mime_for(Path::new("noext")), "application/octet-stream");
    }

    #[test]
    fn bare_root_is_the_index() {
        let root = Path::new("/srv");
        assert_eq!(safe_join(root, "/").unwrap(), root.join("index.html"));
        assert_eq!(safe_join(root, "").unwrap(), root.join("index.html"));
    }

    #[test]
    fn escapes_are_refused() {
        let root = Path::new("/srv");
        assert!(safe_join(root, "/../etc/passwd").is_none());
        assert!(safe_join(root, "/a/../../b").is_none());
        // Nested paths within the root are fine.
        assert_eq!(
            safe_join(root, "/a/b.wasm").unwrap(),
            root.join("a").join("b.wasm")
        );
        // Leading slashes collapse to a path *under* the root rather than an
        // absolute one, which is what every static server does: the request
        // never escapes, it just names a file that is probably not there.
        assert_eq!(
            safe_join(root, "//etc/passwd").unwrap(),
            root.join("etc").join("passwd")
        );
    }

    #[test]
    fn only_get_is_served_and_queries_are_stripped() {
        assert_eq!(
            request_target("GET /fixture.json?v=2 HTTP/1.1"),
            Some("/fixture.json")
        );
        assert_eq!(request_target("GET /a#frag HTTP/1.1"), Some("/a"));
        assert_eq!(request_target("POST /a HTTP/1.1"), None);
        assert_eq!(request_target(""), None);
    }

    #[test]
    fn serves_a_real_file_over_a_real_socket() {
        let dir = std::env::temp_dir().join(format!("lcw-serve-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), b"<h1>hi</h1>").unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = dir.clone();
        std::thread::spawn(move || serve(listener, root));

        let got = get(port, "/");
        assert!(got.contains("200 OK"), "{got}");
        assert!(got.contains("text/html"), "{got}");
        assert!(got.ends_with("<h1>hi</h1>"), "{got}");

        let missing = get(port, "/nope.wasm");
        assert!(missing.contains("404"), "{missing}");

        let escape = get(port, "/../../etc/passwd");
        assert!(escape.contains("400"), "{escape}");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The response must arrive whole even when the client has sent bytes the
    /// server never reads — here a pipelined second request. A socket closed
    /// with unread input is reset rather than shut down, and a reset lets the
    /// peer's stack drop response data it has not yet delivered: with a
    /// wasm-sized body still in flight, that is a truncated module.
    #[test]
    fn unread_input_does_not_cost_the_client_its_response() {
        let dir = std::env::temp_dir().join(format!("lcw-serve-rst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = vec![b'w'; 4 << 20];
        std::fs::write(dir.join("big.wasm"), &body).unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let root = dir.clone();
        std::thread::spawn(move || serve(listener, root));

        for _ in 0..20 {
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            s.write_all(b"GET /big.wasm HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            // Wait until the server is mid-response, so it is no longer
            // reading: bytes sent now sit unread in its kernel queue rather
            // than in a userspace buffer, which is the case that matters.
            let mut got = vec![0u8; 1];
            s.read_exact(&mut got).unwrap();
            s.write_all(b"GET /big.wasm HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            // Let the server finish and close before we read the rest, which
            // is when a reset does its damage.
            std::thread::sleep(std::time::Duration::from_millis(50));
            s.read_to_end(&mut got).expect("response, not a reset");
            assert!(
                got.ends_with(&body[body.len() - 64..]),
                "truncated: {} bytes",
                got.len()
            );
            assert!(got.len() > body.len(), "truncated: {} bytes", got.len());
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
        s.flush().unwrap();
        let mut buf = String::new();
        s.read_to_string(&mut buf).unwrap();
        buf
    }
}

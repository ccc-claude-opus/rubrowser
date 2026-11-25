//! The remote debugging server (`--remote-debugging-port`): HTTP for
//! Chrome's `/json` discovery endpoints and WebSocket (tungstenite) for the
//! Chrome DevTools Protocol, on 127.0.0.1 only.
//!
//! Connections run on their own threads; what they receive goes to the
//! browser's thread as [`Incoming`] (see `browser/cdp.rs` for the protocol)
//! and the browser's replies come back through [`Server::send`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tungstenite::Message;

/// Something for the browser to handle.
pub enum Incoming {
    /// An HTTP request for `path`; answer with (status, JSON body).
    Http { path: String, reply: Sender<(u16, String)> },
    /// A WebSocket connection to `path`; answer whether it has a target.
    Open { conn: u32, path: String, ok: Sender<bool> },
    Message { conn: u32, text: String },
    Closed { conn: u32 },
}

/// The running server: sends to its open connections.
#[derive(Clone)]
pub struct Server {
    pub port: u16,
    conns: Arc<Mutex<HashMap<u32, Sender<String>>>>,
}

impl Server {
    /// Sends `text` to connection `conn` (if it is still open).
    pub fn send(&self, conn: u32, text: String) {
        if let Some(tx) = self.conns.lock().unwrap_or_else(|e| e.into_inner()).get(&conn) {
            let _ = tx.send(text);
        }
    }
}

/// Starts listening on 127.0.0.1:`port` (0: any free port); `deliver`
/// hands each [`Incoming`] to the browser's thread.
pub fn start(
    port: u16,
    deliver: impl Fn(Incoming) + Send + Sync + 'static,
) -> std::io::Result<Server> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let server = Server {
        port: listener.local_addr()?.port(),
        conns: Default::default(),
    };
    let deliver = Arc::new(deliver);
    let s = server.clone();
    std::thread::spawn(move || {
        let mut next = 0u32;
        for stream in listener.incoming().flatten() {
            next += 1;
            let (conn, s, deliver) = (next, s.clone(), deliver.clone());
            std::thread::spawn(move || handle(stream, conn, &s, &*deliver));
        }
    });
    Ok(server)
}

/// The request line and headers of an HTTP request not read yet (peeked,
/// so the WebSocket handshake can read it again); `None` if malformed.
fn peek_head(stream: &TcpStream) -> Option<(String, String, usize)> {
    let mut buf = vec![0u8; 16 * 1024];
    for _ in 0..200 {
        let n = stream.peek(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        if let Some(end) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let (line, headers) = head.split_once("\r\n").unwrap_or((&head, ""));
            return Some((line.to_string(), headers.to_string(), end + 4));
        }
        if n == buf.len() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

fn header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Whether a `Host` header names this machine (as Chrome checks, so web
/// pages cannot reach the port through DNS rebinding).
fn local_host(host: Option<&str>) -> bool {
    let Some(h) = host else { return true };
    let name = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':').map_or(h, |(n, _)| n)
    };
    matches!(name.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let ctype = if body.starts_with('{') || body.starts_with('[') {
        "application/json; charset=UTF-8"
    } else {
        "text/plain; charset=UTF-8"
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

fn handle(mut stream: TcpStream, conn: u32, server: &Server, deliver: &dyn Fn(Incoming)) {
    let Some((line, headers, len)) = peek_head(&stream) else {
        return;
    };
    let mut parts = line.split_whitespace();
    let (_method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/").to_string());
    if !local_host(header(&headers, "Host")) {
        let mut head = vec![0u8; len];
        let _ = stream.read_exact(&mut head);
        return respond(&mut stream, 403, "Host header is specified and is not an IP address or localhost.");
    }
    let upgrade = header(&headers, "Upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    if !upgrade {
        let mut head = vec![0u8; len];
        let _ = stream.read_exact(&mut head);
        let (tx, rx) = mpsc::channel();
        deliver(Incoming::Http { path, reply: tx });
        let (status, body) = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or((500, "the browser did not answer".into()));
        return respond(&mut stream, status, &body);
    }
    let (tx, rx) = mpsc::channel();
    deliver(Incoming::Open { conn, path, ok: tx });
    if rx.recv_timeout(Duration::from_secs(10)) != Ok(true) {
        let mut head = vec![0u8; len];
        let _ = stream.read_exact(&mut head);
        return respond(&mut stream, 404, "No such target id");
    }
    let Ok(mut ws) = tungstenite::accept(stream) else {
        return deliver(Incoming::Closed { conn });
    };
    let (out_tx, out_rx): (Sender<String>, Receiver<String>) = mpsc::channel();
    server
        .conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(conn, out_tx);
    // Reads with a short timeout, then writes what the browser queued.
    let _ = ws.get_ref().set_read_timeout(Some(Duration::from_millis(10)));
    'run: loop {
        match ws.read() {
            Ok(Message::Text(t)) => deliver(Incoming::Message { conn, text: t.to_string() }),
            Ok(Message::Binary(b)) => deliver(Incoming::Message {
                conn,
                text: String::from_utf8_lossy(&b).into_owned(),
            }),
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
        while let Ok(text) = out_rx.try_recv() {
            if ws.send(Message::text(text)).is_err() {
                break 'run;
            }
        }
    }
    server.conns.lock().unwrap_or_else(|e| e.into_inner()).remove(&conn);
    deliver(Incoming::Closed { conn });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_host_headers() {
        assert!(local_host(Some("127.0.0.1:9222")));
        assert!(local_host(Some("localhost:9222")));
        assert!(local_host(Some("[::1]:9222")));
        assert!(local_host(None));
        assert!(!local_host(Some("evil.example:9222")));
        assert!(!local_host(Some("127.0.0.1.evil.example")));
    }
}

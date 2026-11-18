//! WebSocket connections of the native platform (RFC 6455, with
//! tungstenite): one thread per connection, which reads frames and sends
//! what the page asked for.

use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

use tungstenite::client::IntoClientRequest;
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error, Message};

use crate::input::WsEvent;

/// What the page asks of an open connection.
pub enum Command {
    /// A message (`true`: a text frame).
    Send(Vec<u8>, bool),
    /// Start the closing handshake (code 1005: no status).
    Close(u16, String),
}

/// Opens a connection to `url` on its own thread; `emit` reports what
/// happens. Dropping the returned sender closes the connection.
pub fn open(
    url: &str,
    protocols: &[String],
    origin: &str,
    user_agent: &str,
    emit: impl Fn(WsEvent) + Send + 'static,
) -> mpsc::Sender<Command> {
    let (tx, rx) = mpsc::channel();
    let (url, protocols) = (url.to_string(), protocols.to_vec());
    let (origin, user_agent) = (origin.to_string(), user_agent.to_string());
    std::thread::spawn(move || {
        let (code, reason, clean) = run(&url, &protocols, &origin, &user_agent, &rx, &emit);
        emit(WsEvent::Close { code, reason, clean });
    });
    tx
}

/// Runs a connection; returns how it closed.
fn run(
    url: &str,
    protocols: &[String],
    origin: &str,
    user_agent: &str,
    rx: &mpsc::Receiver<Command>,
    emit: &impl Fn(WsEvent),
) -> (u16, String, bool) {
    let failed = (1006, String::new(), false);
    let Ok(mut request) = url.into_client_request() else {
        return failed;
    };
    let headers = request.headers_mut();
    if let Ok(v) = origin.parse() {
        headers.insert("Origin", v);
    }
    if let Ok(v) = user_agent.parse() {
        headers.insert("User-Agent", v);
    }
    if !protocols.is_empty()
        && let Ok(v) = protocols.join(", ").parse()
    {
        headers.insert("Sec-WebSocket-Protocol", v);
    }
    let Ok((mut socket, response)) = tungstenite::connect(request) else {
        return failed;
    };
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    // The server must pick one of the offered subprotocols, or none.
    if !protocol.is_empty() && !protocols.contains(&protocol) {
        let _ = socket.close(None);
        return failed;
    }
    // Short read timeouts let the thread also send what the page queued.
    let timeout = Some(Duration::from_millis(20));
    let _ = match socket.get_mut() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(s) => s.get_mut().set_read_timeout(timeout),
        _ => Ok(()),
    };
    emit(WsEvent::Open(protocol));
    let mut status = (1005, String::new());
    let mut closing = false;
    loop {
        loop {
            match rx.try_recv() {
                Ok(Command::Send(data, text)) if !closing => {
                    let m = if text {
                        Message::text(String::from_utf8_lossy(&data).into_owned())
                    } else {
                        Message::binary(data)
                    };
                    if socket.send(m).is_err() {
                        return failed;
                    }
                }
                Ok(Command::Send(..)) => {}
                Ok(Command::Close(code, reason)) => {
                    if !closing {
                        closing = true;
                        let frame = (code != 1005).then(|| CloseFrame {
                            code: CloseCode::from(code),
                            reason: reason.into(),
                        });
                        let _ = socket.close(frame);
                    }
                }
                Err(TryRecvError::Empty) => break,
                // The page is gone.
                Err(TryRecvError::Disconnected) => {
                    if !closing {
                        closing = true;
                        let _ = socket.close(Some(CloseFrame {
                            code: CloseCode::Away,
                            reason: "".into(),
                        }));
                    }
                    break;
                }
            }
        }
        match socket.read() {
            Ok(Message::Text(t)) => emit(WsEvent::Text(t.to_string())),
            Ok(Message::Binary(b)) => emit(WsEvent::Binary(b.to_vec())),
            // tungstenite answers the closing handshake; reading goes on
            // until the connection is closed.
            Ok(Message::Close(frame)) => {
                if let Some(f) = frame {
                    status = (u16::from(f.code), f.reason.to_string());
                }
            }
            Ok(_) => {}
            Err(Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                let _ = socket.flush();
            }
            Err(Error::ConnectionClosed) => return (status.0, status.1, true),
            Err(_) => return failed,
        }
    }
}

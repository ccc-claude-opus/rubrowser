//! Pausing in the JavaScript debugger (CDP `Debugger`).
//!
//! When a page's script stops at a breakpoint, the browser's main thread
//! stays inside the script call (the language runtime runs a nested
//! message loop, as Chrome does). Protocol messages for that page then
//! come here straight from the debugging server's thread (`divert`)
//! instead of through the event loop, and the runtime takes them with the
//! `debug_wait` import until it resumes. Its replies and events go out
//! through `debug_send`.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock};

/// A debugging connection (and session) attached to a page's runtime.
pub struct Link {
    pub conn: u32,
    pub session: Option<String>,
    state: Mutex<State>,
    cv: Condvar,
}

#[derive(Default)]
struct State {
    /// The page's script is paused and waiting for commands.
    paused: bool,
    mailbox: VecDeque<String>,
    /// Messages to send when no server sends them directly (tests).
    sent: Vec<String>,
    /// Commands that arrived after the runtime resumed: handled by the
    /// browser as usual.
    leftovers: Vec<String>,
}

static LINKS: Mutex<Vec<Arc<Link>>> = Mutex::new(Vec::new());
type Sender = Box<dyn Fn(u32, String) + Send + Sync>;
static SENDER: OnceLock<Sender> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How messages reach the debugging connections (the server sets it).
/// Without it nothing blocks: a paused runtime gets no command and
/// resumes.
pub fn set_sender(f: impl Fn(u32, String) + Send + Sync + 'static) {
    let _ = SENDER.set(Box::new(f));
}

/// Attaches connection `conn` (`session`) to a page's debugger.
pub fn attach(conn: u32, session: Option<String>) -> Arc<Link> {
    let link = Arc::new(Link { conn, session: session.clone(), state: Mutex::default(), cv: Condvar::new() });
    let mut links = lock(&LINKS);
    links.retain(|l| !(l.conn == conn && l.session == session));
    links.push(link.clone());
    link
}

pub fn detach(conn: u32) {
    lock(&LINKS).retain(|l| l.conn != conn);
}

/// On the server's thread: a message for a connection whose page is
/// paused goes to the paused runtime. Returns whether it was taken.
pub fn divert(conn: u32, text: &str) -> bool {
    let links = lock(&LINKS);
    for l in links.iter().filter(|l| l.conn == conn) {
        let mut s = lock(&l.state);
        if s.paused {
            s.mailbox.push_back(text.to_string());
            l.cv.notify_all();
            return true;
        }
    }
    false
}

/// Links with messages that the browser must still handle or send.
pub fn links() -> Vec<Arc<Link>> {
    lock(&LINKS).clone()
}

impl Link {
    /// Sends a reply or event (JSON) to the connection, in its session.
    pub fn send(&self, text: &str) {
        let text = match (&self.session, serde_json::from_str::<serde_json::Value>(text)) {
            (Some(s), Ok(mut v)) => {
                v["sessionId"] = serde_json::json!(s);
                v.to_string()
            }
            _ => text.to_string(),
        };
        match SENDER.get() {
            Some(f) => f(self.conn, text),
            None => lock(&self.state).sent.push(text),
        }
    }

    /// The next command while paused (None: resume, nothing can come).
    pub fn wait(&self) -> Option<String> {
        let mut s = lock(&self.state);
        s.paused = true;
        loop {
            if let Some(m) = s.mailbox.pop_front() {
                return Some(m);
            }
            SENDER.get()?;
            s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The runtime resumed: later messages take the usual way.
    pub fn resumed(&self) {
        let mut s = lock(&self.state);
        s.paused = false;
        let rest: Vec<String> = s.mailbox.drain(..).collect();
        s.leftovers.extend(rest);
    }

    /// Queues a command for the next pause (tests).
    pub fn push(&self, text: &str) {
        lock(&self.state).mailbox.push_back(text.to_string());
    }

    pub fn take_sent(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.state).sent)
    }

    pub fn take_leftovers(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.state).leftovers)
    }
}

//! The Chrome DevTools Protocol (CDP): a subset, so tools that drive or
//! inspect Chrome (Puppeteer-style scripts, `chrome-remote-interface`,
//! test harnesses) can work with rubrowser.
//!
//! The platform serves it over WebSocket (`--remote-debugging-port`, see
//! `platform/cdp_server.rs`) and hands this module each connection's
//! messages; replies and events queue up for [`Browser::take_cdp_output`].
//!
//! Domains: Browser, Target (with flattened sessions), Page, Runtime, DOM,
//! CSS, Network, Input, Overlay, Log; enabling and disabling other domains
//! is accepted. Remote objects are returned by value (there are no object
//! ids). See `doc/devtools.md`.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use super::devtools::Level;
use super::{Browser, JS_MIME, Request, js_string};
use crate::dom::{Dom, NodeData, NodeId};
use crate::input::{Event, Host, Modifiers};

/// Message marker of a result from `__rb_cdp_eval` (prelude.js).
pub(super) const CDP_RESULT: &str = "\u{1}devtools-cdp ";

/// What a connection talks to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Endpoint {
    Browser,
    /// A tab (`Tab::id`).
    Page(u64),
}

struct Conn {
    endpoint: Endpoint,
    /// Flattened sessions (`Target.attachToTarget`): session id → tab.
    sessions: HashMap<String, u64>,
    /// Enabled domains, per session (`None`: the connection itself).
    enabled: HashSet<(Option<String>, String)>,
    /// `Target.setDiscoverTargets`.
    discover: bool,
}

/// Protocol state: connections, replies waiting for scripts, output.
#[derive(Default)]
pub struct Cdp {
    conns: HashMap<u32, Conn>,
    /// Messages to send: (connection, JSON).
    out: Vec<(u32, String)>,
    /// `Runtime.evaluate` calls waiting for the page: key → (connection,
    /// request id, session).
    pending: HashMap<u64, (u32, Value, Option<String>)>,
    next_key: u64,
    next_session: u64,
    /// A client sent `Browser.close`.
    close: bool,
}

/// A CDP target id for tab `id`.
fn target_id(tab: u64) -> String {
    format!("{tab:032X}")
}

fn parse_target(s: &str) -> Option<u64> {
    u64::from_str_radix(s, 16).ok()
}

/// CDP node ids start at 1.
fn node_id(n: NodeId) -> usize {
    n + 1
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |a, &b| a << 8 | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            out.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// Seconds since the Unix epoch (CDP's `wallTime`), or the frame time.
fn seconds(b: &Browser, host: &mut dyn Host) -> f64 {
    host.wall_clock().unwrap_or(b.now) / 1000.0
}

/// The network resource type of a request kind (`devtools::NetEntry`).
fn resource_type(kind: &str) -> &'static str {
    match kind {
        "document" => "Document",
        "stylesheet" => "Stylesheet",
        "script" => "Script",
        "image" => "Image",
        "font" => "Font",
        "fetch" => "Fetch",
        _ => "Other",
    }
}

/// How a request ended: (status, headers, size) or the error.
pub(super) type Ended<'a> = Result<(u16, &'a [(String, String)], usize), &'a str>;

/// Methods that are accepted and do nothing here.
const NO_OPS: &[&str] = &[
    "Page.setLifecycleEventsEnabled",
    "Page.setBypassCSP",
    "Network.setCacheDisabled",
    "Runtime.runIfWaitingForDebugger",
    "Runtime.addBinding",
    "Target.setAutoAttach",
    "Emulation.setFocusEmulationEnabled",
    "Log.startViolationsReport",
    "Overlay.setShowViewportSizeOnResize",
    "Page.setInterceptFileChooserDialog",
];

impl Browser {
    // ----- connections ---------------------------------------------------------

    /// The answer to an HTTP request on the debugging port (`/json/version`,
    /// `/json`, `/json/list`, `/json/new?URL`, `/json/close/ID`,
    /// `/json/activate/ID`): status and JSON body.
    pub fn cdp_http(&mut self, path: &str, port: u16, host: &mut dyn Host) -> (u16, String) {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let page = |b: &Browser, i: usize| {
            let t = &b.tabs[i];
            let id = target_id(t.id);
            json!({
                "id": id,
                "type": "page",
                "title": t.title(),
                "url": t.url(),
                "description": "",
                "devtoolsFrontendUrl": "",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/page/{id}"),
            })
        };
        let body = match path.trim_end_matches('/') {
            "/json/version" => json!({
                "Browser": format!("rubrowser/{}", env!("CARGO_PKG_VERSION")),
                "Protocol-Version": "1.3",
                "User-Agent": format!("rubrowser/{}", env!("CARGO_PKG_VERSION")),
                "V8-Version": "",
                "WebKit-Version": "",
                "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/browser/rubrowser"),
            }),
            "/json" | "/json/list" => Value::Array((0..self.tabs.len()).map(|i| page(self, i)).collect()),
            "/json/new" => {
                let url = crate::net::url::percent_decode(query);
                let url = String::from_utf8_lossy(&url).into_owned();
                self.new_tab(if url.is_empty() { "about:blank" } else { &url }, host);
                page(self, self.active)
            }
            p if p.starts_with("/json/close/") || p.starts_with("/json/activate/") => {
                let id = p.rsplit('/').next().and_then(parse_target);
                let Some(i) = id.and_then(|id| self.tab_index(id)) else {
                    return (404, "No such target id".into());
                };
                if p.starts_with("/json/close/") {
                    self.close_tab(i, host);
                    return (200, "Target is closing".into());
                }
                self.active = i;
                self.tab_switched();
                return (200, "Target activated".into());
            }
            _ => return (404, "Unknown command".into()),
        };
        (200, body.to_string())
    }

    /// A WebSocket connection to `path` (`/devtools/page/ID` or
    /// `/devtools/browser/…`) was opened; returns whether there is such a
    /// target.
    pub fn cdp_open(&mut self, conn: u32, path: &str) -> bool {
        let endpoint = if path.starts_with("/devtools/browser/") {
            Endpoint::Browser
        } else if let Some(id) = path.strip_prefix("/devtools/page/").and_then(parse_target) {
            if self.tab_index(id).is_none() {
                return false;
            }
            Endpoint::Page(id)
        } else {
            return false;
        };
        self.cdp.conns.insert(
            conn,
            Conn {
                endpoint,
                sessions: HashMap::new(),
                enabled: HashSet::new(),
                discover: false,
            },
        );
        true
    }

    pub fn cdp_close(&mut self, conn: u32) {
        self.cdp.conns.remove(&conn);
        self.cdp.pending.retain(|_, p| p.0 != conn);
        crate::script::debug::detach(conn);
        for t in &mut self.tabs {
            if t.debug.as_ref().is_some_and(|l| l.conn == conn) {
                t.debug = None;
                for s in t.scripts.iter_mut().flatten() {
                    s.set_debug(None);
                }
            }
        }
    }

    /// Debugger.disable: the runtime stops pausing.
    fn cdp_debugger_detach(&mut self, conn: u32, host: &mut dyn Host) {
        for i in 0..self.tabs.len() {
            if self.tabs[i].debug.as_ref().is_some_and(|l| l.conn == conn) {
                self.run_source(i, JS_MIME, "typeof __rb_debug_disable === 'function' && __rb_debug_disable()", "devtools", host);
            }
        }
        self.cdp_close_debugger_only(conn);
    }

    fn cdp_close_debugger_only(&mut self, conn: u32) {
        crate::script::debug::detach(conn);
        for t in &mut self.tabs {
            if t.debug.as_ref().is_some_and(|l| l.conn == conn) {
                t.debug = None;
                for s in t.scripts.iter_mut().flatten() {
                    s.set_debug(None);
                }
            }
        }
    }

    /// Runs `code(key)` in tab `i`'s runtime; its `__rb` helper answers
    /// request `id` with the key.
    #[allow(clippy::too_many_arguments)]
    fn cdp_js(&mut self, i: usize, conn: u32, id: Value, session: Option<String>, host: &mut dyn Host, code: impl FnOnce(u64) -> String) {
        self.cdp.next_key += 1;
        let key = self.cdp.next_key;
        self.cdp.pending.insert(key, (conn, id, session));
        let code = code(key);
        self.run_source(i, JS_MIME, &code, "devtools", host);
    }

    /// Messages the debugger handled outside the event loop: what paused
    /// runtimes sent without a server (tests), and commands that arrived
    /// just after a runtime resumed.
    pub(super) fn cdp_debugger_flush(&mut self, host: &mut dyn Host) {
        for l in crate::script::debug::links() {
            for text in l.take_sent() {
                self.cdp.out.push((l.conn, text));
            }
            for text in l.take_leftovers() {
                self.cdp_message(l.conn, &text, host);
            }
        }
    }

    /// Whether a remote debugger asked the browser to quit.
    pub fn cdp_wants_close(&self) -> bool {
        self.cdp.close
    }

    /// Replies and events to send: (connection, JSON text).
    pub fn take_cdp_output(&mut self) -> Vec<(u32, String)> {
        std::mem::take(&mut self.cdp.out)
    }

    fn cdp_send(&mut self, conn: u32, msg: Value) {
        self.cdp.out.push((conn, msg.to_string()));
    }

    fn cdp_reply(&mut self, conn: u32, id: Value, session: Option<String>, result: Result<Value, String>) {
        let mut msg = match result {
            Ok(r) => json!({ "id": id, "result": r }),
            Err(e) => json!({ "id": id, "error": { "code": -32000, "message": e } }),
        };
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.cdp_send(conn, msg);
    }

    /// Sends event `method` of `domain` about tab `tab` to every
    /// connection and session that enabled the domain for it.
    pub(super) fn cdp_event(&mut self, tab: u64, domain: &str, method: &str, params: Value) {
        if self.cdp.conns.is_empty() {
            return;
        }
        let mut out = Vec::new();
        for (&c, conn) in &self.cdp.conns {
            if conn.endpoint == Endpoint::Page(tab) && conn.enabled.contains(&(None, domain.to_string())) {
                out.push((c, json!({ "method": method, "params": params }).to_string()));
            }
            for (sid, &t) in &conn.sessions {
                if t == tab && conn.enabled.contains(&(Some(sid.clone()), domain.to_string())) {
                    out.push((
                        c,
                        json!({ "method": method, "params": params, "sessionId": sid }).to_string(),
                    ));
                }
            }
        }
        self.cdp.out.extend(out);
    }

    /// A `Target` event to connections that discover targets.
    fn cdp_target_event(&mut self, method: &str, params: Value) {
        let conns: Vec<u32> = self
            .cdp
            .conns
            .iter()
            .filter(|(_, c)| c.discover)
            .map(|(&id, _)| id)
            .collect();
        for c in conns {
            self.cdp_send(c, json!({ "method": method, "params": params }));
        }
    }

    fn target_info(&self, i: usize) -> Value {
        let t = &self.tabs[i];
        json!({
            "targetId": target_id(t.id),
            "type": "page",
            "title": t.title(),
            "url": t.url(),
            "attached": self.cdp.conns.values().any(|c| c.endpoint == Endpoint::Page(t.id) || c.sessions.values().any(|&s| s == t.id)),
            "canAccessOpener": false,
            "browserContextId": "default",
        })
    }

    // ----- hooks from the browser ---------------------------------------------

    /// Tab `i` shows a new document (its URL is known, scripts not run).
    pub(super) fn cdp_navigated(&mut self, i: usize, host: &mut dyn Host) {
        if self.cdp.conns.is_empty() {
            return;
        }
        let t = &self.tabs[i];
        let (tab, page, url) = (t.id, t.page, t.doc.url.clone());
        let frame = target_id(tab);
        let ts = seconds(self, host);
        self.cdp_event(tab, "Page", "Page.frameNavigated", json!({ "frame": {
            "id": frame, "loaderId": format!("L{page}"), "url": url, "securityOrigin": super::origin(&url).unwrap_or_default(),
            "mimeType": "text/html", "domainAndRegistry": "", "secureContextType": "Secure", "crossOriginIsolatedContextType": "NotIsolated", "gatedAPIFeatures": [] },
            "type": "Navigation" }));
        self.cdp_event(tab, "Page", "Page.lifecycleEvent", json!({ "frameId": frame, "loaderId": format!("L{page}"), "name": "init", "timestamp": ts }));
        self.cdp_event(tab, "Runtime", "Runtime.executionContextsCleared", json!({}));
        let ctx = self.cdp_context(i);
        self.cdp_event(tab, "Runtime", "Runtime.executionContextCreated", json!({ "context": ctx }));
        self.cdp_event(tab, "DOM", "DOM.documentUpdated", json!({}));
        let infos = self.target_info(i);
        self.cdp_target_event("Target.targetInfoChanged", json!({ "targetInfo": infos }));
    }

    /// Tab `i`'s scripts ran: DOMContentLoaded and load.
    pub(super) fn cdp_loaded(&mut self, i: usize, host: &mut dyn Host) {
        if self.cdp.conns.is_empty() {
            return;
        }
        let (tab, page) = (self.tabs[i].id, self.tabs[i].page);
        let ts = seconds(self, host);
        let frame = target_id(tab);
        self.cdp_event(tab, "Page", "Page.domContentEventFired", json!({ "timestamp": ts }));
        self.cdp_event(tab, "Page", "Page.lifecycleEvent", json!({ "frameId": frame, "loaderId": format!("L{page}"), "name": "DOMContentLoaded", "timestamp": ts }));
        self.cdp_event(tab, "Page", "Page.loadEventFired", json!({ "timestamp": ts }));
        self.cdp_event(tab, "Page", "Page.lifecycleEvent", json!({ "frameId": frame, "loaderId": format!("L{page}"), "name": "load", "timestamp": ts }));
        self.cdp_event(tab, "Page", "Page.frameStoppedLoading", json!({ "frameId": frame }));
    }

    /// A console message of tab `i`.
    pub(super) fn cdp_console(&mut self, i: usize, level: Level, text: &str, host: &mut dyn Host) {
        if self.cdp.conns.is_empty() || matches!(level, Level::Command | Level::Result) {
            return;
        }
        let tab = self.tabs[i].id;
        let ts = seconds(self, host) * 1000.0;
        if level == Level::Error && text.starts_with("Uncaught ") {
            self.cdp_event(tab, "Runtime", "Runtime.exceptionThrown", json!({ "timestamp": ts, "exceptionDetails": {
                "exceptionId": 1, "text": text.lines().next().unwrap_or(text), "lineNumber": 0, "columnNumber": 0,
                "executionContextId": self.tabs[i].page,
                "exception": { "type": "object", "subtype": "error", "description": text } } }));
            return;
        }
        let kind = match level {
            Level::Warn => "warning",
            Level::Error => "error",
            _ => "log",
        };
        self.cdp_event(tab, "Runtime", "Runtime.consoleAPICalled", json!({
            "type": kind, "args": [{ "type": "string", "value": text }],
            "executionContextId": self.tabs[i].page, "timestamp": ts }));
        let level = if kind == "warning" { "warning" } else if kind == "error" { "error" } else { "info" };
        self.cdp_event(tab, "Log", "Log.entryAdded", json!({ "entry": {
            "source": "javascript", "level": level, "text": text, "timestamp": ts } }));
    }

    /// Request `id` started (it is in `self.requests`).
    pub(super) fn cdp_request(&mut self, id: u32, url: &str, method: &str, kind: &str, post: Option<&[u8]>, host: &mut dyn Host) {
        if self.cdp.conns.is_empty() {
            return;
        }
        let Some(tab) = self.requests.get(&id).map(Request::tab) else {
            return;
        };
        let page = self.tab_index(tab).map_or(0, |i| self.tabs[i].page);
        let ts = seconds(self, host);
        let mut request = json!({ "url": url, "method": method, "headers": {}, "initialPriority": "High", "referrerPolicy": "strict-origin-when-cross-origin" });
        if let Some(body) = post.filter(|b| !b.is_empty()) {
            request["postData"] = json!(String::from_utf8_lossy(body));
            request["hasPostData"] = json!(true);
        }
        self.cdp_event(tab, "Network", "Network.requestWillBeSent", json!({
            "requestId": id.to_string(), "loaderId": format!("L{page}"), "documentURL": url, "request": request,
            "timestamp": ts, "wallTime": ts, "initiator": { "type": "other" }, "redirectHasExtraInfo": false,
            "type": resource_type(kind), "frameId": target_id(tab) }));
    }

    /// Request `id` ended: `status` and headers, or `error`.
    pub(super) fn cdp_response(
        &mut self,
        id: u32,
        url: &str,
        kind: &str,
        result: Ended<'_>,
        host: &mut dyn Host,
    ) {
        if self.cdp.conns.is_empty() {
            return;
        }
        let Some(tab) = self.requests.get(&id).map(Request::tab) else {
            return;
        };
        let ts = seconds(self, host);
        let rid = id.to_string();
        match result {
            Ok((status, headers, size)) => {
                let h: serde_json::Map<String, Value> = headers.iter().map(|(n, v)| (n.clone(), json!(v))).collect();
                let mime = headers
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                    .map_or("", |(_, v)| v.split(';').next().unwrap_or("").trim());
                self.cdp_event(tab, "Network", "Network.responseReceived", json!({
                    "requestId": rid, "loaderId": "", "timestamp": ts, "type": resource_type(kind), "frameId": target_id(tab),
                    "response": { "url": url, "status": status, "statusText": "", "headers": h, "mimeType": mime,
                        "connectionReused": false, "connectionId": 0, "encodedDataLength": size, "securityState": "unknown" } }));
                self.cdp_event(tab, "Network", "Network.loadingFinished", json!({
                    "requestId": rid, "timestamp": ts, "encodedDataLength": size }));
            }
            Err(e) => self.cdp_event(tab, "Network", "Network.loadingFailed", json!({
                "requestId": rid, "timestamp": ts, "type": resource_type(kind), "errorText": e, "canceled": false })),
        }
    }

    /// A result of `__rb_cdp_eval` (`KEY JSON`).
    pub(super) fn cdp_eval_result(&mut self, msg: &str) {
        let (key, json) = msg.split_once(' ').unwrap_or((msg, "{}"));
        let Some((conn, id, session)) = key.parse().ok().and_then(|k: u64| self.cdp.pending.remove(&k)) else {
            return;
        };
        let value: Value = serde_json::from_str(json).unwrap_or_else(|_| json!({ "result": { "type": "undefined" } }));
        // The page answers {error} for an unknown object id.
        let r = match value.get("error").and_then(Value::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(value),
        };
        self.cdp_reply(conn, id, session, r);
    }

    fn cdp_context(&self, i: usize) -> Value {
        let t = &self.tabs[i];
        json!({ "id": t.page, "origin": super::origin(&t.doc.url).unwrap_or_default(), "name": "",
            "uniqueId": format!("{}.{}", t.id, t.page), "auxData": { "isDefault": true, "type": "default", "frameId": target_id(t.id) } })
    }

    // ----- messages -------------------------------------------------------------

    /// A message from connection `conn`.
    pub fn cdp_message(&mut self, conn: u32, text: &str, host: &mut dyn Host) {
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            return self.cdp_send(conn, json!({ "id": 0, "error": { "code": -32700, "message": "Message must be a valid JSON" } }));
        };
        let id = msg.get("id").cloned().unwrap_or(json!(0));
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let session = msg.get("sessionId").and_then(Value::as_str).map(String::from);
        let Some(c) = self.cdp.conns.get(&conn) else {
            return;
        };
        let tab = match (&session, c.endpoint) {
            (Some(s), _) => match c.sessions.get(s) {
                Some(&t) => Some(t),
                None => {
                    let e = format!("Session with given id not found: {s}");
                    return self.cdp_reply(conn, id, session, Err(e));
                }
            },
            (None, Endpoint::Page(t)) => Some(t),
            (None, Endpoint::Browser) => None,
        };
        let result = self.cdp_call(conn, &id, session.clone(), tab, &method, &params, host);
        match result {
            Ok(Some(v)) => self.cdp_reply(conn, id, session, Ok(v)),
            Ok(None) => {} // answered later
            Err(e) => {
                let mut msg = json!({ "id": id, "error": { "code": if e.ends_with("wasn't found") { -32601 } else { -32000 }, "message": e } });
                if let Some(s) = session {
                    msg["sessionId"] = json!(s);
                }
                self.cdp_send(conn, msg);
            }
        }
    }

    /// Runs one method; `Ok(None)` when the reply comes later.
    #[allow(clippy::too_many_arguments)]
    fn cdp_call(
        &mut self,
        conn: u32,
        id: &Value,
        session: Option<String>,
        tab: Option<u64>,
        method: &str,
        p: &Value,
        host: &mut dyn Host,
    ) -> Result<Option<Value>, String> {
        let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let n = |k: &str| p.get(k).and_then(Value::as_f64);
        let (domain, name) = method.split_once('.').unwrap_or((method, ""));
        // Browser-wide methods.
        match method {
            "Browser.getVersion" => {
                let v = format!("rubrowser/{}", env!("CARGO_PKG_VERSION"));
                return Ok(Some(json!({ "protocolVersion": "1.3", "product": v, "revision": "", "userAgent": v, "jsVersion": "boa" })));
            }
            "Browser.close" => {
                self.cdp.close = true;
                return Ok(Some(json!({})));
            }
            "Target.getTargets" => {
                let infos: Vec<Value> = (0..self.tabs.len()).map(|i| self.target_info(i)).collect();
                return Ok(Some(json!({ "targetInfos": infos })));
            }
            "Target.getBrowserContexts" => return Ok(Some(json!({ "browserContextIds": [] }))),
            "Target.setDiscoverTargets" => {
                let on = p.get("discover").and_then(Value::as_bool).unwrap_or(false);
                if let Some(c) = self.cdp.conns.get_mut(&conn) {
                    c.discover = on;
                }
                if on {
                    for i in 0..self.tabs.len() {
                        let info = self.target_info(i);
                        self.cdp_send(conn, json!({ "method": "Target.targetCreated", "params": { "targetInfo": info } }));
                    }
                }
                return Ok(Some(json!({})));
            }
            "Target.createTarget" => {
                let url = s("url");
                self.new_tab(if url.is_empty() { "about:blank" } else { &url }, host);
                let info = self.target_info(self.active);
                self.cdp_target_event("Target.targetCreated", json!({ "targetInfo": info }));
                return Ok(Some(json!({ "targetId": target_id(self.tabs[self.active].id) })));
            }
            "Target.closeTarget" | "Target.activateTarget" | "Target.attachToTarget" => {
                let t = parse_target(&s("targetId")).ok_or("No target with given id found")?;
                let i = self.tab_index(t).ok_or("No target with given id found")?;
                match name {
                    "closeTarget" => {
                        self.close_tab(i, host);
                        self.cdp_target_event("Target.targetDestroyed", json!({ "targetId": target_id(t) }));
                        return Ok(Some(json!({ "success": true })));
                    }
                    "activateTarget" => {
                        self.active = i;
                        self.tab_switched();
                        return Ok(Some(json!({})));
                    }
                    _ => {
                        self.cdp.next_session += 1;
                        let sid = format!("S{:X}{:X}", t, self.cdp.next_session);
                        if let Some(c) = self.cdp.conns.get_mut(&conn) {
                            c.sessions.insert(sid.clone(), t);
                        }
                        let info = self.target_info(i);
                        self.cdp_send(conn, json!({ "method": "Target.attachedToTarget", "params": {
                            "sessionId": sid, "targetInfo": info, "waitingForDebugger": false } }));
                        return Ok(Some(json!({ "sessionId": sid })));
                    }
                }
            }
            "Target.detachFromTarget" => {
                let sid = s("sessionId");
                if let Some(c) = self.cdp.conns.get_mut(&conn) {
                    c.sessions.remove(&sid);
                }
                return Ok(Some(json!({})));
            }
            _ => {}
        }
        if NO_OPS.contains(&method) || name == "disable" {
            if name == "disable"
                && let Some(c) = self.cdp.conns.get_mut(&conn)
            {
                c.enabled.remove(&(session.clone(), domain.to_string()));
            }
            if method == "Debugger.disable" {
                self.cdp_debugger_detach(conn, host);
            }
            return Ok(Some(json!({})));
        }
        let tab = tab.ok_or_else(|| format!("'{method}' wasn't found"))?;
        let i = self.tab_index(tab).ok_or("No target with given id found")?;
        // The page's methods act on the active tab.
        if i != self.active {
            self.active = i;
            self.tab_switched();
        }
        if name == "enable" {
            if let Some(c) = self.cdp.conns.get_mut(&conn) {
                c.enabled.insert((session.clone(), domain.to_string()));
            }
            if domain == "Debugger" {
                // Attach this connection to the page's runtime; the runtime
                // reports scripts compiled from now on (scriptParsed).
                let link = crate::script::debug::attach(conn, session.clone());
                let t = &mut self.tabs[i];
                t.debug = Some(link.clone());
                for s in t.scripts.iter_mut().flatten() {
                    s.set_debug(Some(link.clone()));
                }
                if self.javascript || self.tabs[i].doc.url.starts_with("about:") {
                    self.run_source(i, JS_MIME, "typeof __rb_debug_enable === 'function' && __rb_debug_enable()", "devtools", host);
                }
                return Ok(Some(json!({ "debuggerId": "rubrowser" })));
            }
            if domain == "Runtime" {
                let ctx = self.cdp_context(i);
                let mut msg = json!({ "method": "Runtime.executionContextCreated", "params": { "context": ctx } });
                if let Some(s) = &session {
                    msg["sessionId"] = json!(s);
                }
                self.cdp_send(conn, msg);
            }
            return Ok(Some(json!({})));
        }
        match method {
            // ----- Page
            "Page.navigate" => {
                let url = crate::net::url::resolve(&self.tabs[i].doc.url, &s("url"));
                self.navigate(&url, host);
                let t = &self.tabs[i];
                Ok(Some(json!({ "frameId": target_id(t.id), "loaderId": format!("L{}", t.serial) })))
            }
            "Page.reload" => {
                self.reload(host);
                Ok(Some(json!({})))
            }
            "Page.getFrameTree" => {
                let t = &self.tabs[i];
                Ok(Some(json!({ "frameTree": { "frame": {
                    "id": target_id(t.id), "loaderId": format!("L{}", t.page), "url": t.doc.url,
                    "securityOrigin": super::origin(&t.doc.url).unwrap_or_default(), "mimeType": "text/html",
                    "domainAndRegistry": "", "secureContextType": "Secure", "crossOriginIsolatedContextType": "NotIsolated", "gatedAPIFeatures": [] } } })))
            }
            "Page.getNavigationHistory" => {
                let t = &self.tabs[i];
                let entries: Vec<Value> = t.history.iter().enumerate()
                    .map(|(k, u)| json!({ "id": k, "url": u, "userTypedURL": u, "title": "", "transitionType": "link" }))
                    .collect();
                Ok(Some(json!({ "currentIndex": t.index, "entries": entries })))
            }
            "Page.getLayoutMetrics" => {
                let vp = self.viewport_rect();
                let view = json!({ "pageX": 0, "pageY": 0, "clientWidth": vp.w, "clientHeight": vp.h });
                let size = json!({ "x": 0, "y": 0, "width": vp.w, "height": vp.h });
                Ok(Some(json!({ "layoutViewport": view, "cssLayoutViewport": view,
                    "visualViewport": { "offsetX": 0, "offsetY": 0, "pageX": 0, "pageY": 0, "clientWidth": vp.w, "clientHeight": vp.h, "scale": 1, "zoom": 1 },
                    "contentSize": size, "cssContentSize": size })))
            }
            "Page.captureScreenshot" => {
                let png = self.cdp_screenshot(host)?;
                Ok(Some(json!({ "data": base64(&png) })))
            }
            "Page.bringToFront" => Ok(Some(json!({}))),
            // ----- Runtime
            "Runtime.evaluate" | "Runtime.callFunctionOn" => {
                let web = !self.tabs[i].doc.url.starts_with("about:");
                if web && !self.javascript {
                    return Err("JavaScript is turned off".into());
                }
                let wait = p.get("awaitPromise").and_then(Value::as_bool).unwrap_or(false);
                let by_value = p.get("returnByValue").and_then(Value::as_bool).unwrap_or(false);
                let group = js_string(&s("objectGroup"));
                let code = if method == "Runtime.evaluate" {
                    format!("__rb_cdp_eval({{key}}, {}, {wait}, null, null, {group}, {by_value})", js_string(&s("expression")))
                } else {
                    let args: Vec<Value> = p.get("arguments").and_then(Value::as_array).cloned().unwrap_or_default();
                    let args: Vec<String> = args
                        .iter()
                        .map(|a| match (a.get("value"), a.get("unserializableValue").and_then(Value::as_str), a.get("objectId").and_then(Value::as_str)) {
                            (_, _, Some(o)) => format!("{{objectId: {}}}", js_string(o)),
                            (_, Some(u), _) => u.to_string(),
                            (Some(v), _, _) => v.to_string(),
                            _ => "undefined".into(),
                        })
                        .collect();
                    let this = p.get("objectId").and_then(Value::as_str).map_or("null".into(), js_string);
                    format!("__rb_cdp_eval({{key}}, {}, {wait}, [{}], {this}, {group}, {by_value})", js_string(&s("functionDeclaration")), args.join(","))
                };
                self.cdp.next_key += 1;
                let key = self.cdp.next_key;
                self.cdp.pending.insert(key, (conn, id.clone(), session));
                self.run_source(i, JS_MIME, &code.replace("{key}", &key.to_string()), "devtools", host);
                Ok(None)
            }
            "Runtime.getProperties" => {
                let web = !self.tabs[i].doc.url.starts_with("about:");
                if web && !self.javascript {
                    return Err("JavaScript is turned off".into());
                }
                let own = p.get("ownProperties").and_then(Value::as_bool).unwrap_or(false);
                let accessors = p.get("accessorPropertiesOnly").and_then(Value::as_bool).unwrap_or(false);
                self.cdp.next_key += 1;
                let key = self.cdp.next_key;
                self.cdp.pending.insert(key, (conn, id.clone(), session));
                let code = format!("__rb_cdp_props({key}, {}, {own}, {accessors})", js_string(&s("objectId")));
                self.run_source(i, JS_MIME, &code, "devtools", host);
                Ok(None)
            }
            "Runtime.releaseObject" | "Runtime.releaseObjectGroup" => {
                let code = if method == "Runtime.releaseObject" {
                    format!("__rb_cdp_release({}, '')", js_string(&s("objectId")))
                } else {
                    format!("__rb_cdp_release(null, {})", js_string(&s("objectGroup")))
                };
                if self.javascript || self.tabs[i].doc.url.starts_with("about:") {
                    self.run_source(i, JS_MIME, &code, "devtools", host);
                }
                Ok(Some(json!({})))
            }
            "Runtime.getIsolateId" => Ok(Some(json!({ "id": "rubrowser" }))),
            // ----- Debugger (the rujse runtime; the page's scripts pause
            // inside their call, see script::debug)
            "Debugger.setBreakpointByUrl" => {
                let line = n("lineNumber").unwrap_or(0.0);
                let col = n("columnNumber").map_or("null".to_string(), |c| c.to_string());
                let cond = js_string(&s("condition"));
                let url = js_string(&s("url"));
                // rujse positions are 1-based.
                let col1 = n("columnNumber").map_or("null".to_string(), |c| (c + 1.0).to_string());
                self.tabs[i].debug_breakpoints.push(format!("{url}, {}, {col1}, {cond}", line + 1.0));
                self.cdp_js(i, conn, id.clone(), session.clone(), host, |key| format!("__rb_debug_bp({key}, {url}, {line}, {col}, {cond})"));
                Ok(None)
            }
            "Debugger.removeBreakpoint" | "Debugger.setPauseOnExceptions" | "Debugger.getScriptSource" | "Debugger.pause" => {
                let arg = match method {
                    "Debugger.removeBreakpoint" => js_string(&s("breakpointId")),
                    "Debugger.setPauseOnExceptions" => js_string(&s("state")),
                    "Debugger.getScriptSource" => js_string(&s("scriptId")),
                    _ => "null".into(),
                };
                let what = js_string(method);
                self.cdp_js(i, conn, id.clone(), session.clone(), host, |key| format!("__rb_debug_call({key}, {what}, {arg})"));
                Ok(None)
            }
            // Not paused: nothing to resume or step.
            "Debugger.resume" | "Debugger.stepOver" | "Debugger.stepInto" | "Debugger.stepOut" => Ok(Some(json!({}))),
            "Debugger.setAsyncCallStackDepth" | "Debugger.setBlackboxPatterns" | "Debugger.setBreakpointsActive" | "Debugger.setSkipAllPauses" => Ok(Some(json!({}))),
            // ----- DOM
            "DOM.getDocument" => {
                let depth = n("depth").map_or(2, |d| if d < 0.0 { usize::MAX } else { d as usize + 1 });
                let doc = &self.tabs[i].doc;
                Ok(Some(json!({ "root": dom_node(&doc.dom, Dom::ROOT, depth, &doc.url) })))
            }
            "DOM.querySelector" | "DOM.querySelectorAll" => {
                let root = self.cdp_node(i, p)?;
                let doc = &self.tabs[i].doc;
                let found = crate::css::query(&doc.dom, doc.dom.descendants(root), &s("selector"), &doc.state())
                    .ok_or("DOM Error while querying")?;
                let ids: Vec<usize> = found.into_iter().filter(|&f| f != root).map(node_id).collect();
                Ok(Some(if method == "DOM.querySelector" {
                    json!({ "nodeId": ids.first().copied().unwrap_or(0) })
                } else {
                    json!({ "nodeIds": ids })
                }))
            }
            "DOM.describeNode" | "DOM.requestChildNodes" => {
                let node = self.cdp_node(i, p)?;
                let depth = n("depth").map_or(1, |d| if d < 0.0 { usize::MAX } else { d as usize });
                let doc = &self.tabs[i].doc;
                if method == "DOM.describeNode" {
                    return Ok(Some(json!({ "node": dom_node(&doc.dom, node, depth, &doc.url) })));
                }
                let kids: Vec<Value> = doc.dom.children(node).iter().map(|&k| dom_node(&doc.dom, k, depth, &doc.url)).collect();
                self.cdp_event(tab, "DOM", "DOM.setChildNodes", json!({ "parentId": node_id(node), "nodes": kids }));
                Ok(Some(json!({})))
            }
            "DOM.getOuterHTML" => {
                let node = self.cdp_node(i, p)?;
                Ok(Some(json!({ "outerHTML": self.tabs[i].doc.dom.to_html(node, true) })))
            }
            "DOM.getAttributes" => {
                let node = self.cdp_node(i, p)?;
                let el = self.tabs[i].doc.dom.element(node).ok_or("Node is not an Element")?;
                let flat: Vec<&str> = el.attrs.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]).collect();
                Ok(Some(json!({ "attributes": flat })))
            }
            "DOM.setAttributeValue" | "DOM.removeAttribute" => {
                let node = self.cdp_node(i, p)?;
                let doc = &mut self.tabs[i].doc;
                let e = doc.dom_mut().element_mut(node).ok_or("Node is not an Element")?;
                if method == "DOM.removeAttribute" {
                    e.remove_attr(&s("name"));
                } else {
                    e.set_attr(&s("name").to_ascii_lowercase(), &s("value"));
                }
                doc.dom_changed();
                self.dirty = true;
                Ok(Some(json!({})))
            }
            "DOM.setNodeValue" => {
                let node = self.cdp_node(i, p)?;
                self.tabs[i].doc.dom_mut().set_text(node, &s("value"));
                self.dirty = true;
                Ok(Some(json!({})))
            }
            "DOM.getBoxModel" => {
                let node = self.cdp_node(i, p)?;
                let doc = &mut self.tabs[i].doc;
                let (border, content) = doc.box_rects(node).ok_or("Could not compute box model.")?;
                let px = |doc: &mut crate::document::Document, prop: &str| {
                    doc.computed_value(node, prop)
                        .and_then(|v| v.trim_end_matches("px").parse::<f32>().ok())
                        .unwrap_or(0.0)
                };
                let side = |doc: &mut crate::document::Document, f: &str| {
                    ["top", "right", "bottom", "left"].map(|s| px(doc, &f.replace('*', s)))
                };
                let (b, m) = (side(doc, "border-*-width"), side(doc, "margin-*"));
                let quad = |x: f32, y: f32, w: f32, h: f32| json!([x, y, x + w, y, x + w, y + h, x, y + h]);
                let padding = (border.x + b[3], border.y + b[0], border.w - b[1] - b[3], border.h - b[0] - b[2]);
                Ok(Some(json!({ "model": {
                    "content": quad(content.x, content.y, content.w, content.h),
                    "padding": quad(padding.0, padding.1, padding.2, padding.3),
                    "border": quad(border.x, border.y, border.w, border.h),
                    "margin": quad(border.x - m[3], border.y - m[0], border.w + m[1] + m[3], border.h + m[0] + m[2]),
                    "width": border.w, "height": border.h } })))
            }
            "DOM.scrollIntoViewIfNeeded" | "DOM.focus" => {
                let node = self.cdp_node(i, p)?;
                if method == "DOM.focus" {
                    self.focus_page(Some(node), host);
                }
                Ok(Some(json!({})))
            }
            // ----- CSS
            "CSS.getComputedStyleForNode" => {
                let node = self.cdp_node(i, p)?;
                let doc = &mut self.tabs[i].doc;
                let props: Vec<Value> = super::devtools::COMPUTED
                    .iter()
                    .filter_map(|&name| Some(json!({ "name": name, "value": doc.computed_value(node, name)? })))
                    .collect();
                Ok(Some(json!({ "computedStyle": props })))
            }
            "CSS.getMatchedStylesForNode" | "CSS.getInlineStylesForNode" => {
                let node = self.cdp_node(i, p)?;
                let doc = &self.tabs[i].doc;
                let style = |decls: Vec<(String, String, bool, bool)>| {
                    let props: Vec<Value> = decls.into_iter()
                        .map(|(name, value, important, disabled)| json!({ "name": name, "value": value, "important": important, "disabled": disabled }))
                        .collect();
                    json!({ "cssProperties": props, "shorthandEntries": [] })
                };
                let inline = crate::css::syntax::parse_declarations(doc.dom.attr(node, "style").unwrap_or(""), None)
                    .into_iter()
                    .map(|d| (d.name, d.value, d.important, false))
                    .collect();
                let rules: Vec<Value> = doc.matched_rules(node).into_iter().map(|r| {
                    let decls = r.decls.iter().enumerate()
                        .map(|(k, d)| (d.name.clone(), d.value.clone(), d.important, doc.declaration_disabled(r.decls_id, k)))
                        .collect();
                    json!({ "rule": { "selectorList": { "text": &*r.selector, "selectors": [{ "text": &*r.selector }] },
                        "origin": if r.user_agent { "user-agent" } else { "regular" }, "style": style(decls) },
                        "matchingSelectors": [0] })
                }).collect();
                Ok(Some(json!({ "inlineStyle": style(inline), "matchedCSSRules": rules })))
            }
            // ----- Overlay
            "Overlay.highlightNode" => {
                let node = self.cdp_node(i, p)?;
                self.devtools.hover = Some(node);
                self.devtools.remote_highlight = true;
                self.dirty = true;
                Ok(Some(json!({})))
            }
            "Overlay.hideHighlight" => {
                self.devtools.hover = None;
                self.devtools.remote_highlight = false;
                self.dirty = true;
                Ok(Some(json!({})))
            }
            // ----- Network
            "Network.getResponseBody" => {
                let rid: u32 = s("requestId").parse().map_err(|_| "No resource with given identifier found")?;
                let e = self.tabs[i].dev.network.iter().find(|e| e.id == rid).ok_or("No resource with given identifier found")?;
                if e.end.is_none() {
                    return Err("No data found for resource with given identifier".into());
                }
                Ok(Some(match std::str::from_utf8(&e.body) {
                    Ok(t) => json!({ "body": t, "base64Encoded": false }),
                    Err(_) => json!({ "body": base64(&e.body), "base64Encoded": true }),
                }))
            }
            // ----- Input
            "Input.dispatchMouseEvent" => {
                let vp = self.viewport_rect();
                let (x, y) = ((vp.x + n("x").unwrap_or(0.0) as f32) as i32, (vp.y + n("y").unwrap_or(0.0) as f32) as i32);
                let button = match s("button").as_str() {
                    "right" => 2,
                    "middle" => 1,
                    _ => 0,
                };
                let now = self.now;
                let e = match s("type").as_str() {
                    "mousePressed" => Event::MouseDown { x, y, button },
                    "mouseReleased" => Event::MouseUp { x, y, button },
                    "mouseMoved" => Event::MouseMove { x, y },
                    "mouseWheel" => {
                        self.event(Event::MouseMove { x, y }, now, host);
                        Event::Wheel { dx: n("deltaX").unwrap_or(0.0) as i32, dy: n("deltaY").unwrap_or(0.0) as i32 }
                    }
                    t => return Err(format!("Unknown mouse event type: {t}")),
                };
                self.event(e, now, host);
                Ok(Some(json!({})))
            }
            "Input.dispatchKeyEvent" => {
                let bits = n("modifiers").unwrap_or(0.0) as i32;
                let mods = Modifiers { alt: bits & 1 != 0, ctrl: bits & 2 != 0, meta: bits & 4 != 0, shift: bits & 8 != 0 };
                let text = s("text");
                let code = key_code(&s("key"));
                let now = self.now;
                match s("type").as_str() {
                    "keyDown" | "rawKeyDown" => {
                        self.event(Event::KeyDown { key: code, mods, repeat: false }, now, host);
                        let printable = !text.is_empty() && !text.chars().any(char::is_control);
                        if s("type") == "keyDown" && printable && !mods.command() {
                            self.event(Event::Text(text), now, host);
                        }
                    }
                    "keyUp" => self.event(Event::KeyUp { key: code, mods }, now, host),
                    "char" => self.event(Event::Text(text), now, host),
                    t => return Err(format!("Unknown key event type: {t}")),
                }
                Ok(Some(json!({})))
            }
            "Input.insertText" => {
                let now = self.now;
                self.event(Event::Text(s("text")), now, host);
                Ok(Some(json!({})))
            }
            _ => Err(format!("'{method}' wasn't found")),
        }
    }

    /// The node named by `nodeId` (or `backendNodeId`) in `p`.
    fn cdp_node(&self, i: usize, p: &Value) -> Result<NodeId, String> {
        let id = p
            .get("nodeId")
            .or_else(|| p.get("backendNodeId"))
            .and_then(Value::as_u64)
            .ok_or("Either nodeId or backendNodeId must be specified")?;
        let n = (id as usize).checked_sub(1).ok_or("Could not find node with given id")?;
        self.tabs[i].doc.dom.get(n).ok_or("Could not find node with given id")?;
        Ok(n)
    }

    /// The page area of the window as PNG.
    fn cdp_screenshot(&mut self, host: &mut dyn Host) -> Result<Vec<u8>, String> {
        let vp = self.viewport_rect();
        let scale = self.scale;
        let now = self.now;
        let canvas = self.render(now, host);
        let (cw, ch) = (canvas.width(), canvas.height());
        let x0 = ((vp.x * scale) as usize).min(cw);
        let y0 = ((vp.y * scale) as usize).min(ch);
        let w = ((vp.w * scale) as usize).min(cw - x0);
        let h = ((vp.h * scale) as usize).min(ch - y0);
        let px = canvas.pixels();
        let mut rgba = Vec::with_capacity(w * h * 4);
        for y in y0..y0 + h {
            for &p in &px[y * cw + x0..y * cw + x0 + w] {
                rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
            }
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, rgba).ok_or("empty page area")?;
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        Ok(out)
    }
}

/// The `abi::key` code of a DOM `key` name.
fn key_code(name: &str) -> i32 {
    use crate::abi::key;
    match name {
        "Enter" => key::ENTER,
        "Tab" => key::TAB,
        "Backspace" => key::BACKSPACE,
        "Escape" => key::ESCAPE,
        " " => key::SPACE,
        "ArrowLeft" => key::LEFT,
        "ArrowRight" => key::RIGHT,
        "ArrowUp" => key::UP,
        "ArrowDown" => key::DOWN,
        "Delete" => key::DELETE,
        "Home" => key::HOME,
        "End" => key::END,
        "PageUp" => key::PAGE_UP,
        "PageDown" => key::PAGE_DOWN,
        "F5" => key::F5,
        "F12" => key::F12,
        "[" => key::LEFT_BRACKET,
        "]" => key::RIGHT_BRACKET,
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase() as i32,
                (Some(c), None) => key::CHAR_BASE + c as i32,
                _ => 0,
            }
        }
    }
}

/// A DOM node as CDP describes it, with children down to `depth` levels.
fn dom_node(dom: &Dom, n: NodeId, depth: usize, url: &str) -> Value {
    let kids: Vec<NodeId> = dom.children(n).to_vec();
    let mut v = json!({ "nodeId": node_id(n), "backendNodeId": node_id(n), "childNodeCount": kids.len() });
    if let Some(p) = dom.parent(n) {
        v["parentId"] = json!(node_id(p));
    }
    match &dom.node(n).data {
        NodeData::Document => {
            v["nodeType"] = json!(9);
            v["nodeName"] = json!("#document");
            v["localName"] = json!("");
            v["nodeValue"] = json!("");
            v["documentURL"] = json!(url);
            v["baseURL"] = json!(url);
            v["xmlVersion"] = json!("");
        }
        NodeData::Element(e) => {
            v["nodeType"] = json!(1);
            v["nodeName"] = json!(e.name.to_ascii_uppercase());
            v["localName"] = json!(e.name);
            v["nodeValue"] = json!("");
            let flat: Vec<&str> = e.attrs.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]).collect();
            v["attributes"] = json!(flat);
        }
        NodeData::Text(t) => {
            v["nodeType"] = json!(3);
            v["nodeName"] = json!("#text");
            v["localName"] = json!("");
            v["nodeValue"] = json!(t);
        }
        NodeData::Comment(t) => {
            v["nodeType"] = json!(8);
            v["nodeName"] = json!("#comment");
            v["localName"] = json!("");
            v["nodeValue"] = json!(t);
        }
    }
    if depth > 0 && !kids.is_empty() {
        let children: Vec<Value> = kids.iter().map(|&k| dom_node(dom, k, depth - 1, url)).collect();
        v["children"] = json!(children);
    }
    v
}

//! JavaScript runtime: the rujse engine compiled to wasm (`js-rujse.wasm`).
//!
//! The same protocol and browser environment as `lang::js` (boa): the
//! browser hands source code to `rb_eval`, DOM access goes through the
//! native `__rb` object, and `prelude.js` builds `document`, elements,
//! events and timers on top of it. Built by `scripts/build.sh` (feature
//! `js-runtime-rujse`, example `js_runtime_rujse`); the browser uses it
//! when built with the `rujse` feature.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use rujse::module::ModuleSource;
use rujse::{Throw, Value, Vm};

use crate::sdk::{ffi, read_string};

type JsResult<T> = Result<T, Throw>;

type Native = fn(&mut Vm, Value, &[Value], Value) -> JsResult<Value>;

const PRELUDE: &str = include_str!("prelude.js");
/// The heap after the prelude (`make_snapshot`, run by `scripts/build.sh`):
/// a page starts from it without running the prelude at all.
const PRELUDE_SNAPSHOT: &[u8] = include_bytes!("prelude.rjss");

thread_local! {
    static VM: RefCell<Option<Box<Vm>>> = const { RefCell::new(None) };
    /// Buffers handed out by `rb_alloc`, freed after `rb_eval`.
    static BUFFERS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    /// Set while a call runs: still set at the next call means the browser
    /// stopped that one (it ran too long).
    static RUNNING: Cell<bool> = const { Cell::new(false) };
    /// The `__rb` functions, by the index stored in each native function.
    static NATIVES: RefCell<Vec<Native>> = const { RefCell::new(Vec::new()) };
    static NAMES: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    /// `make_snapshot` is running (no browser to call).
    static SNAPSHOTTING: Cell<bool> = const { Cell::new(false) };
    /// ES module sources the browser provided (by URL) and the import map.
    static MODULES: RefCell<UrlModules> = RefCell::new(UrlModules::default());
}

#[derive(Default)]
struct UrlModules {
    sources: HashMap<String, String>,
    /// Import map entries (`specifier` or `prefix/` → URL).
    import_map: Vec<(String, String)>,
}

/// The browser side of the engine: time, time zone, output, modules and
/// interruption.
struct Host;

impl rujse::Host for Host {
    fn now(&mut self) -> f64 {
        unsafe { ffi::wall_time() }
    }

    // Date's local time is the host's time zone (the browser's tz database).
    fn local_tz_offset(&mut self, utc_ms: f64) -> f64 {
        let m = unsafe { ffi::tz_offset([].as_ptr(), 0, utc_ms) };
        if m.is_finite() { m } else { 0.0 }
    }

    fn uncaught_error(&mut self, text: &str) {
        log(&format!("Uncaught {text}"));
    }

    fn print(&mut self, text: &str) {
        log(text);
    }

    // The browser stops a script that runs too long: checked at the
    // engine's safepoints, so even a loop without calls stops cleanly.
    fn interrupt_requested(&mut self) -> bool {
        unsafe { ffi::interrupted() != 0 }
    }

    fn resolve_module(&mut self, referrer: Option<&str>, specifier: &str) -> Result<String, String> {
        MODULES.with(|m| Ok(crate::abi::resolve_module(specifier, referrer.unwrap_or(""), &m.borrow().import_map)))
    }

    // The browser sends every module of a graph before evaluating its root.
    fn fetch_module(&mut self, url: &str) -> ModuleSource {
        match MODULES.with(|m| m.borrow().sources.get(url).cloned()) {
            Some(src) => ModuleSource::Ready(src),
            None => ModuleSource::Error(format!("module not loaded: {url}")),
        }
    }
}

// ----- argument helpers -----------------------------------------------------

fn arg(args: &[Value], i: usize) -> Value {
    args.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn int(args: &[Value], i: usize, c: &mut Vm) -> JsResult<i32> {
    c.to_int32(arg(args, i))
}

/// The `SyntaxError` of `querySelector` & co. for an invalid selector.
fn invalid_selector(c: &mut Vm, s: &str) -> Throw {
    c.syntax_error(&format!("'{s}' is not a valid selector"))
}

fn num(args: &[Value], i: usize, c: &mut Vm) -> JsResult<f64> {
    c.to_number(arg(args, i))
}

fn text(args: &[Value], i: usize, c: &mut Vm) -> JsResult<String> {
    let s = c.to_string(arg(args, i))?;
    Ok(c.str(s).to_rust_lossy())
}

/// Like `text` (lone surrogates become U+FFFD in both).
fn text_lossy(args: &[Value], i: usize, c: &mut Vm) -> JsResult<String> {
    text(args, i, c)
}

fn string(c: &mut Vm, s: &str) -> Value {
    c.new_string(s)
}

fn opt_string(c: &mut Vm, s: Option<String>) -> Value {
    s.map_or(Value::NULL, |s| string(c, &s))
}

fn array(c: &mut Vm, items: Vec<Value>) -> Value {
    Value::object(c.new_array(items))
}

/// The bytes of an `ArrayBuffer` or typed array (its viewed part);
/// `None` for other values.
fn bytes(v: Value, c: &mut Vm) -> JsResult<Option<Vec<u8>>> {
    if let Some(b) = c.array_buffer_bytes(v) {
        return Ok(Some(b.to_vec()));
    }
    Ok(c.view_bytes(v).map(<[u8]>::to_vec))
}

/// A new `Uint8Array` holding `b`.
fn u8array(b: &[u8], c: &mut Vm) -> JsResult<Value> {
    Ok(c.uint8_array_from_vec(b.to_vec()))
}

/// Bytes the browser writes into a buffer, growing it if needed (`None`
/// when the browser returns -1).
fn read_bytes(mut get: impl FnMut(*mut u8, usize) -> i32) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = get(buf.as_mut_ptr(), buf.len());
        if n < 0 {
            return None;
        }
        let n = n as usize;
        if n <= buf.len() {
            buf.truncate(n);
            return Some(buf);
        }
        buf.resize(n, 0);
    }
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().fold(0u32, |a, &b| a << 8 | b as u32) << (8 * (3 - chunk.len()));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The `__rb` object: one function per rb import, with JS-friendly types.
fn natives() -> Vec<(&'static str, usize, Native)> {
    vec![
        ("query", 1, |c, _, a, _| {
            let s = text(a, 0, c)?;
            let h = unsafe { ffi::query(s.as_ptr(), s.len()) };
            if h == -2 {
                return Err(invalid_selector(c, &s));
            }
            Ok(h.into())
        }),
        ("queryAll", 1, |c, _, a, _| {
            let s = text(a, 0, c)?;
            let mut out = vec![0i32; 64];
            loop {
                let n = unsafe {
                    ffi::query_all(s.as_ptr(), s.len(), out.as_mut_ptr(), out.len() as i32)
                };
                if n < 0 {
                    return Err(invalid_selector(c, &s));
                }
                let n = n as usize;
                if n <= out.len() {
                    out.truncate(n);
                    break;
                }
                out.resize(n, 0);
            }
            Ok(array(c, out.into_iter().map(Value::int).collect()))
        }),
        ("queryWithin", 2, |c, _, a, _| {
            let root = int(a, 0, c)?;
            let s = text(a, 1, c)?;
            let mut out = vec![0i32; 64];
            loop {
                let n = unsafe {
                    ffi::query_within(
                        root,
                        s.as_ptr(),
                        s.len(),
                        out.as_mut_ptr(),
                        out.len() as i32,
                    )
                };
                if n < 0 {
                    return Err(invalid_selector(c, &s));
                }
                let n = n as usize;
                if n <= out.len() {
                    out.truncate(n);
                    break;
                }
                out.resize(n, 0);
            }
            Ok(array(c, out.into_iter().map(Value::int).collect()))
        }),
        ("wsOpen", 2, |c, _, a, _| {
            let (url, protocols) = (text(a, 0, c)?, text(a, 1, c)?);
            Ok(unsafe { ffi::ws_open(url.as_ptr(), url.len(), protocols.as_ptr(), protocols.len()) }.into())
        }),
        ("wsSend", 3, |c, _, a, _| {
            let id = int(a, 0, c)?;
            let (data, is_text) = match bytes(arg(a, 1), c)? {
                Some(b) => (b, 0),
                None => (text(a, 1, c)?.into_bytes(), 1),
            };
            unsafe { ffi::ws_send(id, data.as_ptr(), data.len(), is_text) };
            Ok(Value::UNDEFINED)
        }),
        ("wsClose", 3, |c, _, a, _| {
            let id = int(a, 0, c)?;
            let code = int(a, 1, c)?;
            let reason = text(a, 2, c)?;
            unsafe { ffi::ws_close(id, code, reason.as_ptr(), reason.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("http", 4, |c, _, a, _| {
            // The body is a string (sent as UTF-8) or bytes.
            let body = match bytes(arg(a, 2), c)? {
                Some(b) => b,
                None => text(a, 2, c)?.into_bytes(),
            };
            // The 4th argument: header lines (`name: value\r\n`, with
            // `:credentials` / `:mode`).
            let (url, method, lines) = (text(a, 0, c)?, text(a, 1, c)?, text(a, 3, c)?);
            Ok(unsafe {
                ffi::http_request(
                    url.as_ptr(),
                    url.len(),
                    method.as_ptr(),
                    method.len(),
                    body.as_ptr(),
                    body.len(),
                    lines.as_ptr(),
                    lines.len(),
                )
            }
            .into())        }),
        ("httpBody", 1, |c, _, a, _| {
            let id = int(a, 0, c)?;
            let mut buf = vec![0u8; 64 * 1024];
            let mut n = unsafe { ffi::http_body(id, buf.as_mut_ptr(), buf.len()) };
            if n > buf.len() as i32 {
                buf.resize(n as usize, 0);
                n = unsafe { ffi::http_body(id, buf.as_mut_ptr(), buf.len()) };
            }
            buf.truncate(n.max(0) as usize);
            Ok(string(c, &String::from_utf8_lossy(&buf)))
        }),
        ("storage", 4, |c, _, a, _| {
            // (area, op, key, value) → string, null, or a number for ops
            // that return one (see the `storage` import).
            let (area, op) = (int(a, 0, c)?, int(a, 1, c)?);
            let (key, value) = (text(a, 2, c)?, text(a, 3, c)?);
            let mut buf = vec![0u8; 256];
            loop {
                let n = unsafe {
                    ffi::storage(area, op, key.as_ptr(), key.len(), value.as_ptr(), value.len(), buf.as_mut_ptr(), buf.len())
                };
                if !matches!(op, 0 | 5) || n < 0 {
                    return Ok(if n == -1 && matches!(op, 0 | 5) { Value::NULL } else { n.into() });
                }
                let n = n as usize;
                if n <= buf.len() {
                    buf.truncate(n);
                    return Ok(string(c, &String::from_utf8_lossy(&buf)));
                }
                buf.resize(n, 0);
            }
        }),
        ("cookie", 0, |c, _, _, _| {
            Ok(string(c, &read_string(|p, n| unsafe { ffi::cookie(p, n) }).unwrap_or_default()))
        }),
        ("setCookie", 1, |c, _, a, _| {
            let v = text(a, 0, c)?;
            unsafe { ffi::set_cookie(v.as_ptr(), v.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("httpBytes", 1, |c, _, a, _| {
            let id = int(a, 0, c)?;
            let b = read_bytes(|p, n| unsafe { ffi::http_body(id, p, n) }).unwrap_or_default();
            u8array(&b, c)
        }),
        ("httpHeaders", 1, |c, _, a, _| {
            let id = int(a, 0, c)?;
            Ok(string(c, 
                &read_string(|p, n| unsafe { ffi::http_headers(id, p, n) }).unwrap_or_default(),
            ))
        }),
        ("files", 1, |c, _, a, _| {
            let h = int(a, 0, c)?;
            Ok(opt_string(c, read_string(|p, n| unsafe {
                ffi::files(h, p, n)
            })))
        }),
        ("fileData", 2, |c, _, a, _| {
            let (h, i) = (int(a, 0, c)?, int(a, 1, c)?);
            match read_bytes(|p, n| unsafe { ffi::file_data(h, i, p, n) }) {
                Some(b) => u8array(&b, c),
                None => Ok(Value::NULL),
            }
        }),
        ("blobUrl", 3, |c, _, a, _| {
            let (url, mime) = (text(a, 0, c)?, text(a, 1, c)?);
            let data = bytes(arg(a, 2), c)?.unwrap_or_default();
            unsafe {
                ffi::blob_url(
                    url.as_ptr(),
                    url.len(),
                    mime.as_ptr(),
                    mime.len(),
                    data.as_ptr(),
                    data.len(),
                )
            };
            Ok(Value::UNDEFINED)
        }),
        ("blobRevoke", 1, |c, _, a, _| {
            let url = text(a, 0, c)?;
            unsafe { ffi::blob_revoke(url.as_ptr(), url.len()) };
            Ok(Value::UNDEFINED)
        }),
        // Byte helpers for Blob, FileReader and fetch bodies.
        ("encodeUtf8", 1, |c, _, a, _| {
            let s = text_lossy(a, 0, c)?;
            u8array(s.as_bytes(), c)
        }),
        ("decodeUtf8", 1, |c, _, a, _| {
            let b = bytes(arg(a, 0), c)?.unwrap_or_default();
            // A BOM is not part of the text (Encoding: "UTF-8 decode").
            let b = b.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&b);
            Ok(string(c, &String::from_utf8_lossy(b)))
        }),
        ("base64", 1, |c, _, a, _| {
            let b = bytes(arg(a, 0), c)?.unwrap_or_default();
            Ok(string(c, &base64(&b)))
        }),
        ("rect", 2, |c, _, a, _| {
            Ok(unsafe { ffi::rect(int(a, 0, c)?, int(a, 1, c)?) }.into())
        }),
        ("audio", 1, |c, _, a, _| {
            let s = text(a, 0, c)?;
            Ok(unsafe { ffi::audio(s.as_ptr(), s.len()) }.into())
        }),
        ("quirks", 0, |_, _, _, _| Ok(unsafe { ffi::quirks() }.into())),
        ("focused", 0, |_, _, _, _| Ok(unsafe { ffi::focused() }.into())),
        ("imageInfo", 2, |c, _, a, _| {
            Ok(unsafe { ffi::image_info(int(a, 0, c)?, int(a, 1, c)?) }.into())
        }),
        ("nodeType", 1, |c, _, a, _| {
            Ok(unsafe { ffi::node_type(int(a, 0, c)?) }.into())
        }),
        ("relative", 2, |c, _, a, _| {
            Ok(unsafe { ffi::relative(int(a, 0, c)?, int(a, 1, c)?) }.into())
        }),
        ("childCount", 1, |c, _, a, _| {
            Ok(unsafe { ffi::child_count(int(a, 0, c)?) }.into())
        }),
        ("childAt", 2, |c, _, a, _| {
            Ok(unsafe { ffi::child_at(int(a, 0, c)?, int(a, 1, c)?) }.into())
        }),
        ("special", 1, |c, _, a, _| {
            Ok(unsafe { ffi::special(int(a, 0, c)?) }.into())
        }),
        ("tagName", 1, |c, _, a, _| {
            let h = int(a, 0, c)?;
            Ok(string(c, 
                &read_string(|p, n| unsafe { ffi::tag_name(h, p, n) }).unwrap_or_default(),
            ))
        }),
        ("matches", 2, |c, _, a, _| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            match unsafe { ffi::matches(h, s.as_ptr(), s.len()) } {
                r if r < 0 => Err(invalid_selector(c, &s)),
                r => Ok((r != 0).into()),
            }
        }),
        ("getText", 1, |c, _, a, _| {
            let h = int(a, 0, c)?;
            Ok(opt_string(c, read_string(|p, n| unsafe {
                ffi::get_text(h, p, n)
            })))
        }),
        ("setText", 2, |c, _, a, _| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::set_text(h, s.as_ptr(), s.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("getAttr", 2, |c, _, a, _| {
            let (h, name) = (int(a, 0, c)?, text(a, 1, c)?);
            Ok(opt_string(c, read_string(|p, n| unsafe {
                ffi::get_attr(h, name.as_ptr(), name.len(), p, n)
            })))
        }),
        ("sheetText", 1, |c, _, a, _| {
            let h = int(a, 0, c)?;
            Ok(opt_string(c, read_string(|p, n| unsafe {
                ffi::sheet_text(h, p, n)
            })))
        }),
        ("setSheet", 2, |c, _, a, _| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::set_sheet(h, s.as_ptr(), s.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("canvas", 2, |c, _, a, _| {
            let (h, cmd) = (int(a, 0, c)?, text(a, 1, c)?);
            Ok(opt_string(c, read_string(|p, n| unsafe {
                ffi::canvas(h, cmd.as_ptr(), cmd.len(), p, n)
            })))
        }),
        ("media", 2, |c, _, a, _| {
            let (h, cmd) = (int(a, 0, c)?, text(a, 1, c)?);
            Ok(opt_string(c, read_string(|p, n| unsafe {
                ffi::media(h, cmd.as_ptr(), cmd.len(), p, n)
            })))
        }),
        ("mediaMatches", 1, |c, _, a, _| {
            let q = text(a, 0, c)?;
            Ok((unsafe { ffi::media_matches(q.as_ptr(), q.len()) } != 0).into())
        }),
        ("cssSupports", 1, |c, _, a, _| {
            let q = text(a, 0, c)?;
            Ok((unsafe { ffi::css_supports(q.as_ptr(), q.len()) } != 0).into())
        }),
        ("computedStyle", 2, |c, _, a, _| {
            let (h, name) = (int(a, 0, c)?, text(a, 1, c)?);
            Ok(opt_string(c, read_string(|p, n| unsafe {
                ffi::computed_style(h, name.as_ptr(), name.len(), p, n)
            })))
        }),
        ("setAttr", 3, |c, _, a, _| {
            let (h, name, v) = (int(a, 0, c)?, text(a, 1, c)?, text(a, 2, c)?);
            unsafe { ffi::set_attr(h, name.as_ptr(), name.len(), v.as_ptr(), v.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("removeAttr", 2, |c, _, a, _| {
            let (h, name) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::remove_attr(h, name.as_ptr(), name.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("attrNames", 1, |c, _, a, _| {
            let h = int(a, 0, c)?;
            let n = unsafe { ffi::attr_count(h) }.max(0);
            let names: Vec<Value> = (0..n)
                .filter_map(|i| read_string(|p, cap| unsafe { ffi::attr_name_at(h, i, p, cap) }))
                .map(|s| string(c, &s))
                .collect();
            Ok(array(c, names))
        }),
        ("classOp", 3, |c, _, a, _| {
            let (h, op, s) = (int(a, 0, c)?, int(a, 1, c)?, text(a, 2, c)?);
            Ok((unsafe { ffi::class_op(h, op, s.as_ptr(), s.len()) } != 0).into())
        }),
        ("setStyle", 3, |c, _, a, _| {
            let (h, p, v) = (int(a, 0, c)?, text(a, 1, c)?, text(a, 2, c)?);
            unsafe { ffi::set_style(h, p.as_ptr(), p.len(), v.as_ptr(), v.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("getValue", 1, |c, _, a, _| {
            let h = int(a, 0, c)?;
            Ok(string(c, 
                &read_string(|p, n| unsafe { ffi::get_value(h, p, n) }).unwrap_or_default(),
            ))
        }),
        ("setValue", 2, |c, _, a, _| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::set_value(h, s.as_ptr(), s.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("getHtml", 2, |c, _, a, _| {
            let (h, outer) = (int(a, 0, c)?, int(a, 1, c)?);
            Ok(string(c, 
                &read_string(|p, n| unsafe { ffi::get_html(h, outer, p, n) }).unwrap_or_default(),
            ))
        }),
        ("setHtml", 2, |c, _, a, _| {
            let (h, s) = (int(a, 0, c)?, text(a, 1, c)?);
            unsafe { ffi::set_html(h, s.as_ptr(), s.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("createElement", 1, |c, _, a, _| {
            let s = text(a, 0, c)?;
            Ok(unsafe { ffi::create_element(s.as_ptr(), s.len()) }.into())
        }),
        ("createText", 1, |c, _, a, _| {
            let s = text(a, 0, c)?;
            Ok(unsafe { ffi::create_text(s.as_ptr(), s.len()) }.into())
        }),
        ("insert", 3, |c, _, a, _| {
            let (p, ch, b) = (int(a, 0, c)?, int(a, 1, c)?, int(a, 2, c)?);
            Ok((unsafe { ffi::insert(p, ch, b) } != 0).into())
        }),
        ("remove", 1, |c, _, a, _| {
            unsafe { ffi::remove(int(a, 0, c)?) };
            Ok(Value::UNDEFINED)
        }),
        ("listen", 3, |c, _, a, _| {
            let (h, t, id) = (int(a, 0, c)?, text(a, 1, c)?, int(a, 2, c)?);
            unsafe { ffi::listen(h, t.as_ptr(), t.len(), id) };
            Ok(Value::UNDEFINED)
        }),
        ("unlisten", 3, |c, _, a, _| {
            let (h, t, id) = (int(a, 0, c)?, text(a, 1, c)?, int(a, 2, c)?);
            unsafe { ffi::unlisten(h, t.as_ptr(), t.len(), id) };
            Ok(Value::UNDEFINED)
        }),
        ("preventDefault", 0, |_, _, _, _| {
            unsafe { ffi::prevent_default() };
            Ok(Value::UNDEFINED)
        }),
        ("stopPropagation", 0, |_, _, _, _| {
            unsafe { ffi::stop_propagation() };
            Ok(Value::UNDEFINED)
        }),
        ("setTimeout", 2, |c, _, a, _| {
            let (ms, id) = (num(a, 0, c)?, int(a, 1, c)?);
            unsafe { ffi::set_timeout(ms, id) };
            Ok(Value::UNDEFINED)
        }),
        ("clearTimeout", 1, |c, _, a, _| {
            unsafe { ffi::clear_timeout(int(a, 0, c)?) };
            Ok(Value::UNDEFINED)
        }),
        ("navigate", 1, |c, _, a, _| {
            let s = text(a, 0, c)?;
            unsafe { ffi::navigate(s.as_ptr(), s.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("log", 1, |c, _, a, _| {
            let s = text(a, 0, c)?;
            unsafe { ffi::log(s.as_ptr(), s.len()) };
            Ok(Value::UNDEFINED)
        }),
        ("now", 0, |_, _, _, _| Ok(unsafe { ffi::now() }.into())),
        // Time zones for Intl: the host's IANA name, a zone's offset in
        // minutes at a time (NaN: unknown zone), the names the browser knows.
        ("tzLocal", 0, |c, _, _, _| {
            Ok(string(c, &read_string(|p, n| unsafe { ffi::tz_local(p, n) }).unwrap_or_default()))
        }),
        ("tzOffset", 2, |c, _, a, _| {
            let name = text(a, 0, c)?;
            let ms = num(a, 1, c)?;
            Ok(unsafe { ffi::tz_offset(name.as_ptr(), name.len(), ms) }.into())
        }),
        ("tzNames", 0, |c, _, _, _| {
            Ok(string(c, &read_string(|p, n| unsafe { ffi::tz_names(p, n) }).unwrap_or_default()))
        }),
        ("location", 0, |c, _, _, _| {
            Ok(string(c, 
                &read_string(|p, n| unsafe { ffi::location(p, n) }).unwrap_or_default(),
            ))
        }),
        ("languages", 0, |c, _, _, _| {
            Ok(string(c, 
                &read_string(|p, n| unsafe { ffi::languages(p, n) }).unwrap_or_default(),
            ))
        }),
        ("historyGo", 1, |c, _, a, _| {
            unsafe { ffi::history_go(int(a, 0, c)?) };
            Ok(Value::UNDEFINED)
        }),
        ("eventString", 1, |c, _, a, _| {
            let w = int(a, 0, c)?;
            Ok(string(c, 
                &read_string(|p, n| unsafe { ffi::event_string(w, p, n) }).unwrap_or_default(),
            ))
        }),
        ("eventInt", 1, |c, _, a, _| {
            Ok(unsafe { ffi::event_int(int(a, 0, c)?) }.into())
        }),
        ("getChecked", 1, |c, _, a, _| {
            Ok((unsafe { ffi::get_checked(int(a, 0, c)?) } != 0).into())
        }),
        ("setChecked", 2, |c, _, a, _| {
            let on = c.to_boolean(arg(a, 1));
            unsafe { ffi::set_checked(int(a, 0, c)?, on as i32) };
            Ok(Value::UNDEFINED)
        }),
        ("getSelected", 1, |c, _, a, _| {
            Ok(unsafe { ffi::get_selected(int(a, 0, c)?) }.into())
        }),
        ("setSelected", 2, |c, _, a, _| {
            unsafe { ffi::set_selected(int(a, 0, c)?, int(a, 1, c)?) };
            Ok(Value::UNDEFINED)
        }),
        ("formAction", 2, |c, _, a, _| {
            unsafe { ffi::form_action(int(a, 0, c)?, int(a, 1, c)?) };
            Ok(Value::UNDEFINED)
        }),
        ("eventPos", 1, |c, _, a, _| {
            Ok(unsafe { ffi::event_pos(int(a, 0, c)?) }.into())
        }),
    ]
}


fn log(msg: &str) {
    unsafe { ffi::log(msg.as_ptr(), msg.len()) };
}

/// Reports an uncaught exception through the prelude (for stack traces),
/// or directly if that fails.
fn report(err: Throw, vm: &mut Vm) {
    let value = match err {
        Throw::Value(v) => v,
        // A termination (the browser stopped the script) is reported by
        // the browser.
        Throw::Terminate => return,
    };
    let k = vm.key("__rb_report");
    let g = vm.global;
    let reporter = vm.get(g, k).ok().filter(|f| vm.is_callable(*f));
    let reported = reporter.is_some_and(|f| vm.call(f, Value::UNDEFINED, &[value]).is_ok());
    if !reported {
        let s = vm.error_to_string(value);
        log(&format!("Uncaught {s}"));
    }
}

fn with_vm(f: impl FnOnce(&mut Vm)) {
    // No borrow is held while scripts run: when the browser stops a script
    // that runs too long, the call never returns, and the next call must
    // still find the VM.
    let vm = VM.with(|v| v.borrow_mut().as_deref_mut().map(|vm| vm as *mut Vm));
    if let Some(vm) = vm {
        // SAFETY: the VM is boxed (a stable address) and only reached from
        // the runtime's exports, which the browser never calls while
        // another is running.
        let vm = unsafe { &mut *vm };
        if RUNNING.with(|r| r.replace(true)) {
            // The last call was stopped: drop its frames and what it left
            // queued (a chain of promise reactions that never ends,
            // typically), as when a browser stops an unresponsive script.
            vm.recover();
            vm.jobs.clear();
        }
        f(vm);
        if let Err(e) = vm.run_jobs() {
            report(e, vm);
        }
        RUNNING.with(|r| r.set(false));
    }
}

/// Calls the global function `name` (event, timer, network dispatch).
fn call_global(name: &str, args: &[Value]) {
    with_vm(|vm| {
        let k = vm.key(name);
        let g = vm.global;
        if let Ok(f) = vm.get(g, k)
            && vm.is_callable(f)
            && let Err(e) = vm.call(f, Value::UNDEFINED, args)
        {
            report(e, vm);
        }
    });
}

/// Evaluates the module at `url` (its graph was provided already).
fn run_module(url: &str, vm: &mut Vm) {
    let p = vm.import_module(url, None);
    let r = vm.run_jobs();
    let err = match r {
        Err(e) => Some(e),
        Ok(()) => match rujse::builtins::promise::state_of(vm, p) {
            Some((rujse::builtins::promise::State::Rejected, e)) => Some(Throw::Value(e)),
            _ => None,
        },
    };
    if let Some(e) = err {
        report(e, vm);
        log(&format!("  in module {url}"));
    }
}

/// Calls the `__rb` function stored at the native's index.
fn dispatch(vm: &mut Vm, this: Value, args: &[Value], nt: Value) -> JsResult<Value> {
    let i = vm.native_data(0).as_int().unwrap_or(0) as usize;
    if SNAPSHOTTING.with(Cell::get) {
        // Making the snapshot: no browser. The document is node 0.
        let name = NAMES.with(|n| n.borrow()[i]);
        return Ok(if name == "nodeType" { Value::int(9) } else { Value::UNDEFINED });
    }
    let f = NATIVES.with(|n| n.borrow()[i]);
    // Every call into the browser is also a point to stop a script the
    // browser interrupted.
    if unsafe { ffi::interrupted() } != 0 {
        return Err(Throw::Terminate);
    }
    f(vm, this, args, nt)
}

/// The engine with the browser's natives (`__rb`), before any script:
/// `init` and `make_snapshot` must build it identically (the snapshot
/// refers to the natives by creation order).
fn setup_vm() -> Box<Vm> {
    let mut vm = Vm::new();
    vm.host = Some(Box::new(Host));
    let rb = vm.new_object();
    let list = natives();
    for (i, (name, argc, _)) in list.iter().enumerate() {
        let f = vm.native_fn_data(name, *argc as u32, dispatch, false, Box::new([Value::int(i as i32)]));
        let k = vm.key(name);
        vm.define_data(rb, k, Value::object(f), rujse::object::attr::HIDDEN);
    }
    // DevTools: engine state scripts cannot see (Runtime.getProperties)
    // and the debugger (see prelude.js, __rb_debug_*).
    let devtools: [(&str, u32, Native); 7] = [
        ("internal", 1, internal_fn),
        ("debugEnable", 0, debug_enable_fn),
        ("debugDisable", 0, debug_disable_fn),
        ("debugSetBreakpoint", 4, debug_set_breakpoint_fn),
        ("debugCall", 2, debug_call_fn),
        ("debugEval", 2, debug_eval_fn),
        ("debugSend", 1, debug_send_fn),
    ];
    for (name, argc, f) in devtools {
        let f = vm.native_fn(name, argc, f);
        let k = vm.key(name);
        vm.define_data(rb, k, Value::object(f), rujse::object::attr::HIDDEN);
    }
    NATIVES.with(|n| *n.borrow_mut() = list.iter().map(|(_, _, f)| *f).collect());
    NAMES.with(|n| *n.borrow_mut() = list.iter().map(|(name, _, _)| *name).collect());
    vm.global_prop("__rb", Value::object(rb));
    vm
}

/// `__rb.internal(v)`: [[name, value], …] from rujse's inspector
/// (`[[PromiseState]]`, `[[Entries]]`, `[[FunctionLocation]]`, …).
fn internal_fn(vm: &mut Vm, _: Value, args: &[Value], _: Value) -> JsResult<Value> {
    let props = vm.internal_properties(arg(args, 0));
    let mut out = Vec::new();
    for (name, v) in props {
        let n = vm.new_string(name);
        out.push(Value::object(vm.new_array(vec![n, v])));
    }
    Ok(Value::object(vm.new_array(out)))
}

// ----- the debugger -----------------------------------------------------------

thread_local! {
    /// Frames on the VM stack when it paused: the debugger's own calls
    /// into JS run above them.
    static PAUSED_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Pauses for CDP: reports through prelude.js (`__rb_debug_paused`), then
/// handles commands (`__rb_debug_command`) until one resumes.
struct CdpDebugger;

fn json_str(s: &str) -> String {
    let mut o = String::from('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn send(text: &str) {
    unsafe { ffi::debug_send(text.as_ptr(), text.len()) };
}

fn call_js(vm: &mut Vm, name: &str, args: &[Value]) -> Option<Value> {
    let k = vm.key(name);
    let g = vm.global;
    let f = vm.get(g, k).ok().filter(|f| vm.is_callable(*f))?;
    vm.call(f, Value::UNDEFINED, args).ok()
}

/// The call stack as JS objects for prelude.js: function name, script,
/// 1-based position, `this` and the scope chain (variables copied into
/// objects; `with` and global scopes are their objects).
fn frames_value(vm: &mut Vm) -> Value {
    let frames = vm.debug_frames();
    let mut out = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let o = vm.new_object();
        let put = |vm: &mut Vm, k: &str, v: Value| {
            let key = vm.key(k);
            vm.define_data(o, key, v, rujse::object::attr::DEFAULT);
        };
        let v = vm.new_string(&f.function_name);
        put(vm, "functionName", v);
        let v = vm.new_string(&f.url);
        put(vm, "url", v);
        put(vm, "scriptId", Value::from(f.script_id as f64));
        put(vm, "line", Value::from(f.line as f64));
        put(vm, "column", Value::from(f.column as f64));
        put(vm, "this", f.this);
        let mut scopes = Vec::new();
        for s in vm.debug_scopes(i) {
            let so = vm.new_object();
            let kind = match s.kind {
                rujse::debug::ScopeType::Local => "local",
                rujse::debug::ScopeType::Closure => "closure",
                rujse::debug::ScopeType::With => "with",
                rujse::debug::ScopeType::Global => "global",
            };
            let object = match s.object {
                Some(obj) => obj,
                None => {
                    let vars = vm.new_object();
                    for (n, v) in &s.variables {
                        if let Some(v) = v {
                            let k = vm.key(n);
                            vm.define_data(vars, k, *v, rujse::object::attr::DEFAULT);
                        }
                    }
                    Value::object(vars)
                }
            };
            let kt = vm.key("type");
            let tv = vm.new_string(kind);
            vm.define_data(so, kt, tv, rujse::object::attr::DEFAULT);
            let ko = vm.key("object");
            vm.define_data(so, ko, object, rujse::object::attr::DEFAULT);
            scopes.push(Value::object(so));
        }
        let sv = Value::object(vm.new_array(scopes));
        put(vm, "scopes", sv);
        out.push(Value::object(o));
    }
    Value::object(vm.new_array(out))
}

impl rujse::debug::DebugHandler for CdpDebugger {
    fn paused(&mut self, vm: &mut Vm, reason: &rujse::debug::PauseReason) -> rujse::debug::Resume {
        use rujse::debug::{PauseReason, Resume};
        PAUSED_DEPTH.with(|d| d.set(vm.frames.len()));
        let (why, data, hit) = match reason {
            PauseReason::Breakpoint(id) => ("other", Value::UNDEFINED, Value::from(*id as f64)),
            PauseReason::Exception(v) => ("exception", *v, Value::UNDEFINED),
            PauseReason::DebuggerStatement => ("debugCommand", Value::UNDEFINED, Value::UNDEFINED),
            PauseReason::Step | PauseReason::Request => ("other", Value::UNDEFINED, Value::UNDEFINED),
        };
        let frames = frames_value(vm);
        let w = vm.new_string(why);
        call_js(vm, "__rb_debug_paused", &[w, data, frames, hit]);
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = unsafe { ffi::debug_wait(buf.as_mut_ptr(), buf.len()) };
            if n < 0 {
                break;
            }
            let text = String::from_utf8_lossy(&buf[..(n as usize).min(buf.len())]).into_owned();
            let t = vm.new_string(&text);
            let action = call_js(vm, "__rb_debug_command", &[t]).map(|v| vm.display(v)).unwrap_or_default();
            let resume = match action.as_str() {
                "resume" => Resume::Continue,
                "stepOver" => Resume::StepOver,
                "stepInto" => Resume::StepInto,
                "stepOut" => Resume::StepOut,
                _ => continue,
            };
            unsafe { ffi::debug_resumed() };
            return resume;
        }
        unsafe { ffi::debug_resumed() };
        rujse::debug::Resume::Continue
    }

    fn script_parsed(&mut self, _vm: &mut Vm, s: &rujse::debug::ScriptInfo) {
        let lines = s.source.lines().count();
        send(&format!(
            r#"{{"method":"Debugger.scriptParsed","params":{{"scriptId":"{}","url":{},"startLine":0,"startColumn":0,"endLine":{lines},"endColumn":0,"executionContextId":1,"hash":""}}}}"#,
            s.id,
            json_str(&s.url)
        ));
    }
}

fn debug_enable_fn(vm: &mut Vm, _: Value, _: &[Value], _: Value) -> JsResult<Value> {
    if vm.debugger.is_none() {
        vm.enable_debugger(Box::new(CdpDebugger));
    }
    Ok(Value::UNDEFINED)
}

fn debug_disable_fn(vm: &mut Vm, _: Value, _: &[Value], _: Value) -> JsResult<Value> {
    vm.disable_debugger();
    Ok(Value::UNDEFINED)
}

/// (url, line, column or null, condition) → [id, [[scriptId, line, column], …]]
/// (1-based positions).
fn debug_set_breakpoint_fn(vm: &mut Vm, _: Value, a: &[Value], _: Value) -> JsResult<Value> {
    let url = text(a, 0, vm)?;
    let line = int(a, 1, vm)? as u32;
    let col = if arg(a, 2).is_nullish() { None } else { Some(int(a, 2, vm)? as u32) };
    let cond = text(a, 3, vm)?;
    let cond = if cond.is_empty() { None } else { Some(cond) };
    let Some(id) = vm.set_breakpoint(&url, line, col, cond.as_deref()) else { return Ok(Value::NULL) };
    let mut locs = Vec::new();
    for (s, l, c) in vm.breakpoint_locations(id) {
        let item = vec![Value::from(s as f64), Value::from(l as f64), Value::from(c as f64)];
        locs.push(Value::object(vm.new_array(item)));
    }
    let locs = Value::object(vm.new_array(locs));
    Ok(Value::object(vm.new_array(vec![Value::from(id as f64), locs])))
}

/// (what, argument): removeBreakpoint, setPauseOnExceptions,
/// getScriptSource, pause.
fn debug_call_fn(vm: &mut Vm, _: Value, a: &[Value], _: Value) -> JsResult<Value> {
    let what = text(a, 0, vm)?;
    let x = text(a, 1, vm)?;
    Ok(match what.as_str() {
        "Debugger.removeBreakpoint" => {
            if let Ok(id) = x.parse() {
                vm.remove_breakpoint(id);
            }
            Value::UNDEFINED
        }
        "Debugger.setPauseOnExceptions" => {
            use rujse::debug::PauseOnExceptions as P;
            vm.set_pause_on_exceptions(match x.as_str() {
                "all" => P::All,
                "uncaught" => P::Uncaught,
                _ => P::None,
            });
            Value::UNDEFINED
        }
        "Debugger.getScriptSource" => match x.parse().ok().and_then(|id| vm.debug_script_source(id)) {
            Some(src) => vm.new_string(&src),
            None => Value::NULL,
        },
        "Debugger.pause" => {
            vm.debug_request_pause();
            Value::UNDEFINED
        }
        _ => Value::UNDEFINED,
    })
}

/// (frame index from the paused stack's top, source) → value.
fn debug_eval_fn(vm: &mut Vm, _: Value, a: &[Value], _: Value) -> JsResult<Value> {
    let index = int(a, 0, vm)?.max(0) as usize;
    let src = text(a, 1, vm)?;
    // The debugger's own frames (this call's caller) are above the paused
    // ones.
    let extra = vm.frames.len().saturating_sub(PAUSED_DEPTH.with(Cell::get));
    vm.debug_evaluate(index + extra, &src)
}

fn debug_send_fn(vm: &mut Vm, _: Value, a: &[Value], _: Value) -> JsResult<Value> {
    let t = text(a, 0, vm)?;
    send(&t);
    Ok(Value::UNDEFINED)
}

fn run_prelude(vm: &mut Vm) -> JsResult<Value> {
    vm.eval_script(PRELUDE, "prelude.js")
}

/// `rb_init`: creates the engine and the browser environment.
pub fn init() {
    // A panic ends the runtime (a trap); its message goes to the console.
    std::panic::set_hook(Box::new(|info| log(&format!("JavaScript runtime panicked: {info}"))));
    let mut vm = setup_vm();
    // The heap as the prelude leaves it, saved at build time: no prelude
    // to run. Used only when made from this very prelude.
    let restored = Vm::snapshot_source(PRELUDE_SNAPSHOT, "prelude.js") == Some(PRELUDE) && vm.restore(PRELUDE_SNAPSHOT).is_ok();
    if !restored {
        if let Err(e) = run_prelude(&mut vm) {
            let msg = match e {
                Throw::Value(v) => vm.error_to_string(v),
                Throw::Terminate => "terminated".into(),
            };
            log(&format!("prelude failed: {msg}"));
        }
    }
    // Math.random: seeded from the browser clock (not for cryptography);
    // after the restore, so pages do not share a sequence.
    vm.rng = (unsafe { ffi::wall_time() } as u64) ^ 0x9E37_79B9_7F4A_7C15 | 1;
    VM.with(|v| *v.borrow_mut() = Some(vm));
}

/// Builds `prelude.rjss`, the heap after the prelude (scripts/build.sh runs
/// this on the build machine). The prelude only asks the browser for the
/// document's node type while it loads; nothing else is called.
pub fn make_snapshot() -> Result<Vec<u8>, String> {
    SNAPSHOTTING.with(|s| s.set(true));
    let mut vm = setup_vm();
    // No browser behind the host either (time, interrupts).
    vm.host = None;
    let r = run_prelude(&mut vm).and_then(|_| vm.run_jobs());
    SNAPSHOTTING.with(|s| s.set(false));
    if let Err(e) = r {
        return Err(match e {
            Throw::Value(v) => vm.error_to_string(v),
            Throw::Terminate => "terminated".into(),
        });
    }
    vm.snapshot()
}

/// `rb_alloc`: a buffer for the browser to write source code into.
pub fn alloc(len: usize) -> *mut u8 {
    BUFFERS.with(|b| {
        let mut b = b.borrow_mut();
        b.push(vec![0; len]);
        b.last_mut().map_or(std::ptr::null_mut(), |v| v.as_mut_ptr())
    })
}

/// `rb_eval`: runs a script.
///
/// # Safety
///
/// `src..src+len` and `name..name+name_len` must be readable (buffers from
/// [`alloc`]).
pub unsafe fn eval(src: *const u8, len: usize, name: *const u8, name_len: usize) {
    let (source, name) = unsafe {
        (
            String::from_utf8_lossy(std::slice::from_raw_parts(src, len)).into_owned(),
            String::from_utf8_lossy(std::slice::from_raw_parts(name, name_len)).into_owned(),
        )
    };
    BUFFERS.with(|b| b.borrow_mut().clear());
    // Module protocol (see doc/javascript.md): the name says what to do.
    if let Some(url) = name.strip_prefix("rb-define-module:") {
        MODULES.with(|m| m.borrow_mut().sources.insert(url.to_string(), source));
        return;
    }
    if name == "rb-import-map" {
        let map: Vec<(String, String)> = source
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        MODULES.with(|m| m.borrow_mut().import_map = map);
        return;
    }
    if let Some(url) = name.strip_prefix("rb-module:") {
        let url = url.to_string();
        with_vm(|vm| run_module(&url, vm));
        return;
    }
    with_vm(|vm| {
        // The name shows in stack traces.
        if let Err(e) = vm.eval_script(&source, &name) {
            report(e, vm);
        }
    });
}

/// `rb_http`: HTTP request `id` finished with `status` (0: failed).
pub fn http(id: i32, status: i32) {
    call_global("__rb_http", &[Value::int(id), Value::int(status)]);
}

/// `rb_ws`: something happened on WebSocket `id` (see `Script::ws_event`).
pub fn ws(id: i32, kind: i32) {
    call_global("__rb_ws", &[Value::int(id), Value::int(kind)]);
}

/// `rb_event`: runs listener `id`.
pub fn event(id: i32) {
    call_global("__rb_dispatch", &[Value::int(id)]);
}

/// `rb_timer`: runs timer `id`.
pub fn timer(id: i32) {
    call_global("__rb_timer", &[Value::int(id)]);
}

/// Defines the runtime's exports; used by `examples/js_runtime_rujse.rs`.
#[macro_export]
macro_rules! js_runtime_rujse {
    () => {
        #[unsafe(no_mangle)]
        pub extern "C" fn rb_init() {
            $crate::lang::js_rujse::init()
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_alloc(len: usize) -> *mut u8 {
            $crate::lang::js_rujse::alloc(len)
        }

        #[unsafe(no_mangle)]
        #[allow(clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn rb_eval(src: *const u8, len: usize, name: *const u8, name_len: usize) {
            // The browser passes buffers it got from rb_alloc.
            unsafe { $crate::lang::js_rujse::eval(src, len, name, name_len) }
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_event(id: i32) {
            $crate::lang::js_rujse::event(id)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_timer(id: i32) {
            $crate::lang::js_rujse::timer(id)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_http(id: i32, status: i32) {
            $crate::lang::js_rujse::http(id, status)
        }

        #[unsafe(no_mangle)]
        pub extern "C" fn rb_ws(id: i32, kind: i32) {
            $crate::lang::js_rujse::ws(id, kind)
        }
    };
}

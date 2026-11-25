//! Cookies as `document.cookie` sees them (RFC 6265 §5.4 / HTML's
//! "cookie-averse" rules): the platform installs the jar its HTTP client
//! uses, so scripts and servers share one set of cookies. Without one (the
//! web build, tests), an in-memory jar keeps the page's own cookies.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A cookie jar shared with the HTTP client.
pub trait CookieJar: Send + Sync {
    /// `name=value; …` of the cookies a request to `url` would send,
    /// without HttpOnly ones.
    fn document_cookie(&self, url: &str) -> String;
    /// Stores a cookie set by a script (`document.cookie = value`) for
    /// `url`; HttpOnly cookies cannot be set or replaced this way.
    fn set_document_cookie(&self, url: &str, value: &str);
}

static JAR: OnceLock<Box<dyn CookieJar>> = OnceLock::new();

/// Installs the platform's jar (once, before pages run).
pub fn install(jar: Box<dyn CookieJar>) {
    let _ = JAR.set(jar);
}

/// The jar in use.
pub fn jar() -> &'static dyn CookieJar {
    JAR.get_or_init(|| Box::new(MemoryJar::default())).as_ref()
}

/// Whether `url` can have cookies (HTTP(S) only, as in browsers).
pub fn cookie_url(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Cookies kept in memory per host (name, value), without paths,
/// expiry or attributes beyond `Max-Age=0` / a past `Expires`.
#[derive(Default)]
pub struct MemoryJar(Mutex<HashMap<String, Vec<(String, String)>>>);

fn host_of(url: &str) -> String {
    url.split_once("://")
        .map_or("", |(_, r)| r)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

impl CookieJar for MemoryJar {
    fn document_cookie(&self, url: &str) -> String {
        let jar = self.0.lock().unwrap_or_else(|e| e.into_inner());
        jar.get(&host_of(url))
            .map(|c| c.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("; "))
            .unwrap_or_default()
    }

    fn set_document_cookie(&self, url: &str, value: &str) {
        let mut parts = value.split(';');
        let pair = parts.next().unwrap_or("").trim();
        let (name, val) = pair.split_once('=').unwrap_or(("", pair));
        let (name, val) = (name.trim().to_string(), val.trim().to_string());
        let expired = parts.any(|a| {
            let a = a.trim().to_ascii_lowercase();
            a == "max-age=0" || a.starts_with("max-age=-") || a.contains("expires=thu, 01 jan 1970")
        });
        let mut jar = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let list = jar.entry(host_of(url)).or_default();
        list.retain(|(n, _)| *n != name);
        if !expired {
            list.push((name, val));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_jar() {
        let j = MemoryJar::default();
        j.set_document_cookie("https://a.test/x", "a=1; path=/");
        j.set_document_cookie("https://a.test/", "b=2");
        j.set_document_cookie("https://b.test/", "c=3");
        assert_eq!(j.document_cookie("https://a.test/y"), "a=1; b=2");
        j.set_document_cookie("https://a.test/", "a=; Max-Age=0");
        assert_eq!(j.document_cookie("https://a.test/"), "b=2");
        assert!(cookie_url("http://x.test/") && !cookie_url("file:///x"));
    }
}

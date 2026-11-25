//! Web Storage (`localStorage`, `sessionStorage`) kept by the browser:
//! local storage per origin (saved with the profile through the host),
//! session storage per tab and origin (it survives reloads and
//! navigation in the tab).

use std::cell::RefCell;
use std::rc::Rc;

/// Most UTF-16 code units of keys and values one area holds (5 MB, as in
/// other browsers).
pub const QUOTA: usize = 5_000_000;

/// One storage area: its items in insertion order.
#[derive(Debug, Default)]
pub struct Area {
    items: Vec<(String, String)>,
    /// Changed since it was last saved.
    pub dirty: bool,
}

pub type Shared = Rc<RefCell<Area>>;

fn units(s: &str) -> usize {
    s.encode_utf16().count()
}

impl Area {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.items.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    /// Sets `key`; `false` when that would exceed [`QUOTA`].
    pub fn set(&mut self, key: &str, value: &str) -> bool {
        let used: usize = self
            .items
            .iter()
            .filter(|(k, _)| k != key)
            .map(|(k, v)| units(k) + units(v))
            .sum();
        if used + units(key) + units(value) > QUOTA {
            return false;
        }
        match self.items.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) if v == value => return true,
            Some((_, v)) => *v = value.to_string(),
            None => self.items.push((key.to_string(), value.to_string())),
        }
        self.dirty = true;
        true
    }

    pub fn remove(&mut self, key: &str) {
        let n = self.items.len();
        self.items.retain(|(k, _)| k != key);
        self.dirty |= self.items.len() != n;
    }

    pub fn clear(&mut self) {
        self.dirty |= !self.items.is_empty();
        self.items.clear();
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn key(&self, i: usize) -> Option<&str> {
        self.items.get(i).map(|(k, _)| k.as_str())
    }

    /// The items as text: one `key<TAB>value` line each, with `%`, tab,
    /// CR and LF percent-encoded.
    pub fn encode(&self) -> String {
        let esc = |s: &str| {
            s.replace('%', "%25")
                .replace('\t', "%09")
                .replace('\n', "%0A")
                .replace('\r', "%0D")
        };
        self.items
            .iter()
            .map(|(k, v)| format!("{}\t{}\n", esc(k), esc(v)))
            .collect()
    }

    /// Reads [`Area::encode`]'s text.
    pub fn decode(text: &str) -> Area {
        let unesc = |s: &str| String::from_utf8_lossy(&super::url::percent_decode(s)).into_owned();
        let items = text
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(k, v)| (unesc(k), unesc(v)))
            .collect();
        Area { items, dirty: false }
    }
}

/// The storage key of a document URL: its origin (`scheme://host[:port]`),
/// `file://` for files, the scheme for others.
pub fn origin_key(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) if scheme == "http" || scheme == "https" => {
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            format!("{scheme}://{}", host.to_ascii_lowercase())
        }
        Some((scheme, _)) => format!("{scheme}://"),
        None => url.split(':').next().unwrap_or("").to_string() + ":",
    }
}

/// A file name for an origin's saved local storage.
pub fn file_name(origin: &str) -> String {
    let safe: String = origin
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    format!("{safe}.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas() {
        let mut a = Area::default();
        assert!(a.set("k", "v\tw\n%"));
        assert!(a.set("x", "1"));
        assert!(a.set("k", "2"));
        assert_eq!(a.key(0), Some("k"));
        assert_eq!(a.get("k"), Some("2"));
        a.set("k", "a\tb\nc%d");
        let back = Area::decode(&a.encode());
        assert_eq!(back.get("k"), Some("a\tb\nc%d"));
        assert_eq!(back.len(), 2);
        assert!(!a.set("big", &"x".repeat(QUOTA)), "over the quota");
        a.remove("x");
        assert_eq!(a.len(), 1);
        assert_eq!(origin_key("https://A.test:8443/p?q"), "https://a.test:8443");
        assert_eq!(origin_key("file:///tmp/x.html"), "file://");
        assert_eq!(file_name("https://a.test:8443"), "https___a.test_8443.txt");
    }
}

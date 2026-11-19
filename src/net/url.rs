//! URL handling.
//!
//! Supported forms:
//! - `about:NAME`: pages built into the browser
//! - `http://…`, `https://…`
//! - `file://…` and plain paths (native only), relative to the working directory
//!
//! On the web version, plain paths are fetched relative to the HTML page.

/// Returns the scheme (`"https"`, `"about"`, …) if `s` starts with one.
pub fn scheme(s: &str) -> Option<&str> {
    let i = s.find(':')?;
    let sch = &s[..i];
    let valid = !sch.is_empty()
        && sch.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && sch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
    let known = matches!(sch, "about" | "blob" | "data" | "file" | "http" | "https");
    (valid && (known || s[i + 1..].starts_with("//"))).then_some(sch)
}

/// Turns what the user typed in the address bar into a URL.
pub fn from_input(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() || scheme(s).is_some() {
        return encode(s);
    }
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return s.to_string();
    }
    let host = s.split('/').next().unwrap_or("");
    let is_file_name = !s.contains('/') && s.ends_with(".wasm");
    if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
        return encode(&format!("http://{s}"));
    }
    if host.contains('.') && !is_file_name {
        return encode(&format!("https://{s}"));
    }
    s.to_string()
}

/// Percent-encodes what the URL Standard's parser encodes in an `http(s)`
/// URL: in the path, controls, space, `"`, `<`, `>`, `` ` ``, `{`, `}` and
/// non-ASCII (as UTF-8); in the query, controls, space, `"`, `'`, `<`, `>`
/// and non-ASCII; in the fragment, controls, space, `"`, `<`, `>`, `` ` ``
/// and non-ASCII. Existing `%XX` escapes are kept. Other schemes are
/// returned as they are.
pub fn encode(url: &str) -> String {
    if !matches!(scheme(url), Some("http" | "https")) || url.bytes().all(|b| b > 0x20 && b < 0x7f && !b"\"<>`{}'".contains(&b)) {
        return url.to_string();
    }
    let (rest, fragment) = match url.split_once('#') {
        Some((r, f)) => (r, Some(f)),
        None => (url, None),
    };
    let (path, query) = match rest.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (rest, None),
    };
    let esc = |s: &str, set: &[u8], out: &mut String| {
        for &b in s.as_bytes() {
            if b <= 0x20 || b >= 0x7f || set.contains(&b) {
                out.push_str(&format!("%{b:02X}"));
            } else {
                out.push(b as char);
            }
        }
    };
    let mut out = String::with_capacity(url.len() * 2);
    esc(path, b"\"<>`{}", &mut out);
    if let Some(q) = query {
        out.push('?');
        esc(q, b"\"'<>", &mut out);
    }
    if let Some(f) = fragment {
        out.push('#');
        esc(f, b"\"<>`", &mut out);
    }
    out
}

/// Resolves `link` (possibly relative) against the page at `base`, then
/// percent-encodes it ([`encode`]).
pub fn resolve(base: &str, link: &str) -> String {
    encode(&resolve_raw(base, link))
}

fn resolve_raw(base: &str, link: &str) -> String {
    let link = link.trim();
    if scheme(link).is_some() {
        return link.to_string();
    }
    // Same-document references keep the base path (and query for `#`).
    let no_fragment = base.split('#').next().unwrap_or(base);
    if link.is_empty() || link.starts_with('#') {
        return format!("{no_fragment}{link}");
    }
    if link.starts_with('?') {
        return format!("{}{link}", no_fragment.split('?').next().unwrap_or(""));
    }
    // Scheme-relative: //host/path
    if let (Some(rest), Some(sch)) = (link.strip_prefix("//"), scheme(base))
        && sch != "about"
    {
        return format!("{sch}://{rest}");
    }
    match scheme(base) {
        // Files next to a built-in page are built in too.
        Some("about") => format!("about:{}", link.trim_start_matches("./")),
        Some(_) => {
            // "scheme://authority" + path
            let after = base.find("//").map_or(0, |i| i + 2);
            let path_start = base[after..].find('/').map_or(base.len(), |i| after + i);
            let origin = &base[..path_start];
            let path = &base[path_start..];
            if link.starts_with('/') {
                format!("{origin}{}", normalize(link))
            } else {
                format!("{origin}{}", normalize(&join(path, link)))
            }
        }
        None => {
            if link.starts_with('/') {
                normalize(link)
            } else {
                normalize(&join(base, link))
            }
        }
    }
}

/// `dir(base) + link`.
fn join(base: &str, link: &str) -> String {
    let base = base.split(['?', '#']).next().unwrap_or("");
    match base.rfind('/') {
        Some(i) => format!("{}{link}", &base[..=i]),
        None if base.is_empty() => format!("/{link}"),
        None => link.to_string(),
    }
}

/// Removes `.` and `..` segments.
fn normalize(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i == segments.len() - 1;
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.last().is_some_and(|s| !s.is_empty() && *s != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
                if last {
                    out.push("");
                }
            }
            "" if i > 0 && !last => {}
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if absolute && !joined.starts_with('/') {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Decodes a `data:` URL (`data:[type][;base64],payload`).
pub fn decode_data_url(url: &str) -> Option<Vec<u8>> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    if meta.ends_with(";base64") {
        base64(payload)
    } else {
        Some(percent_decode(payload))
    }
}

pub(crate) fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&c| hex(c)),
                b.get(i + 2).and_then(|&c| hex(c)),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Short title for a URL: its last path segment.
pub fn title(url: &str) -> String {
    let u = url.split(['?', '#']).next().unwrap_or(url);
    let name = u.trim_end_matches('/').rsplit('/').next().unwrap_or(u);
    if name.is_empty() {
        url.to_string()
    } else {
        name.to_string()
    }
}

/// The serialized origin (`scheme://host[:port]`) of an HTTP(S) URL; None
/// for other schemes (opaque origins).
fn http_origin(u: &str) -> Option<String> {
    let (scheme, rest) = u.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next()?;
    // credentials are never sent
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h).to_ascii_lowercase();
    let default = if scheme == "http" { ":80" } else { ":443" };
    Some(format!("{scheme}://{}", host.strip_suffix(default).unwrap_or(&host)))
}

/// The `Origin` header of a request made by a document at `doc` (Fetch
/// §3.1): its origin, or `null` for documents without an HTTP(S) origin.
pub fn origin_header(doc: &str) -> String {
    http_origin(doc).unwrap_or_else(|| "null".into())
}

/// Whether `url` is potentially trustworthy (Secure Contexts §3.1): HTTPS,
/// or HTTP to the loopback (`localhost`, `*.localhost`, 127.0.0.0/8, ::1).
fn trustworthy(url: &str) -> bool {
    let Some(o) = http_origin(url) else { return false };
    if o.starts_with("https:") {
        return true;
    }
    let host = &o["http://".len()..];
    let host = if host.starts_with('[') { host.split(']').next().map_or("", |h| &h[1..]) } else { host.split(':').next().unwrap_or("") };
    host == "localhost" || host.ends_with(".localhost") || host == "::1" || host.starts_with("127.")
}

/// `Sec-Fetch-Site` (Fetch Metadata Request Headers §2.4) for a request from
/// a document at `doc` to `target`; empty when it is not sent (the target is
/// not potentially trustworthy). rubrowser has no Public Suffix List, so a
/// different origin is always `cross-site`, never `same-site` (the stricter
/// answer).
pub fn fetch_site(doc: &str, target: &str) -> String {
    if !trustworthy(target) {
        return String::new();
    }
    match (http_origin(doc), http_origin(target)) {
        (Some(a), Some(b)) if a == b => "same-origin".into(),
        _ => "cross-site".into(),
    }
}

/// The `Referer` for a request from a document at `doc` to `target`, under
/// the default referrer policy `strict-origin-when-cross-origin` (Referrer
/// Policy §3): the whole URL (without fragment) to the same origin, only
/// the origin elsewhere, nothing from HTTPS to HTTP or from non-HTTP pages.
pub fn referrer(doc: &str, target: &str) -> String {
    let (Some(from), Some(to)) = (http_origin(doc), http_origin(target)) else {
        return String::new();
    };
    if from.starts_with("https:") && to.starts_with("http:") {
        return String::new();
    }
    if from == to {
        let url = doc.split('#').next().unwrap_or(doc);
        // and no credentials in the full URL either
        return match url.split_once("://") {
            Some((s, rest)) => match rest.split_once('/') {
                Some((auth, path)) => format!("{s}://{}/{path}", auth.rsplit_once('@').map_or(auth, |(_, h)| h)),
                None => from + "/",
            },
            None => url.to_string(),
        };
    }
    from + "/"
}

#[cfg(test)]
mod tests {
    #[test]
    fn referrer_follows_strict_origin_when_cross_origin() {
        use super::referrer;
        assert_eq!(referrer("http://map.localhost:8700/a/b?q=1#x", "http://map.localhost:8700/c.png"), "http://map.localhost:8700/a/b?q=1");
        assert_eq!(referrer("http://map.localhost:8700/a/b", "https://tile.openstreetmap.org/1/2/3.png"), "http://map.localhost:8700/");
        assert_eq!(referrer("https://a.test/secret/page", "http://b.test/x"), "");
        assert_eq!(referrer("https://user:pw@a.test/p", "https://a.test/q"), "https://a.test/p");
        assert_eq!(referrer("file:///tmp/x.html", "https://b.test/"), "");
        assert_eq!(referrer("about:home", "https://b.test/"), "");
    }

    #[test]
    fn origin_and_fetch_site() {
        use super::{fetch_site, origin_header};
        assert_eq!(origin_header("http://notes.localhost:8700/a?b#c"), "http://notes.localhost:8700");
        assert_eq!(origin_header("https://a.test:443/x"), "https://a.test");
        assert_eq!(origin_header("file:///tmp/x.html"), "null");
        assert_eq!(fetch_site("http://files.localhost:8700/", "http://files.localhost:8700/delete"), "same-origin");
        assert_eq!(fetch_site("http://evil.localhost:8700/", "http://files.localhost:8700/delete"), "cross-site");
        assert_eq!(fetch_site("https://evil.test/", "http://127.0.0.1:8700/x"), "cross-site");
        assert_eq!(fetch_site("about:home", "https://a.test/"), "cross-site");
        assert_eq!(fetch_site("http://a.test/", "http://b.test/"), "", "not potentially trustworthy: not sent");
    }

    use super::*;

    #[test]
    fn non_ascii_is_percent_encoded() {
        assert_eq!(
            resolve("https://cccbook.github.io/cccbook2026/", "電腦/演算法/"),
            "https://cccbook.github.io/cccbook2026/%E9%9B%BB%E8%85%A6/%E6%BC%94%E7%AE%97%E6%B3%95/"
        );
        assert_eq!(resolve("https://a.test/", "/a b?q=x y&n='é'#f g"), "https://a.test/a%20b?q=x%20y&n=%27%C3%A9%27#f%20g");
        assert_eq!(resolve("https://a.test/", "/%E9%9B%BB?a=%20"), "https://a.test/%E9%9B%BB?a=%20", "escapes stay");
        assert_eq!(from_input("zh.wikipedia.org/wiki/電腦"), "https://zh.wikipedia.org/wiki/%E9%9B%BB%E8%85%A6");
        assert_eq!(resolve("about:home", "電腦"), "about:電腦");
    }

    #[test]
    fn schemes() {
        assert_eq!(scheme("https://a.com/x"), Some("https"));
        assert_eq!(scheme("about:home"), Some("about"));
        assert_eq!(scheme("localhost:8000/a.wasm"), None);
        assert_eq!(scheme("pages/a.wasm"), None);
        assert_eq!(scheme("C:/x"), None);
    }

    #[test]
    fn typed_input() {
        assert_eq!(
            from_input(" example.com/a.wasm "),
            "https://example.com/a.wasm"
        );
        assert_eq!(
            from_input("localhost:8000/a.wasm"),
            "http://localhost:8000/a.wasm"
        );
        assert_eq!(from_input("hello.wasm"), "hello.wasm");
        assert_eq!(from_input("pages/hello.wasm"), "pages/hello.wasm");
        assert_eq!(from_input("about:paint"), "about:paint");
        assert_eq!(from_input("/tmp/a.wasm"), "/tmp/a.wasm");
    }

    #[test]
    fn resolving() {
        let b = "https://a.com/dir/page.wasm";
        assert_eq!(resolve(b, "other.wasm"), "https://a.com/dir/other.wasm");
        assert_eq!(resolve(b, "../x.wasm"), "https://a.com/x.wasm");
        assert_eq!(resolve(b, "/root.wasm"), "https://a.com/root.wasm");
        assert_eq!(resolve(b, "about:home"), "about:home");
        assert_eq!(resolve("https://a.com", "x.wasm"), "https://a.com/x.wasm");
        assert_eq!(resolve("pages/a.wasm", "b.wasm"), "pages/b.wasm");
        assert_eq!(resolve("pages/a.wasm", "../b.wasm"), "b.wasm");
        assert_eq!(resolve("a.wasm", "b.wasm"), "b.wasm");
        assert_eq!(resolve("/x/y/a.wasm", "./b.wasm"), "/x/y/b.wasm");
        assert_eq!(
            resolve("about:counter", "counter.wasm"),
            "about:counter.wasm"
        );
    }

    #[test]
    fn data_urls() {
        assert_eq!(
            decode_data_url("data:text/plain;base64,SGVsbG8h").unwrap(),
            b"Hello!"
        );
        assert_eq!(decode_data_url("data:,a%20b").unwrap(), b"a b");
        assert_eq!(decode_data_url("data:;base64,YQ==").unwrap(), b"a");
        assert!(decode_data_url("nope").is_none());
        assert_eq!(scheme("data:image/png;base64,xx"), Some("data"));
        assert_eq!(resolve("https://a.com/x/", "data:,hi"), "data:,hi");
        assert_eq!(
            resolve("https://a.com/w/A_(b)#x", "#y"),
            "https://a.com/w/A_(b)#y"
        );
        assert_eq!(
            resolve("https://a.com/w/p?q=1#x", "?q=2"),
            "https://a.com/w/p?q=2"
        );
        assert_eq!(
            resolve("https://a.com/w/p?q=1", "#t"),
            "https://a.com/w/p?q=1#t"
        );
    }

    #[test]
    fn titles() {
        assert_eq!(title("https://a.com/dir/page.wasm"), "page.wasm");
        assert_eq!(title("about:home"), "about:home");
    }
}

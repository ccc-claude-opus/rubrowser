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
    let known = matches!(sch, "about" | "data" | "file" | "http" | "https");
    (valid && (known || s[i + 1..].starts_with("//"))).then_some(sch)
}

/// Turns what the user typed in the address bar into a URL.
pub fn from_input(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() || scheme(s).is_some() {
        return s.to_string();
    }
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return s.to_string();
    }
    let host = s.split('/').next().unwrap_or("");
    let is_file_name = !s.contains('/') && s.ends_with(".wasm");
    if host.starts_with("localhost") || host.starts_with("127.0.0.1") {
        return format!("http://{s}");
    }
    if host.contains('.') && !is_file_name {
        return format!("https://{s}");
    }
    s.to_string()
}

/// Resolves `link` (possibly relative) against the page at `base`.
pub fn resolve(base: &str, link: &str) -> String {
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
        if b[i] == b'%' {
            if let (Some(h), Some(l)) = (
                b.get(i + 1).and_then(|&c| hex(c)),
                b.get(i + 2).and_then(|&c| hex(c)),
            ) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

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

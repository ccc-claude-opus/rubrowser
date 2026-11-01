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
    let known = matches!(sch, "about" | "file" | "http" | "https");
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
    fn titles() {
        assert_eq!(title("https://a.com/dir/page.wasm"), "page.wasm");
        assert_eq!(title("about:home"), "about:home");
    }
}

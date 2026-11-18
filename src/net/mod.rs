//! Loading resources: URLs and fetching.

pub mod url;

/// The user's preferred languages (BCP 47 tags, most preferred first),
/// for `Accept-Language` and `navigator.languages`: from the locale
/// environment (`LC_ALL`, `LC_MESSAGES`, `LANG`, e.g. `zh_TW.UTF-8`) or,
/// on macOS, the system's preferred languages; then English.
pub fn languages() -> &'static [String] {
    static LANGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    LANGS.get_or_init(|| {
        let env = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty() && v != "C" && v != "POSIX");
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut found: Vec<String> = env.into_iter().collect();
        #[cfg(target_os = "macos")]
        if found.is_empty()
            && let Ok(out) = std::process::Command::new("defaults")
                .args(["read", "-g", "AppleLanguages"])
                .output()
        {
            // A property list array: ( "zh-Hant-TW", "en-US" ).
            found = String::from_utf8_lossy(&out.stdout)
                .split(['(', ')', ',', '\n'])
                .map(|s| s.trim().trim_matches('"').to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        let mut tags: Vec<String> = Vec::new();
        for raw in found {
            let tag = raw.split(['.', '@']).next().unwrap_or("").replace('_', "-");
            // Without the script when a region is given (zh-Hant-TW is zh-TW).
            let parts: Vec<&str> = tag.split('-').filter(|p| !p.is_empty()).collect();
            let tag = match parts[..] {
                [lang, script, region] if script.len() == 4 => format!("{lang}-{region}"),
                _ => parts.join("-"),
            };
            if tag.is_empty() {
                continue;
            }
            let primary = tag.split('-').next().unwrap_or("").to_string();
            for t in [tag, primary] {
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
        }
        for t in ["en-US", "en"] {
            if !tags.iter().any(|x| x == t) {
                tags.push(t.to_string());
            }
        }
        // English variants keep their place; en-US before plain en.
        tags
    })
}

/// `Accept-Language` for [`languages`], with descending weights.
pub fn accept_language() -> String {
    languages()
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i == 0 {
                t.clone()
            } else {
                format!("{t};q={:.1}", (10 - i.min(9)) as f32 / 10.0)
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// A parsed `Content-Disposition` header (RFC 6266).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Disposition {
    /// `attachment`: save rather than show.
    pub attachment: bool,
    /// The suggested file name (`filename*` preferred over `filename`).
    pub filename: Option<String>,
}

/// Parses a `Content-Disposition` value such as
/// `attachment; filename="a b.txt"; filename*=UTF-8''%E6%AA%94.txt`.
pub fn content_disposition(value: &str) -> Disposition {
    let mut params = Vec::new();
    let (kind, mut rest) = value.split_once(';').unwrap_or((value, ""));
    loop {
        let r = rest.trim_start_matches([' ', '\t', ';']);
        let Some((name, after)) = r.split_once('=') else {
            break;
        };
        let after = after.trim_start();
        let (val, next) = if let Some(q) = after.strip_prefix('"') {
            // A quoted string with backslash escapes.
            let mut out = String::new();
            let mut chars = q.char_indices();
            let mut end = q.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => out.extend(chars.next().map(|(_, c)| c)),
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    c => out.push(c),
                }
            }
            (out, &q[end..])
        } else {
            let (v, n) = after.split_once(';').unwrap_or((after, ""));
            (v.trim().to_string(), n)
        };
        params.push((name.trim().to_ascii_lowercase(), val));
        rest = next;
    }
    // RFC 5987 extended value: charset'language'percent-encoded.
    let extended = params
        .iter()
        .find(|(n, _)| n == "filename*")
        .and_then(|(_, v)| {
            let mut parts = v.splitn(3, '\'');
            let charset = parts.next()?.to_ascii_lowercase();
            let encoded = parts.nth(1)?;
            let bytes = url::percent_decode(encoded);
            match charset.as_str() {
                "utf-8" => String::from_utf8(bytes).ok(),
                "iso-8859-1" => Some(bytes.iter().map(|&b| b as char).collect()),
                _ => None,
            }
        });
    let plain = params
        .iter()
        .find(|(n, _)| n == "filename")
        .map(|(_, v)| v.clone());
    Disposition {
        attachment: kind.trim().eq_ignore_ascii_case("attachment"),
        filename: extended.or(plain).filter(|f| !f.is_empty()),
    }
}

/// `name` made safe as a file name in the download folder: no directory
/// parts, control characters or characters file systems reject, not
/// hidden; `download` when nothing is left.
pub fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            c if c.is_control() => '_',
            ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim_end_matches(['.', ' ']);
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.chars().take(200).collect()
    }
}

/// The file name for a download of `url`: the server's
/// (`Content-Disposition`), else the page's (`<a download=…>`), else the
/// last part of the URL's path.
pub fn download_name(url: &str, disposition: Option<&str>, suggested: Option<&str>) -> String {
    let from_header = disposition.and_then(|d| content_disposition(d).filename);
    let from_url = || {
        if url.starts_with("data:") || url.starts_with("blob:") {
            return None;
        }
        let path = url.split(['?', '#']).next()?;
        let path = path.split_once("://").map_or(path, |(_, r)| r);
        let last = path.split_once('/')?.1.rsplit('/').next()?;
        let name = String::from_utf8_lossy(&url::percent_decode(last)).into_owned();
        (!name.is_empty()).then_some(name)
    };
    let name = from_header
        .or_else(|| suggested.map(str::trim).filter(|s| !s.is_empty()).map(String::from))
        .or_else(from_url)
        .unwrap_or_default();
    safe_file_name(&name)
}

/// Whether a navigation response of MIME type `mime` is shown as a page
/// (or saved as a download, like types the browser cannot display).
pub fn displayable(mime: &str) -> bool {
    let essence = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let Some((ty, sub)) = essence.split_once('/') else {
        return true; // missing or invalid: sniffed as before
    };
    matches!(ty, "text" | "image" | "audio" | "video")
        || sub.ends_with("+xml")
        || sub.ends_with("+json")
        || matches!(
            sub,
            "xml" | "json" | "javascript" | "ecmascript" | "x-javascript" | "wasm"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_headers() {
        let d = content_disposition("attachment; filename=\"a \\\"b\\\".txt\"");
        assert!(d.attachment);
        assert_eq!(d.filename.as_deref(), Some("a \"b\".txt"));
        let d = content_disposition(
            "Attachment; filename=plain.txt; filename*=UTF-8''%E6%AA%94%E6%A1%88.txt",
        );
        assert!(d.attachment);
        assert_eq!(d.filename.as_deref(), Some("檔案.txt"));
        let d = content_disposition("inline");
        assert!(!d.attachment);
        assert_eq!(d.filename, None);
    }

    #[test]
    fn download_names() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name(".bashrc"), "bashrc");
        assert_eq!(safe_file_name("a:b?.txt"), "a_b_.txt");
        assert_eq!(safe_file_name(""), "download");
        assert_eq!(
            download_name("https://a.test/files/report%20v2.pdf?x=1", None, None),
            "report v2.pdf"
        );
        assert_eq!(
            download_name("https://a.test/x", Some("attachment; filename=r.csv"), Some("s.txt")),
            "r.csv"
        );
        assert_eq!(download_name("blob:https://a.test/1234", None, Some("s.txt")), "s.txt");
        assert_eq!(download_name("https://a.test/", None, None), "download");
    }

    #[test]
    fn displayable_types() {
        assert!(displayable("text/html; charset=utf-8"));
        assert!(displayable("image/svg+xml"));
        assert!(displayable(""));
        assert!(!displayable("application/octet-stream"));
        assert!(!displayable("application/zip"));
        assert!(!displayable("application/pdf"));
    }
}

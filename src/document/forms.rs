//! Form controls: kinds, checked/selected state, chosen files, labels and
//! form data (`application/x-www-form-urlencoded`, `multipart/form-data`,
//! `text/plain`).

use std::collections::HashMap;
use std::sync::Arc;

use crate::dom::{Dom, NodeId};

/// What kind of form control an element is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Text,
    Password,
    TextArea,
    Checkbox,
    Radio,
    Select,
    /// `<input type=submit>` / `<button>` (default type) / `type=image`.
    Submit,
    Reset,
    /// A button that does nothing by itself (`type=button`).
    Button,
    Hidden,
    /// `type=range`: a slider.
    Range,
    /// `type=color`: a color swatch.
    Color,
    /// `type=file`: files chosen with the platform's file dialog.
    File,
}

impl Control {
    /// Controls whose value is edited as text.
    pub fn is_text(self) -> bool {
        matches!(self, Control::Text | Control::Password | Control::TextArea)
    }

    /// `<input>`s drawn as push buttons (their label is their value).
    pub fn is_input_button(self) -> bool {
        matches!(self, Control::Submit | Control::Reset | Control::Button)
    }
}

/// The control kind of element `node`, if it is one.
pub fn control(dom: &Dom, node: NodeId) -> Option<Control> {
    let ty = dom
        .attr(node, "type")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    Some(match dom.tag(node)? {
        "input" => match ty.as_str() {
            "checkbox" => Control::Checkbox,
            "radio" => Control::Radio,
            "submit" | "image" => Control::Submit,
            "reset" => Control::Reset,
            "button" => Control::Button,
            "hidden" => Control::Hidden,
            "password" => Control::Password,
            "range" => Control::Range,
            "color" => Control::Color,
            "file" => Control::File,
            _ => Control::Text,
        },
        "textarea" => Control::TextArea,
        "select" => Control::Select,
        "button" => match ty.as_str() {
            "reset" => Control::Reset,
            "button" => Control::Button,
            _ => Control::Submit,
        },
        _ => return None,
    })
}

/// Checked and selected state of the document's controls (text values
/// live in the document's text fields).
#[derive(Debug, Default)]
pub struct FormState {
    checked: HashMap<NodeId, bool>,
    selected: HashMap<NodeId, usize>,
    /// Files chosen in `<input type=file>`s, with a serial number that
    /// changes with every selection.
    files: HashMap<NodeId, (u64, Vec<FileData>)>,
    next_selection: u64,
    /// `<select>` whose option list is open.
    pub open_select: Option<NodeId>,
}

/// A file: chosen in an `<input type=file>`, or the contents of a `blob:`
/// URL (without a name).
#[derive(Clone, Debug, PartialEq)]
pub struct FileData {
    pub name: String,
    /// MIME type (empty when unknown).
    pub mime: String,
    pub bytes: Arc<[u8]>,
    /// Last modification, in milliseconds since the Unix epoch.
    pub modified: f64,
}

impl FileData {
    /// A file named `name`, typed from its extension.
    pub fn new(name: &str, bytes: Vec<u8>, modified: f64) -> FileData {
        FileData {
            name: name.to_string(),
            mime: mime_for_name(name).unwrap_or("").to_string(),
            bytes: bytes.into(),
            modified,
        }
    }
}

/// A value in a form's entry list (HTML: "entry list").
#[derive(Clone, Debug, PartialEq)]
pub enum FormValue {
    Text(String),
    File(FileData),
}

impl FormState {
    pub fn checked(&self, dom: &Dom, node: NodeId) -> bool {
        self.checked
            .get(&node)
            .copied()
            .unwrap_or_else(|| dom.attr(node, "checked").is_some())
    }

    /// Checks or unchecks `node`; checking a radio button unchecks the
    /// others in its group (same name, same form).
    pub fn set_checked(&mut self, dom: &Dom, node: NodeId, value: bool) {
        if value && control(dom, node) == Some(Control::Radio) {
            let name = dom.attr(node, "name").unwrap_or("");
            let form = form_of(dom, node);
            for other in dom.descendants(Dom::ROOT) {
                if other != node
                    && control(dom, other) == Some(Control::Radio)
                    && !name.is_empty()
                    && dom.attr(other, "name") == Some(name)
                    && form_of(dom, other) == form
                {
                    self.checked.insert(other, false);
                }
            }
        }
        self.checked.insert(node, value);
    }

    pub fn selected_index(&self, dom: &Dom, select: NodeId) -> Option<usize> {
        let options = options(dom, select);
        if options.is_empty() {
            return None;
        }
        let i = self.selected.get(&select).copied().unwrap_or_else(|| {
            options
                .iter()
                .position(|&o| dom.attr(o, "selected").is_some())
                .unwrap_or(0)
        });
        Some(i.min(options.len() - 1))
    }

    pub fn set_selected(&mut self, select: NodeId, index: usize) {
        self.selected.insert(select, index);
    }

    /// The files chosen in `<input type=file>` `node`.
    pub fn files(&self, node: NodeId) -> &[FileData] {
        self.files.get(&node).map_or(&[], |(_, f)| f)
    }

    /// The serial number of `node`'s current selection (0: never chosen).
    pub fn selection(&self, node: NodeId) -> u64 {
        self.files.get(&node).map_or(0, |(s, _)| *s)
    }

    /// Replaces the files chosen in `node`.
    pub fn set_files(&mut self, node: NodeId, files: Vec<FileData>) {
        self.next_selection += 1;
        self.files.insert(node, (self.next_selection, files));
    }

    /// Restores every control in `form` to its initial state.
    pub fn reset(&mut self, dom: &Dom, form: NodeId) {
        for n in dom.descendants(form) {
            self.checked.remove(&n);
            self.selected.remove(&n);
            if self.files.get(&n).is_some_and(|(_, f)| !f.is_empty()) {
                self.set_files(n, Vec::new());
            }
        }
    }
}

/// `<option>`s of a `<select>` (including those in `<optgroup>`s).
pub fn options(dom: &Dom, select: NodeId) -> Vec<NodeId> {
    dom.descendants(select)
        .into_iter()
        .filter(|&n| dom.tag(n) == Some("option"))
        .collect()
}

/// The value of an `<option>`: its `value` attribute or its text.
pub fn option_value(dom: &Dom, option: NodeId) -> String {
    dom.attr(option, "value")
        .map(str::to_string)
        .unwrap_or_else(|| option_label(dom, option))
}

/// The text shown for an `<option>`.
pub fn option_label(dom: &Dom, option: NodeId) -> String {
    let text = dom.text_content(option);
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The `<form>` containing `node`.
pub fn form_of(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let mut n = dom.parent(node);
    while let Some(p) = n {
        if dom.tag(p) == Some("form") {
            return Some(p);
        }
        n = dom.parent(p);
    }
    None
}

/// The control a `<label>` is for: its `for` target or the first control
/// inside it.
pub fn label_target(dom: &Dom, label: NodeId) -> Option<NodeId> {
    if let Some(id) = dom.attr(label, "for") {
        return dom.element_by_id(id).filter(|&t| control(dom, t).is_some());
    }
    dom.descendants(label)
        .into_iter()
        .find(|&n| n != label && control(dom, n).is_some())
}

/// `application/x-www-form-urlencoded` encoding of name/value pairs.
pub fn urlencode(pairs: &[(String, String)]) -> String {
    let enc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => {
                    out.push(b as char)
                }
                b' ' => out.push('+'),
                b => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    pairs
        .iter()
        .map(|(n, v)| format!("{}={}", enc(n), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Replaces CR, LF and `"` in a name or filename of a `multipart/form-data`
/// part, as the HTML standard does.
fn escape_part_name(s: &str) -> String {
    s.replace('\r', "%0D")
        .replace('\n', "%0A")
        .replace('"', "%22")
}

/// Line breaks in a text value as CRLF (HTML: "normalize line breaks").
fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n")
}

/// A boundary for a `multipart/form-data` body: unlikely to appear in it
/// (the encoder still checks).
pub fn boundary() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    // splitmix64 of the time and a counter.
    let mut x = nanos ^ COUNT.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = String::from("----RubrowserFormBoundary");
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    for _ in 0..2 {
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        for i in 0..8 {
            out.push(CHARS[((z >> (i * 8)) % CHARS.len() as u64) as usize] as char);
        }
    }
    out
}

/// `multipart/form-data` encoding (RFC 7578, as the HTML standard's form
/// submission uses it) of `entries`; returns the body and its
/// `Content-Type` (with the boundary).
pub fn multipart(entries: &[(String, FormValue)]) -> (Vec<u8>, String) {
    let mut b = boundary();
    // A boundary that appears in a file would end the part early.
    while entries.iter().any(|(_, v)| match v {
        FormValue::File(f) => f.bytes.windows(b.len()).any(|w| w == b.as_bytes()),
        FormValue::Text(t) => t.contains(&b),
    }) {
        b = boundary();
    }
    let mut out = Vec::new();
    for (name, value) in entries {
        out.extend_from_slice(format!("--{b}\r\n").as_bytes());
        let name = escape_part_name(&crlf(name));
        match value {
            FormValue::Text(t) => {
                out.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                );
                out.extend_from_slice(crlf(t).as_bytes());
            }
            FormValue::File(f) => {
                let mime = if f.mime.is_empty() {
                    "application/octet-stream"
                } else {
                    &f.mime
                };
                out.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\nContent-Type: {mime}\r\n\r\n",
                        escape_part_name(&f.name)
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(&f.bytes);
            }
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{b}--\r\n").as_bytes());
    (out, format!("multipart/form-data; boundary={b}"))
}

/// Name/value pairs of an entry list, a file standing for its name (for
/// the encodings that cannot send files).
pub fn entry_pairs(entries: &[(String, FormValue)]) -> Vec<(String, String)> {
    entries
        .iter()
        .map(|(n, v)| {
            let v = match v {
                FormValue::Text(t) => t.clone(),
                FormValue::File(f) => f.name.clone(),
            };
            (n.clone(), v)
        })
        .collect()
}

/// `text/plain` form encoding (HTML standard): `name=value` lines.
pub fn text_plain(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(n, v)| format!("{}={}\r\n", crlf(n), crlf(v)))
        .collect()
}

/// The MIME type for a file name's extension, for the common types.
pub fn mime_for_name(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    MIME_TYPES
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, m)| *m)
}

/// File extensions of `mime` (`image/*` matches every image type).
pub fn extensions_for_mime(mime: &str) -> Vec<&'static str> {
    let mime = mime.trim().to_ascii_lowercase();
    let prefix = mime.strip_suffix("/*").map(|p| format!("{p}/"));
    MIME_TYPES
        .iter()
        .filter(|(_, m)| match &prefix {
            Some(p) => m.starts_with(p.as_str()),
            None => *m == mime,
        })
        .map(|(e, _)| *e)
        .collect()
}

/// Extensions and MIME types (those of the WHATWG MIME Sniffing standard
/// and other common ones).
const MIME_TYPES: &[(&str, &str)] = &[
    ("txt", "text/plain"),
    ("text", "text/plain"),
    ("md", "text/markdown"),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("css", "text/css"),
    ("csv", "text/csv"),
    ("ics", "text/calendar"),
    ("vcf", "text/vcard"),
    ("js", "text/javascript"),
    ("mjs", "text/javascript"),
    ("json", "application/json"),
    ("xml", "application/xml"),
    ("pdf", "application/pdf"),
    ("zip", "application/zip"),
    ("gz", "application/gzip"),
    ("tar", "application/x-tar"),
    ("wasm", "application/wasm"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("avif", "image/avif"),
    ("bmp", "image/bmp"),
    ("ico", "image/x-icon"),
    ("svg", "image/svg+xml"),
    ("mp3", "audio/mpeg"),
    ("wav", "audio/wav"),
    ("ogg", "audio/ogg"),
    ("oga", "audio/ogg"),
    ("flac", "audio/flac"),
    ("m4a", "audio/mp4"),
    ("mp4", "video/mp4"),
    ("m4v", "video/mp4"),
    ("webm", "video/webm"),
    ("ogv", "video/ogg"),
    ("mov", "video/quicktime"),
    ("woff", "font/woff"),
    ("woff2", "font/woff2"),
    ("ttf", "font/ttf"),
    ("otf", "font/otf"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::parse_html;

    #[test]
    fn kinds_radio_groups_and_options() {
        let d = parse_html(
            r#"<form><input id=t><input id=c type=checkbox checked>
               <input id=r1 type=radio name=g checked><input id=r2 type=radio name=g>
               <select id=s><option>One<option value=2 selected>Two</select>
               <button id=b>Go</button><label id=l for=t>T</label></form>"#,
        );
        let id = |s: &str| d.element_by_id(s).unwrap();
        assert_eq!(control(&d, id("t")), Some(Control::Text));
        assert_eq!(control(&d, id("b")), Some(Control::Submit));
        let mut f = FormState::default();
        assert!(f.checked(&d, id("c")));
        assert!(f.checked(&d, id("r1")));
        f.set_checked(&d, id("r2"), true);
        assert!(!f.checked(&d, id("r1")), "radio group is exclusive");
        assert_eq!(f.selected_index(&d, id("s")), Some(1));
        assert_eq!(option_value(&d, options(&d, id("s"))[0]), "One");
        assert_eq!(label_target(&d, id("l")), Some(id("t")));
        assert_eq!(form_of(&d, id("t")), d.find_tag(Dom::ROOT, "form"));
        f.reset(&d, d.find_tag(Dom::ROOT, "form").unwrap());
        assert!(f.checked(&d, id("r1")));
    }

    #[test]
    fn url_encoding() {
        let pairs = vec![("q".into(), "a b&c".into()), ("x".into(), "é".into())];
        assert_eq!(urlencode(&pairs), "q=a+b%26c&x=%C3%A9");
    }

    #[test]
    fn multipart_encoding() {
        let file = FileData::new("a\"b.txt", b"hi\r\nthere".to_vec(), 0.0);
        assert_eq!(file.mime, "text/plain");
        let entries = vec![
            ("name".into(), FormValue::Text("line1\nline2".into())),
            ("up".into(), FormValue::File(file)),
            (
                "empty".into(),
                FormValue::File(FileData::new("", Vec::new(), 0.0)),
            ),
        ];
        let (body, ctype) = multipart(&entries);
        let b = ctype.strip_prefix("multipart/form-data; boundary=").unwrap();
        let expected = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nline1\r\nline2\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"up\"; filename=\"a%22b.txt\"\r\n\
             Content-Type: text/plain\r\n\r\nhi\r\nthere\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"empty\"; filename=\"\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\r\n--{b}--\r\n"
        );
        assert_eq!(String::from_utf8(body).unwrap(), expected);
        assert_ne!(boundary(), boundary());
        assert_eq!(
            text_plain(&entry_pairs(&entries)),
            "name=line1\r\nline2\r\nup=a\"b.txt\r\nempty=\r\n"
        );
    }

    #[test]
    fn mime_types() {
        assert_eq!(mime_for_name("Photo.JPG"), Some("image/jpeg"));
        assert_eq!(mime_for_name("README"), None);
        assert!(extensions_for_mime("image/*").contains(&"png"));
        assert_eq!(extensions_for_mime("text/csv"), vec!["csv"]);
    }

    #[test]
    fn chosen_files_and_reset() {
        let d = parse_html("<form><input id=f type=file></form>");
        let f = d.element_by_id("f").unwrap();
        let mut s = FormState::default();
        assert!(s.files(f).is_empty());
        s.set_files(f, vec![FileData::new("x.png", vec![1, 2], 5.0)]);
        let first = s.selection(f);
        assert_eq!(s.files(f)[0].mime, "image/png");
        s.reset(&d, d.find_tag(Dom::ROOT, "form").unwrap());
        assert!(s.files(f).is_empty());
        assert_ne!(s.selection(f), first);
    }
}

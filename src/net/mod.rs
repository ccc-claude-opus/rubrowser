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

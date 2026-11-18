//! Time zones for scripts (`Date` local time, `Intl.DateTimeFormat`'s
//! `timeZone`): IANA names and UTC offsets from the host's time zone
//! database. The web version has no database and only knows UTC.

/// The host's IANA time zone (`TZ`, else the system setting), or "UTC".
pub fn local_name() -> String {
    #[cfg(feature = "native")]
    {
        let tz = jiff::tz::TimeZone::system();
        if let Some(name) = tz.iana_name() {
            return name.to_string();
        }
    }
    "UTC".to_string()
}

/// The offset from UTC in minutes of `name` ("" is the host's zone) at
/// `unix_ms`; None for a zone the database does not know.
pub fn offset_minutes(name: &str, unix_ms: f64) -> Option<f64> {
    if name.eq_ignore_ascii_case("UTC") || name.eq_ignore_ascii_case("Etc/UTC") || name.eq_ignore_ascii_case("GMT") {
        return Some(0.0);
    }
    #[cfg(feature = "native")]
    {
        let tz = if name.is_empty() { jiff::tz::TimeZone::system() } else { jiff::tz::TimeZone::get(name).ok()? };
        let t = jiff::Timestamp::from_millisecond(unix_ms as i64).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        return Some(f64::from(tz.to_offset(t).seconds()) / 60.0);
    }
    #[allow(unreachable_code)]
    {
        let _ = unix_ms; // the web version knows only UTC
        name.is_empty().then_some(0.0)
    }
}

/// The zone names the database knows, sorted (`Intl.supportedValuesOf`).
pub fn names() -> Vec<String> {
    #[cfg(feature = "native")]
    {
        let mut v: Vec<String> = jiff::tz::db()
            .available()
            .map(|n| n.as_str().to_string())
            .filter(|n| n.contains('/') && !n.starts_with("posix/") && !n.starts_with("right/") || n == "UTC")
            .collect();
        v.sort();
        v.dedup();
        if !v.is_empty() {
            return v;
        }
    }
    vec!["UTC".to_string()]
}

#[cfg(all(test, feature = "native"))]
mod tests {
    #[test]
    fn known_offsets() {
        // 2026-01-15 and 2026-07-15 (New York: EST, then EDT)
        assert_eq!(super::offset_minutes("Asia/Taipei", 1_768_435_200_000.0), Some(480.0));
        assert_eq!(super::offset_minutes("America/New_York", 1_768_435_200_000.0), Some(-300.0));
        assert_eq!(super::offset_minutes("America/New_York", 1_784_073_600_000.0), Some(-240.0));
        assert_eq!(super::offset_minutes("UTC", 0.0), Some(0.0));
        assert_eq!(super::offset_minutes("Nowhere/Atlantis", 0.0), None);
        assert!(super::names().iter().any(|n| n == "Asia/Tokyo"));
    }
}

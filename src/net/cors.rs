//! Cross-origin requests from scripts: the CORS protocol of the Fetch
//! standard (§3.2): which requests need a preflight, the checks on the
//! preflight and on the response, and which response headers scripts see.

/// A CORS-safelisted request header (Fetch §2.2.2), name lowercase.
fn safelisted(name: &str, value: &str) -> bool {
    match name {
        "accept" | "accept-language" | "content-language" => {
            value.len() <= 128 && !value.bytes().any(|b| matches!(b, b'"' | b'(' | b')' | b':' | b'<' | b'>' | b'?' | b'@' | b'[' | b'\\' | b']' | b'{' | b'}' | 0x7f) || (b < 0x20 && b != b'\t'))
        }
        "content-type" => {
            let essence = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            value.len() <= 128
                && matches!(essence.as_str(), "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain")
        }
        _ => false,
    }
}

/// The request's headers that are not safelisted, lowercase and sorted
/// (what `Access-Control-Request-Headers` lists).
pub fn unsafe_headers(headers: &[(String, String)], content_type: &str) -> Vec<String> {
    let mut out: Vec<String> = headers
        .iter()
        .map(|(n, v)| (n.to_ascii_lowercase(), v))
        .filter(|(n, v)| !safelisted(n, v))
        .map(|(n, _)| n)
        .collect();
    if !content_type.is_empty() && !safelisted("content-type", content_type) {
        out.push("content-type".into());
    }
    out.sort();
    out.dedup();
    out
}

/// Whether a method needs no preflight.
pub fn simple_method(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "POST")
}

/// Whether a cross-origin request needs a preflight (`OPTIONS`) first.
pub fn needs_preflight(method: &str, headers: &[(String, String)], content_type: &str) -> bool {
    !simple_method(method) || !unsafe_headers(headers, content_type).is_empty()
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

fn list(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The CORS check (Fetch §4.9) of a response to a request from `origin`.
pub fn check(origin: &str, credentials: bool, headers: &[(String, String)]) -> Result<(), String> {
    match header(headers, "access-control-allow-origin") {
        None => return Err("No 'Access-Control-Allow-Origin' header is present on the requested resource.".into()),
        Some("*") if !credentials => return Ok(()),
        Some("*") => {
            return Err("The value of the 'Access-Control-Allow-Origin' header must not be the wildcard '*' when the request's credentials mode is 'include'.".into());
        }
        Some(o) if o != origin => {
            return Err(format!("The 'Access-Control-Allow-Origin' header has a value '{o}' that is not equal to the supplied origin."));
        }
        Some(_) => {}
    }
    if credentials && header(headers, "access-control-allow-credentials") != Some("true") {
        return Err("The value of the 'Access-Control-Allow-Credentials' header must be 'true' when the request's credentials mode is 'include'.".into());
    }
    Ok(())
}

/// The checks on a preflight's response: status, CORS, and that the
/// method and headers are allowed.
pub fn check_preflight(
    origin: &str,
    credentials: bool,
    status: u16,
    method: &str,
    requested: &[String],
    headers: &[(String, String)],
) -> Result<(), String> {
    if !(200..300).contains(&status) {
        return Err("Response to preflight request doesn't pass access control check: It does not have HTTP ok status.".into());
    }
    check(origin, credentials, headers).map_err(|e| format!("Response to preflight request doesn't pass access control check: {e}"))?;
    let methods = list(header(headers, "access-control-allow-methods"));
    let any = |l: &[String]| !credentials && l.iter().any(|m| m == "*");
    if !simple_method(method) && !any(&methods) && !methods.iter().any(|m| m.eq_ignore_ascii_case(method)) {
        return Err(format!("Method {method} is not allowed by Access-Control-Allow-Methods in preflight response."));
    }
    let allowed = list(header(headers, "access-control-allow-headers"));
    for h in requested {
        // `*` never covers Authorization.
        let wildcard = any(&allowed) && h != "authorization";
        if !wildcard && !allowed.contains(h) {
            return Err(format!("Request header field {h} is not allowed by Access-Control-Allow-Headers in preflight response."));
        }
    }
    Ok(())
}

/// The response headers a script may read (Fetch §2.2.6: safelisted ones
/// and `Access-Control-Expose-Headers`).
pub fn exposed(headers: &[(String, String)], credentials: bool) -> Vec<(String, String)> {
    let expose = list(header(headers, "access-control-expose-headers"));
    let all = !credentials && expose.iter().any(|h| h == "*");
    headers
        .iter()
        .filter(|(n, _)| {
            let n = n.to_ascii_lowercase();
            all || expose.contains(&n)
                || matches!(
                    n.as_str(),
                    "cache-control" | "content-language" | "content-length" | "content-type" | "expires" | "last-modified" | "pragma"
                )
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn preflight_rules() {
        assert!(!needs_preflight("GET", &[], ""));
        assert!(!needs_preflight("POST", &h(&[("Accept", "text/plain")]), "text/plain;charset=UTF-8"));
        assert!(needs_preflight("POST", &[], "application/json"));
        assert!(needs_preflight("PUT", &[], ""));
        assert!(needs_preflight("GET", &h(&[("Authorization", "x")]), ""));
        assert_eq!(unsafe_headers(&h(&[("X-B", "1"), ("authorization", "a")]), "application/json"), ["authorization", "content-type", "x-b"]);
    }

    #[test]
    fn checks() {
        let o = "https://a.test";
        assert!(check(o, false, &h(&[("Access-Control-Allow-Origin", "*")])).is_ok());
        assert!(check(o, true, &h(&[("Access-Control-Allow-Origin", "*")])).is_err());
        assert!(check(o, true, &h(&[("access-control-allow-origin", o)])).is_err(), "credentials need ACAC");
        assert!(check(o, true, &h(&[("access-control-allow-origin", o), ("access-control-allow-credentials", "true")])).is_ok());
        assert!(check(o, false, &h(&[("access-control-allow-origin", "https://b.test")])).is_err());
        assert!(check(o, false, &[]).is_err());
        let ok = h(&[("access-control-allow-origin", o), ("access-control-allow-methods", "PUT, DELETE"), ("access-control-allow-headers", "x-a, Content-Type")]);
        assert!(check_preflight(o, false, 204, "PUT", &["content-type".into(), "x-a".into()], &ok).is_ok());
        assert!(check_preflight(o, false, 204, "PATCH", &[], &ok).is_err());
        assert!(check_preflight(o, false, 204, "PUT", &["x-b".into()], &ok).is_err());
        assert!(check_preflight(o, false, 404, "PUT", &[], &ok).is_err());
        let star = h(&[("access-control-allow-origin", "*"), ("access-control-allow-methods", "*"), ("access-control-allow-headers", "*")]);
        assert!(check_preflight(o, false, 200, "PATCH", &["x-b".into()], &star).is_ok());
        assert!(check_preflight(o, false, 200, "GET", &["authorization".into()], &star).is_err(), "* does not cover Authorization");
        let resp = h(&[("Content-Type", "a"), ("X-Secret", "s"), ("X-Shown", "v"), ("access-control-expose-headers", "x-shown")]);
        let names: Vec<String> = exposed(&resp, false).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["Content-Type", "X-Shown"]);
    }
}

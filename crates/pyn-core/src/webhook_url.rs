use crate::error::{PynError, Result};

/// The longest subscription URL accepted.
pub const MAX_URL_BYTES: usize = 2048;

/// The most event types one subscription may list.
pub const MAX_EVENT_TYPES: usize = 64;

const MAX_EVENT_TYPE_BYTES: usize = 64;

fn bad(why: &str) -> PynError {
    PynError::InvalidRequest(format!("invalid webhook url: {why}"))
}

/// Checks a subscription URL and returns it. `https` only, or `http` when `allow_http`; no userinfo, no fragment.
/// The address it resolves to is the sender's concern, not checked here.
pub fn validate_url(url: &str, allow_http: bool) -> Result<String> {
    let url = url.trim();
    if url.len() > MAX_URL_BYTES {
        return Err(bad(&format!("at most {MAX_URL_BYTES} bytes")));
    }
    if !url.is_ascii()
        || url
            .bytes()
            .any(|b| b.is_ascii_control() || b == b' ' || b == b'\\')
    {
        return Err(bad(
            "ASCII only, with no spaces, control characters or backslashes",
        ));
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| bad("expected https://host/path"))?;
    match scheme.to_ascii_lowercase().as_str() {
        "https" => {}
        "http" if allow_http => {}
        "http" => return Err(bad("https is required (this server does not allow http)")),
        _ => return Err(bad("the scheme must be https")),
    }
    if rest.contains('#') {
        return Err(bad("no fragment"));
    }
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(bad("no user name or password in the url"));
    }
    let (host, port, bracketed) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (addr, after) = inner
                .split_once(']')
                .ok_or_else(|| bad("unclosed [ in host"))?;
            if !after.is_empty() && !after.starts_with(':') {
                return Err(bad("the host is not valid"));
            }
            (addr, after.strip_prefix(':'), true)
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port), false),
            None => (authority, None, false),
        },
    };
    let host_ok = if bracketed {
        !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
    } else {
        host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
    };
    if !host_ok {
        return Err(bad("the host is not valid"));
    }
    if let Some(port) = port
        && !port.parse::<u16>().is_ok_and(|p| p != 0)
    {
        return Err(bad("the port is not valid"));
    }
    Ok(url.to_string())
}

/// `scheme://host[:port]` of a validated URL: what an audit entry may name without the path or query,
/// which can carry a token.
pub fn origin_of(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    format!("{scheme}://{authority}")
}

/// Checks event type names and returns them without duplicates. An empty list means every type.
/// Types are an open set, so only the form is checked: lowercase letters, digits, `.` and `_`.
pub fn validate_event_types(types: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::with_capacity(types.len());
    for t in types {
        let ok = !t.is_empty()
            && t.len() <= MAX_EVENT_TYPE_BYTES
            && t.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_');
        if !ok {
            return Err(PynError::InvalidRequest(format!(
                "invalid event type {t:?}: 1 to {MAX_EVENT_TYPE_BYTES} lowercase letters, digits, '.' or '_'"
            )));
        }
        if !out.contains(t) {
            out.push(t.clone());
        }
    }
    if out.len() > MAX_EVENT_TYPES {
        return Err(PynError::InvalidRequest(format!(
            "a webhook lists at most {MAX_EVENT_TYPES} event types"
        )));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_urls_pass() {
        for ok in [
            "https://example.com/hook",
            "https://example.com",
            "https://a.b-c.example.com:8443/x?y=1",
            "HTTPS://Example.com/",
            "https://[::1]:8443/x",
        ] {
            assert!(validate_url(ok, false).is_ok(), "{ok}");
        }
    }

    #[test]
    fn bad_urls_are_refused() {
        for bad in [
            "http://example.com/",
            "ftp://example.com/",
            "example.com/hook",
            "https://user:pw@example.com/",
            "https://user@example.com/",
            "https://example.com/#frag",
            "https:///path",
            "https://exa mple.com/",
            "https://example.com:0/",
            "https://example.com:99999/",
            "https://example.com:x/",
            "https://-.example..com/",
            "https://example.com\\@evil.com/",
            "https://exämple.com/",
            "https://[::1/",
        ] {
            assert!(validate_url(bad, false).is_err(), "{bad}");
        }
        assert!(
            validate_url(
                &format!("https://e.com/{}", "a".repeat(MAX_URL_BYTES)),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn http_needs_the_operator_setting() {
        assert!(validate_url("http://localhost:9000/hook", true).is_ok());
        assert!(validate_url("http://localhost:9000/hook", false).is_err());
        assert!(validate_url("ftp://localhost/", true).is_err());
    }

    #[test]
    fn the_origin_drops_path_and_query() {
        assert_eq!(
            origin_of("https://example.com:8443/hook/secret?t=1"),
            "https://example.com:8443"
        );
    }

    #[test]
    fn event_types_are_checked_and_deduplicated() {
        let ok = validate_event_types(&["a.b".into(), "a.b".into(), "c_d".into()]).unwrap();
        assert_eq!(ok, ["a.b", "c_d"]);
        assert!(validate_event_types(&["A".into()]).is_err());
        assert!(validate_event_types(&["".into()]).is_err());
        assert!(validate_event_types(&["a b".into()]).is_err());
        let many: Vec<String> = (0..=MAX_EVENT_TYPES).map(|i| format!("t{i}")).collect();
        assert!(validate_event_types(&many).is_err());
    }
}

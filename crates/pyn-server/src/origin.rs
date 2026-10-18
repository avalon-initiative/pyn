//! Origin checks for browser requests: sign-in and cookie-authenticated writes.

use axum::http::HeaderMap;
use axum::http::header::{ORIGIN, REFERER};

/// `scheme://host[:port]` lowercased with the default port dropped; `None` unless http(s) with a host.
/// A path, query or fragment is ignored unless `bare` asks for a plain origin.
pub fn normalize(url: &str, bare: bool) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return None,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if bare && !(tail.is_empty() || tail == "/") {
        return None;
    }
    let authority = authority.to_ascii_lowercase();
    if authority.is_empty() || authority.contains(['@', ' ']) {
        return None;
    }
    let (host, port) = match authority.rfind(':') {
        Some(i) if !authority[i..].contains(']') => (&authority[..i], &authority[i + 1..]),
        _ => (authority.as_str(), ""),
    };
    if host.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(if port.is_empty() || port == default_port {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    })
}

/// Parses a comma-separated list of origins; any entry that is not a bare http(s) origin is an error.
pub fn parse_list(value: &str) -> Result<Vec<String>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            normalize(entry, true).ok_or_else(|| {
                format!("{entry:?} is not an origin; use scheme://host[:port] without a path")
            })
        })
        .collect()
}

/// What the browser said it came from.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    Origin(String),
    /// The header is present but unusable (`null`, malformed or not http).
    Opaque,
    Absent,
}

/// The `Origin` header, else the `Referer`'s origin.
pub fn source(headers: &HeaderMap) -> Source {
    for name in [ORIGIN, REFERER] {
        if let Some(value) = headers.get(&name) {
            return match value.to_str().ok().and_then(|v| normalize(v, false)) {
                Some(origin) => Source::Origin(origin),
                None => Source::Opaque,
            };
        }
    }
    Source::Absent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_default_ports_and_paths() {
        let n = |u| normalize(u, false);
        assert_eq!(
            n("HTTPS://Pyn.Example:443/a?b#c").as_deref(),
            Some("https://pyn.example")
        );
        assert_eq!(
            n("http://localhost:5173/").as_deref(),
            Some("http://localhost:5173")
        );
        assert_eq!(n("http://host:80").as_deref(), Some("http://host"));
        assert_eq!(n("http://host:8080").as_deref(), Some("http://host:8080"));
        assert_eq!(n("http://[::1]:7878").as_deref(), Some("http://[::1]:7878"));
        assert_eq!(n("null"), None);
        assert_eq!(n("ftp://host"), None);
        assert_eq!(n("http://u@host"), None);
        assert_eq!(n("http://"), None);
    }

    #[test]
    fn a_bare_origin_has_no_path() {
        assert_eq!(normalize("http://a.example/x", true), None);
        assert_eq!(
            normalize("http://a.example/", true).as_deref(),
            Some("http://a.example")
        );
    }

    #[test]
    fn lists_split_on_commas_and_reject_bad_entries() {
        assert_eq!(
            parse_list(" http://a.example , https://b.example:8443,").unwrap(),
            ["http://a.example", "https://b.example:8443"]
        );
        assert!(parse_list("a.example").is_err());
        assert!(parse_list("http://a.example/path").is_err());
        assert!(parse_list("").unwrap().is_empty());
    }

    #[test]
    fn source_prefers_origin_then_referer() {
        let mut h = HeaderMap::new();
        assert_eq!(source(&h), Source::Absent);
        h.insert(REFERER, "https://r.example/page".parse().unwrap());
        assert_eq!(source(&h), Source::Origin("https://r.example".into()));
        h.insert(ORIGIN, "null".parse().unwrap());
        assert_eq!(source(&h), Source::Opaque);
        h.insert(ORIGIN, "https://o.example".parse().unwrap());
        assert_eq!(source(&h), Source::Origin("https://o.example".into()));
    }
}

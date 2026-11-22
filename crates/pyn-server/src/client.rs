//! The caller's network address, for rate limits. Behind a reverse proxy the address comes from `X-Forwarded-For`.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap};

use crate::AppState;

/// A key for the caller's address; an IPv6 address stands for its whole /64.
pub(crate) struct Client(pub Option<String>);

fn key(raw: &str) -> String {
    match raw.trim().parse::<IpAddr>() {
        Ok(IpAddr::V6(ip)) => {
            let [a, b, c, d, ..] = ip.segments();
            format!("{a:x}:{b:x}:{c:x}:{d:x}::/64")
        }
        Ok(ip) => ip.to_string(),
        Err(_) => raw.trim().chars().take(64).collect(),
    }
}

/// The entry `hops` positions from the right of the combined `X-Forwarded-For` lines (1 is the last).
/// `None` when the list is shorter than `hops`, a line is not text, or the entry is empty.
fn forwarded_entry(headers: &HeaderMap, hops: usize) -> Option<String> {
    let mut entries = Vec::new();
    for line in headers.get_all("x-forwarded-for") {
        entries.extend(line.to_str().ok()?.split(',').map(str::trim));
    }
    let entry = *entries.iter().rev().nth(hops.checked_sub(1)?)?;
    (!entry.is_empty()).then(|| entry.to_string())
}

/// The caller's address as text: the `X-Forwarded-For` entry `trusted_proxies` from the right when forwarded headers
/// are trusted, else the peer. Rate limits and service credential source checks both start here.
pub(crate) fn address(
    headers: &HeaderMap,
    extensions: &Extensions,
    trust_forwarded: bool,
    trusted_proxies: usize,
) -> Option<String> {
    let forwarded = trust_forwarded
        .then(|| forwarded_entry(headers, trusted_proxies))
        .flatten();
    forwarded.or_else(|| {
        extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip().to_string())
    })
}

/// The caller's address as an IP, when it parses.
pub(crate) fn source_ip(
    headers: &HeaderMap,
    extensions: &Extensions,
    trust_forwarded: bool,
    trusted_proxies: usize,
) -> Option<IpAddr> {
    address(headers, extensions, trust_forwarded, trusted_proxies)?
        .parse()
        .ok()
}

impl FromRequestParts<AppState> for Client {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Client(
            address(
                &parts.headers,
                &parts.extensions,
                state.trust_forwarded,
                state.trusted_proxies,
            )
            .map(|a| key(&a)),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(lines: &[&str]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for line in lines {
            map.append("x-forwarded-for", HeaderValue::from_str(line).unwrap());
        }
        map
    }

    fn peer() -> Extensions {
        let mut ext = Extensions::new();
        ext.insert(ConnectInfo(SocketAddr::from(([10, 0, 0, 9], 4000))));
        ext
    }

    #[test]
    fn the_entry_counted_from_the_right_is_the_client() {
        let cases: &[(&[&str], usize, Option<&str>)] = &[
            (&["1.1.1.1"], 1, Some("1.1.1.1")),
            (&["9.9.9.9, 1.1.1.1"], 1, Some("1.1.1.1")),
            (&["9.9.9.9, 1.1.1.1"], 2, Some("9.9.9.9")),
            (&["7.7.7.7, 9.9.9.9, 1.1.1.1"], 1, Some("1.1.1.1")),
            (&["7.7.7.7, 9.9.9.9, 1.1.1.1"], 2, Some("9.9.9.9")),
            (&["7.7.7.7, 9.9.9.9, 1.1.1.1"], 3, Some("7.7.7.7")),
            (&["6.6.6.6, 7.7.7.7, 9.9.9.9, 1.1.1.1"], 3, Some("7.7.7.7")),
            (&["1.1.1.1"], 2, None),
            (&["9.9.9.9, 1.1.1.1"], 3, None),
            (&["9.9.9.9, 1.1.1.1"], 16, None),
            (&["  9.9.9.9 ,\t 1.1.1.1  "], 2, Some("9.9.9.9")),
            (&["9.9.9.9,"], 1, None),
            (&["9.9.9.9,,1.1.1.1"], 2, None),
            (&["9.9.9.9,,1.1.1.1"], 3, Some("9.9.9.9")),
            (&[""], 1, None),
            (&["2001:db8::1, 2001:db8::2"], 1, Some("2001:db8::2")),
            (&["2001:db8::1, 2001:db8::2"], 2, Some("2001:db8::1")),
            (&["9.9.9.9", "1.1.1.1"], 1, Some("1.1.1.1")),
            (&["9.9.9.9", "1.1.1.1"], 2, Some("9.9.9.9")),
            (&["8.8.8.8, 9.9.9.9", "1.1.1.1"], 2, Some("9.9.9.9")),
            (&["8.8.8.8, 9.9.9.9", "1.1.1.1"], 3, Some("8.8.8.8")),
            (&["9.9.9.9", "1.1.1.1"], 3, None),
            (&[], 1, None),
            (&[], 2, None),
        ];
        for (lines, hops, want) in cases {
            assert_eq!(
                forwarded_entry(&headers(lines), *hops).as_deref(),
                *want,
                "{lines:?} with {hops} proxies"
            );
        }
    }

    #[test]
    fn zero_hops_and_a_non_text_line_yield_nothing() {
        assert_eq!(forwarded_entry(&headers(&["1.1.1.1"]), 0), None);
        let mut map = headers(&["9.9.9.9"]);
        map.append("x-forwarded-for", HeaderValue::from_bytes(b"\xff").unwrap());
        assert_eq!(forwarded_entry(&map, 1), None);
    }

    #[test]
    fn the_address_falls_back_to_the_peer_when_untrusted_or_short() {
        let h = headers(&["9.9.9.9, 1.1.1.1"]);
        let peer = peer();
        assert_eq!(address(&h, &peer, true, 2).as_deref(), Some("9.9.9.9"));
        assert_eq!(address(&h, &peer, true, 3).as_deref(), Some("10.0.0.9"));
        assert_eq!(address(&h, &peer, false, 1).as_deref(), Some("10.0.0.9"));
        assert_eq!(
            address(&HeaderMap::new(), &peer, true, 1).as_deref(),
            Some("10.0.0.9")
        );
        assert_eq!(address(&h, &Extensions::new(), true, 3), None);
    }

    #[test]
    fn an_ipv6_client_is_keyed_by_its_64() {
        assert_eq!(key("2001:db8:1:2:3:4:5:6"), "2001:db8:1:2::/64");
    }
}

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

/// The caller's address as text: the last `X-Forwarded-For` entry when forwarded headers are trusted, else the peer.
/// Rate limits and service credential source checks both start here.
pub(crate) fn address(
    headers: &HeaderMap,
    extensions: &Extensions,
    trust_forwarded: bool,
) -> Option<String> {
    let forwarded = trust_forwarded
        .then(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit(',').next())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
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
) -> Option<IpAddr> {
    address(headers, extensions, trust_forwarded)?.parse().ok()
}

impl FromRequestParts<AppState> for Client {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Client(
            address(&parts.headers, &parts.extensions, state.trust_forwarded).map(|a| key(&a)),
        ))
    }
}

//! The caller's network address, for rate limits. Behind a reverse proxy the address comes from `X-Forwarded-For`.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;

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

impl FromRequestParts<AppState> for Client {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let forwarded = state
            .trust_forwarded
            .then(|| {
                parts
                    .headers
                    .get("x-forwarded-for")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.rsplit(',').next())
                    .filter(|v| !v.trim().is_empty())
                    .map(key)
            })
            .flatten();
        let peer = || {
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| key(&c.0.ip().to_string()))
        };
        Ok(Client(forwarded.or_else(peer)))
    }
}

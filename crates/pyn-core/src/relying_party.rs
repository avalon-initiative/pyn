//! The WebAuthn relying party a server derives from its public address (#1105): the rp id and the origins a
//! ceremony may come from.

use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelyingParty {
    pub id: String,
    /// The public address's origin first, then the allowed origins inside the rp id's scope.
    pub origins: Vec<String>,
}

/// `(origin, host)` of an http(s) address: lowercased, default port dropped, path ignored.
fn split(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return None,
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = rest[..end].to_ascii_lowercase();
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
    let origin = if port.is_empty() || port == default_port {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{port}")
    };
    Some((origin, host.to_string()))
}

fn is_ip(host: &str) -> bool {
    host.starts_with('[') || host.parse::<Ipv4Addr>().is_ok()
}

fn is_secure(origin: &str, host: &str) -> bool {
    origin.starts_with("https://") || host == "localhost" || host.ends_with(".localhost")
}

fn within(host: &str, rp_id: &str) -> bool {
    host == rp_id || host.strip_suffix(rp_id).is_some_and(|p| p.ends_with('.'))
}

/// The relying party for `public_url`, or `None` when WebAuthn cannot work there: no http(s) host, an IP literal, or an
/// origin that is neither https nor localhost. `rp_id` may widen the id to a parent domain of the host.
pub fn relying_party(
    public_url: &str,
    rp_id: Option<&str>,
    allowed_origins: &[String],
) -> Option<RelyingParty> {
    let (origin, host) = split(public_url)?;
    if is_ip(&host) || !is_secure(&origin, &host) {
        return None;
    }
    let id = match rp_id.map(|r| r.trim().to_ascii_lowercase()) {
        Some(id) if !id.is_empty() => {
            if is_ip(&id) || id.contains(['/', ':', ' ']) || !within(&host, &id) {
                return None;
            }
            id
        }
        _ => host,
    };
    let mut origins = vec![origin];
    for extra in allowed_origins {
        if let Some((o, h)) = split(extra)
            && is_secure(&o, &h)
            && within(&h, &id)
            && !origins.contains(&o)
        {
            origins.push(o);
        }
    }
    Some(RelyingParty { id, origins })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(url: &str) -> Option<RelyingParty> {
        relying_party(url, None, &[])
    }

    #[test]
    fn derives_the_id_and_origin_from_the_public_url() {
        let r = rp("https://Pyn.Example:443/some/path").unwrap();
        assert_eq!(r.id, "pyn.example");
        assert_eq!(r.origins, ["https://pyn.example"]);
        assert_eq!(
            rp("https://pyn.example:8443").unwrap().origins,
            ["https://pyn.example:8443"]
        );
        assert_eq!(rp("http://localhost:5173").unwrap().id, "localhost");
    }

    #[test]
    fn an_address_without_a_usable_host_has_none() {
        for url in [
            "",
            "pyn.example",
            "ftp://pyn.example",
            "https://",
            "http://127.0.0.1:7878",
            "https://192.168.1.20",
            "https://[::1]:8080",
            "http://pyn.example",
            "https://user@pyn.example",
            "https://pyn.example:port",
        ] {
            assert_eq!(rp(url), None, "{url}");
        }
    }

    #[test]
    fn an_override_may_only_widen_to_a_parent_domain() {
        let url = "https://git.pyn.example";
        let ok = |id| relying_party(url, Some(id), &[]).map(|r| r.id);
        assert_eq!(ok("pyn.example").as_deref(), Some("pyn.example"));
        assert_eq!(ok(" Git.Pyn.Example ").as_deref(), Some("git.pyn.example"));
        for bad in [
            "other.example",
            "n.example",
            "sub.git.pyn.example",
            "10.0.0.1",
            "pyn.example:443",
        ] {
            assert_eq!(ok(bad), None, "{bad}");
        }
        assert_eq!(ok("").as_deref(), Some("git.pyn.example"));
    }

    #[test]
    fn allowed_origins_join_only_inside_the_rp_scope() {
        let allowed = [
            "https://app.pyn.example".to_string(),
            "https://evil.example".to_string(),
            "http://app.pyn.example".to_string(),
            "https://pyn.example/".to_string(),
        ];
        let r = relying_party("https://pyn.example", None, &allowed).unwrap();
        assert_eq!(
            r.origins,
            ["https://pyn.example", "https://app.pyn.example"]
        );
        let dev = relying_party(
            "http://localhost:7878",
            None,
            &["http://localhost:5173".into()],
        )
        .unwrap();
        assert_eq!(
            dev.origins,
            ["http://localhost:7878", "http://localhost:5173"]
        );
    }
}

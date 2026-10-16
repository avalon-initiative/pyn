//! Telling a pyn server from a git host for an `http(s)` clone address: ask the pyn API, fall back to git.

use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_DOCUMENT: u64 = 16 << 20;
/// A route every pyn server documents in its public OpenAPI document.
const MARKER_PATH: &str = "/v1/registration";

/// What the probe of `<server>/openapi.json` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Probe {
    PynServer,
    /// An answer that is not a pyn server's: a 404, another status or another document.
    NotPyn,
    /// No answer: refused, unreachable, timed out, or a server error.
    Failed(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Host {
    Pyn,
    /// `inconclusive` carries why the probe gave no answer, so a failed clone can say a pyn server may be down.
    Git {
        inconclusive: Option<String>,
    },
}

pub fn decide(probe: Probe) -> Host {
    match probe {
        Probe::PynServer => Host::Pyn,
        Probe::NotPyn => Host::Git { inconclusive: None },
        Probe::Failed(why) => Host::Git {
            inconclusive: Some(why),
        },
    }
}

/// Whether `body` is the OpenAPI document of a pyn server.
pub fn is_pyn_document(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("paths")?.get(MARKER_PATH).cloned())
        .is_some()
}

/// Asks `server` for its OpenAPI document without credentials, within a few seconds.
pub fn run(server: &str) -> Probe {
    let Ok(mut url) =
        reqwest::Url::parse(&format!("{}/openapi.json", server.trim_end_matches('/')))
    else {
        return Probe::NotPyn;
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    let client = match reqwest::blocking::Client::builder()
        .timeout(TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => return Probe::Failed(e.to_string()),
    };
    match client.get(url).send() {
        Ok(r) if r.status().is_server_error() => {
            Probe::Failed(format!("server returned {}", r.status()))
        }
        Ok(r) if r.status().is_success() => {
            use std::io::Read;
            let mut bytes = Vec::new();
            let body = match r.take(MAX_DOCUMENT).read_to_end(&mut bytes) {
                Ok(_) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(e) => return Probe::Failed(e.to_string()),
            };
            if is_pyn_document(&body) {
                Probe::PynServer
            } else {
                Probe::NotPyn
            }
        }
        Ok(_) => Probe::NotPyn,
        Err(e) => Probe::Failed(e.without_url().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_pyn_document_or_a_failed_probe_changes_the_route() {
        assert_eq!(decide(Probe::PynServer), Host::Pyn);
        assert_eq!(decide(Probe::NotPyn), Host::Git { inconclusive: None });
        assert_eq!(
            decide(Probe::Failed("timed out".into())),
            Host::Git {
                inconclusive: Some("timed out".into())
            }
        );
    }

    #[test]
    fn a_pyn_document_names_a_pyn_route() {
        assert!(is_pyn_document(
            r#"{"openapi":"3.1.0","paths":{"/v1/registration":{}}}"#
        ));
        assert!(!is_pyn_document(
            r#"{"openapi":"3.1.0","paths":{"/pets":{}}}"#
        ));
        assert!(!is_pyn_document("<html>not found</html>"));
        assert!(!is_pyn_document("{}"));
    }
}

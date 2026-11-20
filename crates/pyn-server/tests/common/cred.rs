//! Credentials sent the way each real client sends them. A secret of one kind never travels as another: a bearer
//! secret in the dev header would become an unrestricted dev user, so a mismatch panics.

#![allow(dead_code)]

use axum::Router;
use axum::body::Body;
use axum::http::request::Builder;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use pyn_proto as api;
use tower::ServiceExt;

pub const BROWSER_ORIGIN: &str = "http://localhost:5173";

#[derive(Clone, Debug)]
pub enum Cred {
    SignedOut,
    /// The `X-Pyn-User` header: a plain user name, never a secret.
    Dev(String),
    /// A user token (`pyn_...`), full or scoped, as `Authorization: Bearer`.
    Token(String),
    /// A service credential (`pyns_...`) as `Authorization: Bearer`.
    Service(String),
    /// An administrator's access session (`pyna_...`) as `Authorization: Bearer`.
    AccessSession(String),
    /// A browser session: the cookie, plus the CSRF header a browser adds to writes.
    Session {
        cookie: String,
        csrf: Option<String>,
    },
}

impl Cred {
    pub fn dev(user: &str) -> Self {
        Cred::Dev(user.into())
    }

    pub fn token(secret: &str) -> Self {
        Cred::Token(secret.into())
    }

    pub fn service(secret: &str) -> Self {
        Cred::Service(secret.into())
    }

    pub fn access_session(secret: &str) -> Self {
        Cred::AccessSession(secret.into())
    }

    pub fn session(cookie: &str, csrf: &str) -> Self {
        Cred::Session {
            cookie: cookie.into(),
            csrf: Some(csrf.into()),
        }
    }

    /// A session read without a CSRF header, as a browser sends a GET.
    pub fn session_read(cookie: &str) -> Self {
        Cred::Session {
            cookie: cookie.into(),
            csrf: None,
        }
    }

    /// A bearer secret of either kind: `pyns_` is a service credential, anything else must be a user token.
    pub fn bearer(secret: &str) -> Self {
        if secret.starts_with("pyns_") {
            Cred::Service(secret.into())
        } else if secret.starts_with("pyna_") {
            Cred::AccessSession(secret.into())
        } else {
            Cred::Token(secret.into())
        }
    }

    /// Classifies a legacy "who" string: empty is signed out, `pyns_` a service credential, `pyn_` a token,
    /// anything else a dev user name.
    pub fn who(who: &str) -> Self {
        if who.is_empty() {
            Cred::SignedOut
        } else if who.starts_with("pyns_") {
            Cred::Service(who.into())
        } else if who.starts_with("pyna_") {
            Cred::AccessSession(who.into())
        } else if who.starts_with("pyn_") {
            Cred::Token(who.into())
        } else {
            Cred::Dev(who.into())
        }
    }

    /// Adds the headers a real client of this kind sends; panics on a secret that does not fit its kind.
    pub fn apply(&self, b: Builder) -> Builder {
        match self {
            Cred::SignedOut => b,
            Cred::Dev(user) => {
                assert!(
                    !user.is_empty()
                        && !["pyn_", "pyns_", "pyna_", "pyni_"]
                            .iter()
                            .any(|p| user.starts_with(p))
                        && !user.contains(char::is_whitespace),
                    "dev header must carry a user name, not a secret: {user:?}"
                );
                b.header(api::DEV_USER_HEADER, user.as_str())
            }
            Cred::Token(t) => {
                assert!(
                    t.starts_with("pyn_") && !t.starts_with("pyns_"),
                    "a user token starts with `pyn_`: {t:?}"
                );
                b.header("authorization", format!("Bearer {t}"))
            }
            Cred::Service(s) => {
                assert!(
                    s.starts_with("pyns_"),
                    "a service credential starts with `pyns_`: {s:?}"
                );
                b.header("authorization", format!("Bearer {s}"))
            }
            Cred::AccessSession(s) => {
                assert!(
                    s.starts_with("pyna_"),
                    "an access session starts with `pyna_`: {s:?}"
                );
                b.header("authorization", format!("Bearer {s}"))
            }
            Cred::Session { cookie, csrf } => {
                assert!(
                    !cookie.is_empty()
                        && !cookie.contains([';', '=', ' '])
                        && !["pyn_", "pyns_", "pyna_", "pyni_"]
                            .iter()
                            .any(|p| cookie.starts_with(p)),
                    "a session cookie is an opaque value, not a token or credential secret: {cookie:?}"
                );
                let b = b
                    .header("cookie", format!("{}={cookie}", api::SESSION_COOKIE))
                    .header("origin", BROWSER_ORIGIN);
                match csrf {
                    Some(csrf) => b.header(api::CSRF_HEADER, csrf.as_str()),
                    None => b,
                }
            }
        }
    }
}

/// `Request::builder().method("GET").cred(&Cred::dev("alice"))`.
pub trait WithCred {
    fn cred(self, cred: &Cred) -> Self;
}

impl WithCred for Builder {
    fn cred(self, cred: &Cred) -> Self {
        cred.apply(self)
    }
}

pub struct Sent {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// Sends a JSON request carrying `cred`, with any `extra` headers after it.
pub async fn send(
    app: &Router,
    cred: &Cred,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    extra: &[(&str, &str)],
) -> Sent {
    let mut b = cred
        .apply(Request::builder().method(method).uri(uri))
        .header("content-type", "application/json");
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let body = body.map(|v| v.to_string()).unwrap_or_default();
    let r = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let (parts, body) = r.into_parts();
    Sent {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

/// A token-creation body for the nine internal permission names, mapped onto the closest grants, on selected
/// repositories (`owner/name`) of the first one's owner. An empty list names no repository.
pub fn token_body<R: AsRef<str>, P: AsRef<str>>(
    name: &str,
    repos: impl IntoIterator<Item = R>,
    permissions: impl IntoIterator<Item = P>,
) -> serde_json::Value {
    let repos: Vec<String> = repos.into_iter().map(|r| r.as_ref().to_string()).collect();
    let mut levels: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
    let rank = |l: &str| {
        ["read", "write", "admin"]
            .iter()
            .position(|x| *x == l)
            .unwrap_or(0)
    };
    for p in permissions {
        let (resource, level) = match p.as_ref() {
            "read" => ("contents", "read"),
            "checkin" | "restore" => ("contents", "write"),
            "lock" => ("locks", "write"),
            "force_unlock" => ("locks", "admin"),
            "edit_policy" => ("policy", "write"),
            "manage_users" => ("members", "write"),
            "manage_roles" => ("administration", "write"),
            "view_audit" => ("audit", "read"),
            other => (other, "read"),
        };
        let held = levels.entry(resource.to_string()).or_insert(level);
        if rank(held) < rank(level) {
            *held = level;
        }
    }
    let permissions: Vec<_> = levels
        .into_iter()
        .map(|(resource, level)| serde_json::json!({"resource": resource, "level": level}))
        .collect();
    let mut body = serde_json::json!({
        "name": name,
        "repository_access": {"mode": "selected", "repos": repos},
        "permissions": permissions,
    });
    if let Some(owner) = repos
        .first()
        .and_then(|r| r.split_once('/'))
        .map(|(o, _)| o)
    {
        body["owner"] = owner.into();
    }
    body
}

/// A token-creation body for `owner`'s selected repositories with each resource at its level (`("contents", "write")`).
pub fn grants_body(
    name: &str,
    owner: &str,
    repos: &[&str],
    grants: &[(&str, &str)],
) -> serde_json::Value {
    let repos: Vec<String> = repos.iter().map(|r| format!("{owner}/{r}")).collect();
    let permissions: Vec<_> = grants
        .iter()
        .map(|(resource, level)| serde_json::json!({"resource": resource, "level": level}))
        .collect();
    serde_json::json!({
        "name": name,
        "owner": owner,
        "repository_access": {"mode": "selected", "repos": repos},
        "permissions": permissions,
    })
}

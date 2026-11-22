//! The HTTP contract of two-factor authentication (#1104): enrolment, the sign-in challenge, recovery codes, the fresh
//! check behind sensitive actions and the credentials that never reach the factor routes. Codes come from the seed the
//! API returns, through an HMAC-SHA1 written here.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, Sent, send};
use std::sync::Arc;

use axum::Router;
use chrono::{Duration, TimeZone, Utc};
use hmac::{Hmac, KeyInit, Mac};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, Clock, ManualClock, RegistrationMode, Repositories,
    Rules,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use sha1::Sha1;

const PASSWORD: &str = "correct horse battery";
const ED1: &str = include_str!("../../pyn-core/tests/fixtures/keys/ed1.pub");

struct Server {
    app: Router,
    clock: Arc<ManualClock>,
}

fn server() -> Server {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock.clone(),
        Rules::empty(),
    ));
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        ready: Default::default(),
    });
    Server { app, clock }
}

fn base32_decode(text: &str) -> Vec<u8> {
    let (mut buffer, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for c in text.bytes() {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            other => panic!("not base32: {other}"),
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            out.push((buffer >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    out
}

fn app_code(seed: &[u8], unix: i64) -> String {
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(seed).unwrap();
    mac.update(&((unix / 30) as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[19] & 15) as usize;
    let n = (u32::from(h[o] & 127) << 24)
        | (u32::from(h[o + 1]) << 16)
        | (u32::from(h[o + 2]) << 8)
        | u32::from(h[o + 3]);
    format!("{:06}", n % 1_000_000)
}

impl Server {
    fn code(&self, seed: &[u8]) -> String {
        app_code(seed, self.clock.now().timestamp())
    }

    /// A code for a step no earlier code used: the clock moves one period on.
    fn next_code(&self, seed: &[u8]) -> String {
        self.clock.advance(Duration::seconds(30));
        self.code(seed)
    }

    async fn call(
        &self,
        cred: &Cred,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> Sent {
        send(&self.app, cred, method, uri, body, &[]).await
    }

    async fn signup(&self, name: &str) {
        let r = self
            .call(
                &Cred::SignedOut,
                "POST",
                "/v1/register",
                Some(json!({"username": name, "password": PASSWORD})),
            )
            .await;
        assert_eq!(r.status.as_u16(), 201);
    }

    async fn sign_in(&self, name: &str, code: Option<&str>) -> Sent {
        let mut body = json!({"username": name, "password": PASSWORD});
        if let Some(code) = code {
            body["code"] = code.into();
        }
        send(
            &self.app,
            &Cred::SignedOut,
            "POST",
            "/v1/session",
            Some(body),
            &[("origin", cred::BROWSER_ORIGIN)],
        )
        .await
    }

    async fn session(&self, name: &str, code: Option<&str>) -> Cred {
        let r = self.sign_in(name, code).await;
        assert_eq!(
            r.status.as_u16(),
            200,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        let cookie = r.headers["set-cookie"].to_str().unwrap().to_string();
        let cookie = cookie
            .split(';')
            .next()
            .unwrap()
            .strip_prefix(&format!("{}=", api::SESSION_COOKIE))
            .unwrap()
            .to_string();
        let info: api::SessionInfo = json(&r);
        Cred::session(&cookie, &info.csrf_token)
    }

    /// Turns two-factor authentication on for `name`; returns a signed-in session, the seed and the recovery codes.
    async fn enrol(&self, name: &str) -> (Cred, Vec<u8>, Vec<String>) {
        self.signup(name).await;
        let session = self.session(name, None).await;
        let r = self
            .call(
                &session,
                "POST",
                "/v1/me/two-factor/totp",
                Some(json!({"password": PASSWORD, "label": "phone"})),
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            201,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        let begun: api::TotpEnrolment = json(&r);
        let seed = base32_decode(&begun.secret);
        assert!(begun.uri.contains(&begun.secret));
        let r = self
            .call(
                &session,
                "POST",
                &format!("/v1/me/two-factor/totp/{}/confirm", begun.id),
                Some(json!({"code": self.code(&seed)})),
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            200,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        let done: api::TotpConfirmed = json(&r);
        (session, seed, done.recovery_codes.unwrap())
    }
}

fn keys_token(name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "repository_access": {"mode": "public"},
        "permissions": [{"resource": "account_ssh_keys", "level": "write"}],
    })
}

fn json<T: serde::de::DeserializeOwned>(r: &Sent) -> T {
    serde_json::from_slice(&r.body)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&r.body)))
}

fn code_of(r: &Sent) -> String {
    json::<api::ErrorBody>(r).code
}

#[tokio::test]
async fn enrolling_shows_the_codes_once_and_the_status_lists_the_factor() {
    let s = server();
    let (session, _, codes) = s.enrol("alice").await;
    assert_eq!(codes.len(), 10);

    let r = s.call(&session, "GET", "/v1/me/two-factor", None).await;
    let status: api::TwoFactorStatus = json(&r);
    assert!(status.enabled);
    assert_eq!(status.recovery_codes_left, 10);
    assert_eq!(
        (
            status.factors[0].kind.as_str(),
            status.factors[0].label.as_str()
        ),
        ("totp", "phone")
    );
    let text = String::from_utf8_lossy(&r.body);
    assert!(codes.iter().all(|c| !text.contains(c)), "never shown again");
    assert!(!text.contains("secret"), "the seed is never listed");
}

#[tokio::test]
async fn enrolment_needs_the_password_and_a_right_code() {
    let s = server();
    s.signup("alice").await;
    let session = s.session("alice", None).await;
    let r = s
        .call(&session, "POST", "/v1/me/two-factor/totp", Some(json!({})))
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (401, "unauthenticated")
    );
    let r = s
        .call(
            &session,
            "POST",
            "/v1/me/two-factor/totp",
            Some(json!({"password": PASSWORD})),
        )
        .await;
    let begun: api::TotpEnrolment = json(&r);
    let r = s
        .call(
            &session,
            "POST",
            &format!("/v1/me/two-factor/totp/{}/confirm", begun.id),
            Some(json!({"code": "000000"})),
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (401, "second_factor_invalid")
    );
    let r = s.call(&session, "GET", "/v1/me/two-factor", None).await;
    assert!(!json::<api::TwoFactorStatus>(&r).enabled);
    s.sign_in("alice", None).await;
}

#[tokio::test]
async fn password_sign_in_asks_for_the_code_on_both_routes() {
    let s = server();
    let (_, seed, _) = s.enrol("alice").await;

    for (uri, expect) in [("/v1/login", 401), ("/v1/session", 401)] {
        let r = send(
            &s.app,
            &Cred::SignedOut,
            "POST",
            uri,
            Some(json!({"username": "alice", "password": PASSWORD})),
            &[("origin", cred::BROWSER_ORIGIN)],
        )
        .await;
        assert_eq!(
            (r.status.as_u16(), code_of(&r).as_str()),
            (expect, "second_factor_required")
        );
    }
    let wrong = send(
        &s.app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(json!({"username": "alice", "password": "wrong wrong wrong"})),
        &[],
    )
    .await;
    assert_eq!(
        code_of(&wrong),
        "unauthenticated",
        "a wrong password never reaches the second factor"
    );

    let r = send(
        &s.app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(json!({"username": "alice", "password": PASSWORD, "code": s.next_code(&seed)})),
        &[],
    )
    .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let made: api::CreatedToken = json(&r);
    let whoami = s
        .call(&Cred::token(&made.token), "GET", "/v1/me", None)
        .await;
    assert_eq!(whoami.status.as_u16(), 200);
    s.session("alice", Some(&s.next_code(&seed))).await;
}

#[tokio::test]
async fn a_recovery_code_signs_in_once() {
    let s = server();
    let (_, _, codes) = s.enrol("alice").await;
    s.session("alice", Some(&codes[0])).await;
    let r = s.sign_in("alice", Some(&codes[0])).await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (401, "second_factor_invalid")
    );
    s.session("alice", Some(&codes[1].to_uppercase())).await;
}

#[tokio::test]
async fn wrong_codes_are_delayed_with_a_retry_after() {
    let s = server();
    let (_, seed, _) = s.enrol("alice").await;
    for _ in 0..5 {
        assert_eq!(
            s.sign_in("alice", Some("000000")).await.status.as_u16(),
            401
        );
    }
    let r = s.sign_in("alice", Some(&s.code(&seed))).await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (429, "too_many_attempts")
    );
    let wait: i64 = r.headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=30).contains(&wait));
    s.clock.advance(Duration::seconds(31));
    s.session("alice", Some(&s.code(&seed))).await;
}

#[tokio::test]
async fn sensitive_actions_need_a_fresh_check_and_a_code_reopens_them() {
    let s = server();
    let (session, seed, _) = s.enrol("alice").await;
    s.clock.advance(Duration::minutes(16));

    let key = json!({"key": ED1});
    let token = keys_token("ci");
    let guarded = [
        (
            "PUT",
            "/v1/me/password",
            json!({"current": PASSWORD, "new": "another long password"}),
        ),
        ("POST", "/v1/tokens", token.clone()),
        ("POST", "/v1/keys", key.clone()),
        ("DELETE", "/v1/keys/none", json!(null)),
        ("DELETE", "/v1/tokens/none", json!(null)),
        (
            "POST",
            "/v1/account/deletion",
            json!({"password": PASSWORD}),
        ),
        ("POST", "/v1/me/two-factor/recovery-codes", json!(null)),
        (
            "POST",
            "/v1/me/two-factor/totp",
            json!({"password": PASSWORD}),
        ),
    ];
    for (method, uri, body) in &guarded {
        let body = (!body.is_null()).then(|| body.clone());
        let r = s.call(&session, method, uri, body).await;
        assert_eq!(
            (r.status.as_u16(), code_of(&r).as_str()),
            (403, "fresh_check_required"),
            "{method} {uri}"
        );
    }
    let r = s.call(&session, "GET", "/v1/me/two-factor", None).await;
    assert_eq!(r.status.as_u16(), 200, "reading stays open");

    let r = s
        .call(
            &session,
            "POST",
            "/v1/me/two-factor/verify",
            Some(json!({"code": "000000"})),
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (401, "second_factor_invalid")
    );
    let r = s
        .call(
            &session,
            "POST",
            "/v1/me/two-factor/verify",
            Some(json!({"code": s.next_code(&seed)})),
        )
        .await;
    let fresh: api::FreshCheck = json(&r);
    assert_eq!(fresh.fresh_until, s.clock.now() + Duration::minutes(15));

    let r = s.call(&session, "POST", "/v1/tokens", Some(token)).await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = s.call(&session, "POST", "/v1/keys", Some(key)).await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let other = s.session("alice", Some(&s.next_code(&seed))).await;
    s.clock.advance(Duration::minutes(16));
    let r = s
        .call(&other, "POST", "/v1/keys", Some(json!({"key": ED1})))
        .await;
    assert_eq!(code_of(&r), "fresh_check_required");
    let r = s
        .call(&session, "POST", "/v1/keys", Some(json!({"key": ED1})))
        .await;
    assert_eq!(
        code_of(&r),
        "fresh_check_required",
        "the window ended for this session too"
    );
}

#[tokio::test]
async fn an_account_without_a_factor_meets_no_fresh_check() {
    let s = server();
    s.signup("alice").await;
    let session = s.session("alice", None).await;
    s.clock.advance(Duration::hours(5));
    let r = s
        .call(&session, "POST", "/v1/keys", Some(json!({"key": ED1})))
        .await;
    assert_eq!(r.status.as_u16(), 200);
    let r = s
        .call(
            &session,
            "POST",
            "/v1/me/two-factor/verify",
            Some(json!({"code": "123456"})),
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (400, "invalid_request")
    );
}

#[tokio::test]
async fn fine_grained_tokens_are_not_challenged_and_never_reach_the_factor_routes() {
    let s = server();
    let (session, seed, _) = s.enrol("alice").await;
    let r = s
        .call(&session, "POST", "/v1/tokens", Some(keys_token("keys")))
        .await;
    let made: api::CreatedToken = json(&r);
    let token = Cred::token(&made.token);
    s.clock.advance(Duration::hours(2));

    let r = s
        .call(&token, "POST", "/v1/keys", Some(json!({"key": ED1})))
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "an automation credential is not challenged"
    );
    for (method, uri) in [
        ("GET", "/v1/me/two-factor"),
        ("POST", "/v1/me/two-factor/totp"),
        ("POST", "/v1/me/two-factor/recovery-codes"),
        ("POST", "/v1/me/two-factor/verify"),
        ("DELETE", "/v1/me/two-factor/anything"),
    ] {
        let body = (method == "POST").then(|| json!({"code": s.code(&seed)}));
        let r = s.call(&token, method, uri, body).await;
        assert_eq!(
            (r.status.as_u16(), code_of(&r).as_str()),
            (403, "forbidden"),
            "{method} {uri}"
        );
    }
}

#[tokio::test]
async fn a_sign_in_token_that_passed_the_second_factor_is_fresh_for_a_while() {
    let s = server();
    let (_, seed, _) = s.enrol("alice").await;
    let r = send(
        &s.app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(json!({"username": "alice", "password": PASSWORD, "code": s.next_code(&seed)})),
        &[],
    )
    .await;
    let token = Cred::token(&json::<api::CreatedToken>(&r).token);
    let r = s
        .call(&token, "POST", "/v1/keys", Some(json!({"key": ED1})))
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    s.clock.advance(Duration::minutes(16));
    let r = s.call(&token, "DELETE", "/v1/keys/none", None).await;
    assert_eq!(code_of(&r), "fresh_check_required");
    let r = s
        .call(
            &token,
            "POST",
            "/v1/me/two-factor/verify",
            Some(json!({"code": s.code(&seed)})),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = s.call(&token, "DELETE", "/v1/keys/none", None).await;
    assert_eq!(code_of(&r), "key_not_found");
}

#[tokio::test]
async fn removing_the_last_factor_turns_two_factor_off() {
    let s = server();
    let (session, _, _) = s.enrol("alice").await;
    let r = s.call(&session, "GET", "/v1/me/two-factor", None).await;
    let id = json::<api::TwoFactorStatus>(&r).factors[0].id.clone();
    let r = s
        .call(&session, "DELETE", &format!("/v1/me/two-factor/{id}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 204);
    let r = s
        .call(&session, "DELETE", &format!("/v1/me/two-factor/{id}"), None)
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (404, "factor_not_found")
    );
    s.session("alice", None).await;
    let r = s.call(&session, "GET", "/v1/me/two-factor", None).await;
    let status: api::TwoFactorStatus = json(&r);
    assert_eq!((status.enabled, status.recovery_codes_left), (false, 0));
}

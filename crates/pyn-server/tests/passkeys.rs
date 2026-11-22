//! The HTTP contract of passkeys and security keys (#1105): registration, passkey sign-in, the fresh check, managing
//! the account's own passkeys and a server that cannot offer them. A software authenticator plays the browser.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, Sent, send};
use std::sync::Arc;

use axum::Router;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, ManualClock, RegistrationMode, Repositories, Rules,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use pyn_webauthn::software::SoftwareAuthenticator;
use serde_json::json;

const PASSWORD: &str = "correct horse battery";
const ORIGIN: &str = cred::BROWSER_ORIGIN;
const RP: &str = "localhost";

struct Server {
    app: Router,
    clock: Arc<ManualClock>,
}

fn server_at(public_url: &str) -> Server {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 11, 8, 9, 0, 0).unwrap(),
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
            public_url: public_url.into(),
            allowed_origins: vec![ORIGIN.into()],
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

fn server() -> Server {
    server_at(ORIGIN)
}

fn json<T: serde::de::DeserializeOwned>(r: &Sent) -> T {
    serde_json::from_slice(&r.body)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&r.body)))
}

fn code_of(r: &Sent) -> String {
    json::<api::ErrorBody>(r).code
}

fn session_of(r: &Sent) -> Cred {
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let cookie = r.headers["set-cookie"].to_str().unwrap();
    let cookie = cookie
        .split(';')
        .next()
        .unwrap()
        .strip_prefix(&format!("{}=", api::SESSION_COOKIE))
        .unwrap();
    let info: api::SessionInfo = json(r);
    Cred::session(cookie, &info.csrf_token)
}

impl Server {
    async fn call(
        &self,
        cred: &Cred,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> Sent {
        send(&self.app, cred, method, uri, body, &[]).await
    }

    async fn browser(&self, uri: &str, body: serde_json::Value, origin: &str) -> Sent {
        send(
            &self.app,
            &Cred::SignedOut,
            "POST",
            uri,
            Some(body),
            &[("origin", origin)],
        )
        .await
    }

    /// A signed-in session of a fresh account.
    async fn account(&self, name: &str) -> Cred {
        let r = self
            .call(
                &Cred::SignedOut,
                "POST",
                "/v1/register",
                Some(json!({"username": name, "password": PASSWORD})),
            )
            .await;
        assert_eq!(r.status.as_u16(), 201);
        let r = self
            .browser(
                "/v1/session",
                json!({"username": name, "password": PASSWORD}),
                ORIGIN,
            )
            .await;
        session_of(&r)
    }

    async fn register(
        &self,
        session: &Cred,
        device: &SoftwareAuthenticator,
        password: Option<&str>,
    ) -> Sent {
        let body = password.map_or(json!({}), |p| json!({"password": p}));
        let r = self
            .call(
                session,
                "POST",
                "/v1/me/passkeys/register/begin",
                Some(body),
            )
            .await;
        assert_eq!(
            r.status.as_u16(),
            200,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        let begun: api::PasskeyRegistrationBegun = json(&r);
        assert_eq!(begun.public_key.rp.id, RP);
        assert_eq!(begun.public_key.attestation, "none");
        assert_eq!(
            begun.public_key.authenticator_selection.user_verification,
            "required"
        );
        let made = device.create(
            RP,
            ORIGIN,
            &B64.decode(&begun.public_key.challenge).unwrap(),
        );
        self.call(
            session,
            "POST",
            "/v1/me/passkeys/register/finish",
            Some(json!({
                "ceremony": begun.ceremony,
                "name": "laptop",
                "credential": {
                    "id": B64.encode(&device.credential_id),
                    "type": "public-key",
                    "response": {
                        "clientDataJSON": B64.encode(&made.client_data_json),
                        "attestationObject": B64.encode(&made.attestation_object),
                        "transports": ["internal"],
                    },
                },
            })),
        )
        .await
    }

    fn assertion(
        &self,
        ceremony: &str,
        challenge: &str,
        device: &mut SoftwareAuthenticator,
    ) -> serde_json::Value {
        let got = device.get(RP, ORIGIN, &B64.decode(challenge).unwrap());
        json!({
            "ceremony": ceremony,
            "credential": {
                "id": B64.encode(&device.credential_id),
                "type": "public-key",
                "response": {
                    "clientDataJSON": B64.encode(&got.client_data_json),
                    "authenticatorData": B64.encode(&got.authenticator_data),
                    "signature": B64.encode(&got.signature),
                    "userHandle": null,
                },
            },
        })
    }

    async fn sign_in_with(&self, device: &mut SoftwareAuthenticator) -> Sent {
        let r = self
            .browser("/v1/session/passkey/begin", json!({}), ORIGIN)
            .await;
        assert_eq!(
            r.status.as_u16(),
            200,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        let begun: api::PasskeyAuthenticationBegun = json(&r);
        assert!(begun.public_key.allow_credentials.is_empty());
        assert_eq!(begun.public_key.rp_id, RP);
        let answer = self.assertion(&begun.ceremony, &begun.public_key.challenge, device);
        self.browser("/v1/session/passkey/finish", answer, ORIGIN)
            .await
    }
}

#[tokio::test]
async fn a_server_advertises_passkeys_only_with_a_public_address_webauthn_accepts() {
    let s = server();
    let caps: api::ServerCapabilities = json(
        &s.call(&Cred::SignedOut, "GET", "/v1/capabilities", None)
            .await,
    );
    assert!(caps.passkeys);
    let opts: api::SignInOptions = json(
        &s.call(&Cred::SignedOut, "GET", "/v1/sign-in-options", None)
            .await,
    );
    assert!(opts.passkeys);
    let session = s.account("alice").await;
    let status: api::TwoFactorStatus =
        json(&s.call(&session, "GET", "/v1/me/two-factor", None).await);
    assert!(status.passkeys_available && !status.enabled);

    let off = server_at("http://127.0.0.1:7878");
    let caps: api::ServerCapabilities = json(
        &off.call(&Cred::SignedOut, "GET", "/v1/capabilities", None)
            .await,
    );
    assert!(!caps.passkeys);
    let opts: api::SignInOptions = json(
        &off.call(&Cred::SignedOut, "GET", "/v1/sign-in-options", None)
            .await,
    );
    assert!(!opts.passkeys);
    let session = off.account("alice").await;
    let status: api::TwoFactorStatus =
        json(&off.call(&session, "GET", "/v1/me/two-factor", None).await);
    assert!(!status.passkeys_available);
}

#[tokio::test]
async fn a_server_without_one_answers_every_passkey_route_with_a_typed_error() {
    let off = server_at("http://127.0.0.1:7878");
    let session = off.account("alice").await;
    for (cred, uri, body) in [
        (
            &session,
            "/v1/me/passkeys/register/begin",
            json!({"password": PASSWORD}),
        ),
        (&session, "/v1/me/passkeys/verify/begin", json!(null)),
        (&Cred::SignedOut, "/v1/session/passkey/begin", json!({})),
    ] {
        let r = send(
            &off.app,
            cred,
            "POST",
            uri,
            Some(body),
            &[("origin", ORIGIN)],
        )
        .await;
        assert_eq!(r.status.as_u16(), 501, "{uri}");
        assert_eq!(code_of(&r), "passkeys_unavailable");
    }
    let r = off.call(&session, "GET", "/v1/me/passkeys", None).await;
    assert_eq!(r.status.as_u16(), 200, "listing needs no relying party");
}

#[tokio::test]
async fn registering_lists_renames_and_removes_a_passkey() {
    let s = server();
    let session = s.account("alice").await;
    let device = SoftwareAuthenticator::new(1);

    let r = s
        .call(
            &session,
            "POST",
            "/v1/me/passkeys/register/begin",
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status.as_u16(), 401, "the password is needed first");
    let r = s.register(&session, &device, Some(PASSWORD)).await;
    assert_eq!(
        r.status.as_u16(),
        201,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PasskeyRegistered = json(&r);
    assert_eq!(done.recovery_codes.as_ref().map(Vec::len), Some(10));
    assert_eq!(
        (
            done.passkey.name.as_str(),
            done.passkey.transports.as_slice()
        ),
        ("laptop", ["internal".to_string()].as_slice())
    );

    let r = s.call(&session, "GET", "/v1/me/passkeys", None).await;
    let list: Vec<api::Passkey> = json(&r);
    assert_eq!(list, std::slice::from_ref(&done.passkey));
    let text = String::from_utf8_lossy(&r.body);
    assert!(!text.contains("public_key") && !text.contains("credential_id"));
    let status: api::TwoFactorStatus =
        json(&s.call(&session, "GET", "/v1/me/two-factor", None).await);
    assert!(status.enabled);
    assert_eq!(status.passkeys.len(), 1);
    assert_eq!(status.recovery_codes_left, 10);

    let id = &done.passkey.id;
    let r = s
        .call(
            &session,
            "PATCH",
            &format!("/v1/me/passkeys/{id}"),
            Some(json!({"name": "yubikey"})),
        )
        .await;
    assert_eq!(json::<api::Passkey>(&r).name, "yubikey");
    let r = s
        .call(
            &session,
            "PATCH",
            "/v1/me/passkeys/nope",
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (404, "passkey_not_found")
    );

    let second = SoftwareAuthenticator::new(2);
    let r = s.register(&session, &second, None).await;
    assert_eq!(
        r.status.as_u16(),
        201,
        "a fresh check from registering lets the next one in"
    );
    assert!(json::<api::PasskeyRegistered>(&r).recovery_codes.is_none());

    let r = s
        .call(&session, "DELETE", &format!("/v1/me/passkeys/{id}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 204);
    let r = s
        .call(&session, "DELETE", &format!("/v1/me/passkeys/{id}"), None)
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (404, "passkey_not_found")
    );
}

#[tokio::test]
async fn a_passkey_signs_in_without_a_password_and_skips_the_second_step() {
    let s = server();
    let first = s.account("alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    let r = s.register(&first, &device, Some(PASSWORD)).await;
    assert_eq!(r.status.as_u16(), 201);

    let r = s
        .browser(
            "/v1/session",
            json!({"username": "alice", "password": PASSWORD}),
            ORIGIN,
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (401, "second_factor_required")
    );

    s.clock.advance(Duration::minutes(20));
    let r = s.sign_in_with(&mut device).await;
    let session = session_of(&r);
    let r = s.call(&session, "GET", "/v1/me/two-factor", None).await;
    assert!(json::<api::TwoFactorStatus>(&r).enabled);
    let r = s
        .call(&session, "POST", "/v1/me/two-factor/recovery-codes", None)
        .await;
    assert_eq!(r.status.as_u16(), 200, "the sign-in opened the fresh check");

    let list: Vec<api::Passkey> = json(&s.call(&session, "GET", "/v1/me/passkeys", None).await);
    assert!(list[0].last_used_at.is_some());
}

#[tokio::test]
async fn a_ceremony_answers_once_and_a_foreign_origin_is_refused() {
    let s = server();
    let session = s.account("alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    s.register(&session, &device, Some(PASSWORD)).await;

    let r = s
        .browser("/v1/session/passkey/begin", json!({}), ORIGIN)
        .await;
    let begun: api::PasskeyAuthenticationBegun = json(&r);
    let answer = s.assertion(&begun.ceremony, &begun.public_key.challenge, &mut device);
    let r = s
        .browser(
            "/v1/session/passkey/finish",
            answer.clone(),
            "https://evil.example",
        )
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (403, "origin_not_allowed")
    );
    let r = s
        .browser("/v1/session/passkey/finish", answer.clone(), ORIGIN)
        .await;
    assert_eq!(r.status.as_u16(), 200);
    let r = s
        .browser("/v1/session/passkey/finish", answer, ORIGIN)
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (401, "passkey_invalid")
    );

    let r = s
        .browser("/v1/session/passkey/finish", json!({"ceremony": "x", "credential": {"id": "!!", "response": {"clientDataJSON": "", "authenticatorData": "", "signature": ""}}}), ORIGIN)
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (400, "invalid_request")
    );
}

#[tokio::test]
async fn a_passkey_passes_the_fresh_check_that_removing_the_last_factor_needs() {
    let s = server();
    let first = s.account("alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    let done: api::PasskeyRegistered = json(&s.register(&first, &device, Some(PASSWORD)).await);
    let id = done.passkey.id;

    let r = s
        .browser("/v1/session/passkey/begin", json!({}), ORIGIN)
        .await;
    let signed = {
        let begun: api::PasskeyAuthenticationBegun = json(&r);
        let answer = s.assertion(&begun.ceremony, &begun.public_key.challenge, &mut device);
        session_of(
            &s.browser("/v1/session/passkey/finish", answer, ORIGIN)
                .await,
        )
    };
    s.clock.advance(Duration::minutes(20));
    let r = s
        .call(&signed, "DELETE", &format!("/v1/me/passkeys/{id}"), None)
        .await;
    assert_eq!(
        (r.status.as_u16(), code_of(&r).as_str()),
        (403, "fresh_check_required")
    );

    let r = s
        .call(&signed, "POST", "/v1/me/passkeys/verify/begin", None)
        .await;
    let begun: api::PasskeyAuthenticationBegun = json(&r);
    assert_eq!(begun.public_key.allow_credentials.len(), 1);
    let answer = s.assertion(&begun.ceremony, &begun.public_key.challenge, &mut device);
    let r = s
        .call(
            &signed,
            "POST",
            "/v1/me/passkeys/verify/finish",
            Some(answer),
        )
        .await;
    assert_eq!(
        r.status.as_u16(),
        200,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let fresh: api::FreshCheck = json(&r);
    assert_eq!(fresh.fresh_until, s.clock_now() + Duration::minutes(15));

    let r = s
        .call(&signed, "DELETE", &format!("/v1/me/passkeys/{id}"), None)
        .await;
    assert_eq!(r.status.as_u16(), 204);
    let status: api::TwoFactorStatus =
        json(&s.call(&signed, "GET", "/v1/me/two-factor", None).await);
    assert!(!status.enabled);
}

impl Server {
    fn clock_now(&self) -> chrono::DateTime<Utc> {
        use pyn_core::Clock;
        self.clock.now()
    }
}

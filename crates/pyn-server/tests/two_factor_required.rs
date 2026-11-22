//! The HTTP contract of required second factors (#1106): the grace period and its signal, the enrolment-only state
//! after it, who may require it, the administrator's reset and the account's notices.

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
    AccessConfig, AccessService, AccessStore, AuthProvider, Clock, ManualClock, RegistrationMode,
    Repositories, Rules,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use sha1::Sha1;

const PASSWORD: &str = "correct horse battery";

struct Server {
    app: Router,
    clock: Arc<ManualClock>,
    store: Arc<MemoryAccessStore>,
}

fn server() -> Server {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
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
    Server { app, clock, store }
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
}

fn json<T: serde::de::DeserializeOwned>(r: &Sent) -> T {
    serde_json::from_slice(&r.body)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&r.body)))
}

fn code_of(r: &Sent) -> String {
    json::<api::ErrorBody>(r).code
}

fn status(r: &Sent) -> u16 {
    r.status.as_u16()
}

impl Server {
    async fn admin(&self) -> Cred {
        self.signup("root").await;
        self.store
            .set_admin(&pyn_core::UserId::new("root"), true)
            .await
            .unwrap();
        self.session("root", None).await
    }

    async fn require(&self, admin: &Cred, grace: u32) {
        let r = self
            .call(
                admin,
                "PUT",
                "/v1/admin/two-factor",
                Some(json!({"required": true, "grace_days": grace})),
            )
            .await;
        assert_eq!(status(&r), 200, "{}", String::from_utf8_lossy(&r.body));
    }

    async fn enrol_session(&self, session: &Cred) -> Vec<u8> {
        let r = self
            .call(
                session,
                "POST",
                "/v1/me/two-factor/totp",
                Some(json!({"password": PASSWORD})),
            )
            .await;
        assert_eq!(status(&r), 201, "{}", String::from_utf8_lossy(&r.body));
        let begun: api::TotpEnrolment = json(&r);
        let seed = base32_decode(&begun.secret);
        let r = self
            .call(
                session,
                "POST",
                &format!("/v1/me/two-factor/totp/{}/confirm", begun.id),
                Some(json!({"code": self.code(&seed)})),
            )
            .await;
        assert_eq!(status(&r), 200, "{}", String::from_utf8_lossy(&r.body));
        seed
    }

    async fn org(&self, org: &str, owner: &Cred) {
        let r = self
            .call(owner, "POST", "/v1/orgs", Some(json!({"name": org})))
            .await;
        assert_eq!(status(&r), 201, "{}", String::from_utf8_lossy(&r.body));
    }

    async fn org_token(&self, owner: &Cred, org: &str, level: &str) -> Cred {
        let r = self
            .call(
                owner,
                "POST",
                "/v1/tokens",
                Some(json!({
                    "name": "org admin",
                    "owner": org,
                    "repository_access": {"mode": "public"},
                    "permissions": [{"resource": "org_administration", "level": level}],
                })),
            )
            .await;
        assert_eq!(status(&r), 200, "{}", String::from_utf8_lossy(&r.body));
        Cred::token(&json::<api::CreatedToken>(&r).token)
    }
}

#[tokio::test]
async fn during_the_grace_period_the_account_works_and_the_api_says_what_is_due() {
    let s = server();
    let root = s.admin().await;
    s.signup("alice").await;
    let alice = s.session("alice", None).await;
    s.require(&root, 7).await;

    let me: api::Account = json(&s.call(&alice, "GET", "/v1/me", None).await);
    let due = me
        .two_factor_required
        .expect("the grace period is signalled");
    assert_eq!((due.by.as_str(), due.enforced), ("server", false));
    assert_eq!(due.organization, None);
    assert_eq!(due.deadline, s.clock.now() + Duration::days(7));

    let st: api::TwoFactorStatus = json(&s.call(&alice, "GET", "/v1/me/two-factor", None).await);
    assert_eq!(st.required, Some(due));
    assert_eq!(
        status(&s.call(&alice, "GET", "/v1/me/locks", None).await),
        200
    );

    s.enrol_session(&alice).await;
    let me: api::Account = json(&s.call(&alice, "GET", "/v1/me", None).await);
    assert_eq!(me.two_factor_required, None, "met once a factor exists");
}

#[tokio::test]
async fn after_the_grace_period_a_session_reaches_only_the_enrolment_routes_until_it_enrols() {
    let s = server();
    let root = s.admin().await;
    s.signup("alice").await;
    let alice = s.session("alice", None).await;
    s.require(&root, 7).await;
    s.clock.advance(Duration::days(7));

    for (method, uri) in [
        ("GET", "/v1/me"),
        ("GET", "/v1/me/locks"),
        ("GET", "/v1/tokens"),
        ("POST", "/v1/orgs"),
        ("GET", "/v1/me/emails"),
    ] {
        let r = s.call(&alice, method, uri, Some(json!({}))).await;
        assert_eq!(
            (status(&r), code_of(&r).as_str()),
            (403, "two_factor_enrolment_required"),
            "{method} {uri}"
        );
    }
    let st: api::TwoFactorStatus = json(&s.call(&alice, "GET", "/v1/me/two-factor", None).await);
    assert!(st.required.is_some_and(|r| r.enforced));
    assert_eq!(
        status(&s.call(&alice, "GET", "/v1/me/notices", None).await),
        200
    );
    assert_eq!(
        status(&s.call(&alice, "GET", "/v1/session", None).await),
        200
    );

    s.enrol_session(&alice).await;
    assert_eq!(status(&s.call(&alice, "GET", "/v1/me", None).await), 200);
    assert_eq!(
        status(&s.call(&alice, "GET", "/v1/tokens", None).await),
        200
    );
}

#[tokio::test]
async fn a_sign_in_token_is_shut_out_but_other_tokens_keep_working() {
    let s = server();
    let root = s.admin().await;
    s.signup("alice").await;
    let alice = s.session("alice", None).await;
    let made = s
        .call(
            &alice,
            "POST",
            "/v1/tokens",
            Some(json!({
                "name": "ci",
                "repository_access": {"mode": "public"},
                "permissions": [{"resource": "account_ssh_keys", "level": "read"}],
            })),
        )
        .await;
    let ci = Cred::token(&json::<api::CreatedToken>(&made).token);
    let r = send(
        &s.app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(json!({"username": "alice", "password": PASSWORD})),
        &[],
    )
    .await;
    let sign_in = Cred::token(&json::<api::CreatedToken>(&r).token);

    s.require(&root, 0).await;
    let r = s.call(&sign_in, "GET", "/v1/me", None).await;
    assert_eq!(code_of(&r), "two_factor_enrolment_required");
    assert_eq!(
        status(&s.call(&sign_in, "GET", "/v1/me/two-factor", None).await),
        200
    );
    assert_eq!(status(&s.call(&ci, "GET", "/v1/keys", None).await), 200);
    assert_eq!(status(&s.call(&ci, "GET", "/v1/me", None).await), 200);
}

#[tokio::test]
async fn only_a_server_administrator_sets_the_server_requirement() {
    let s = server();
    let root = s.admin().await;
    s.signup("alice").await;
    let alice = s.session("alice", None).await;
    let body = Some(json!({"required": true}));
    let r = s
        .call(&alice, "PUT", "/v1/admin/two-factor", body.clone())
        .await;
    assert_eq!(
        (status(&r), code_of(&r).as_str()),
        (403, "server_admin_required")
    );
    assert_eq!(
        status(&s.call(&alice, "GET", "/v1/admin/two-factor", None).await),
        403
    );

    let r = s
        .call(
            &root,
            "PUT",
            "/v1/admin/two-factor",
            Some(json!({"required": true, "grace_days": 91})),
        )
        .await;
    assert_eq!((status(&r), code_of(&r).as_str()), (400, "invalid_request"));

    let r = s.call(&root, "PUT", "/v1/admin/two-factor", body).await;
    let set: api::TwoFactorSetting = json(&r);
    assert_eq!((set.required, set.grace_days), (true, Some(7)));
    let got: api::TwoFactorSetting =
        json(&s.call(&root, "GET", "/v1/admin/two-factor", None).await);
    assert_eq!(got, set);

    let off: api::TwoFactorSetting = json(
        &s.call(
            &root,
            "PUT",
            "/v1/admin/two-factor",
            Some(json!({"required": false})),
        )
        .await,
    );
    assert_eq!(
        off,
        api::TwoFactorSetting {
            required: false,
            since: None,
            grace_days: None
        }
    );
}

#[tokio::test]
async fn an_owner_requires_it_of_their_own_organization_only() {
    let s = server();
    s.signup("owner1").await;
    s.signup("owner2").await;
    s.signup("mia").await;
    let owner1 = s.session("owner1", None).await;
    let owner2 = s.session("owner2", None).await;
    let mia = s.session("mia", None).await;
    s.org("acme", &owner1).await;
    s.org("globex", &owner2).await;
    s.store
        .add_org_member(
            &pyn_core::UserId::new("acme"),
            &pyn_core::UserId::new("mia"),
            pyn_core::OrgRole::Member,
        )
        .await
        .unwrap();
    let on = Some(json!({"required": true, "grace_days": 0}));

    for who in [&owner2, &mia] {
        let r = s
            .call(who, "PUT", "/v1/orgs/acme/two-factor", on.clone())
            .await;
        assert_eq!((status(&r), code_of(&r).as_str()), (403, "not_org_owner"));
        assert_eq!(
            status(&s.call(who, "GET", "/v1/orgs/acme/two-factor", None).await),
            403
        );
    }
    assert_eq!(status(&s.call(&mia, "GET", "/v1/me", None).await), 200);

    let r = s.call(&owner1, "PUT", "/v1/orgs/acme/two-factor", on).await;
    let set: api::TwoFactorSetting = json(&r);
    assert_eq!((set.required, set.grace_days), (true, Some(0)));
    let r = s.call(&mia, "GET", "/v1/me", None).await;
    assert_eq!(code_of(&r), "two_factor_enrolment_required");
    let st: api::TwoFactorStatus = json(&s.call(&mia, "GET", "/v1/me/two-factor", None).await);
    let due = st.required.unwrap();
    assert_eq!(
        (due.by.as_str(), due.organization.as_deref()),
        ("organization", Some("acme"))
    );
    assert_eq!(status(&s.call(&owner2, "GET", "/v1/me", None).await), 200);
    assert_eq!(
        status(&s.call(&owner1, "GET", "/v1/me", None).await),
        403,
        "the owner is a member too"
    );
}

#[tokio::test]
async fn an_organization_token_needs_the_administration_grant_of_its_own_organization() {
    let s = server();
    s.signup("owner1").await;
    s.signup("owner2").await;
    let owner1 = s.session("owner1", None).await;
    let owner2 = s.session("owner2", None).await;
    s.org("acme", &owner1).await;
    s.org("globex", &owner2).await;
    let write = s.org_token(&owner1, "acme", "write").await;
    let read = s.org_token(&owner1, "acme", "read").await;
    let on = Some(json!({"required": true}));

    let r = s
        .call(&read, "PUT", "/v1/orgs/acme/two-factor", on.clone())
        .await;
    assert_eq!((status(&r), code_of(&r).as_str()), (403, "forbidden"));
    assert_eq!(
        status(&s.call(&read, "GET", "/v1/orgs/acme/two-factor", None).await),
        200
    );
    let r = s
        .call(&write, "PUT", "/v1/orgs/globex/two-factor", on.clone())
        .await;
    assert_eq!((status(&r), code_of(&r).as_str()), (403, "forbidden"));
    assert_eq!(
        status(&s.call(&write, "PUT", "/v1/orgs/acme/two-factor", on).await),
        200
    );
}

#[tokio::test]
async fn an_administrator_resets_the_factors_and_the_account_finds_a_notice() {
    let s = server();
    let root = s.admin().await;
    s.signup("alice").await;
    let alice = s.session("alice", None).await;
    let seed = s.enrol_session(&alice).await;
    s.signup("bob").await;
    let bob = s.session("bob", None).await;

    let r = s
        .call(&bob, "POST", "/v1/admin/users/alice/two-factor/reset", None)
        .await;
    assert_eq!(code_of(&r), "server_admin_required");
    let r = s
        .call(
            &root,
            "POST",
            "/v1/admin/users/nobody/two-factor/reset",
            None,
        )
        .await;
    assert_eq!(code_of(&r), "user_not_found");
    let r = s
        .call(&root, "POST", "/v1/admin/users/root/two-factor/reset", None)
        .await;
    assert_eq!(code_of(&r), "invalid_request");
    let r = s
        .call(
            &root,
            "POST",
            "/v1/admin/users/alice/two-factor/reset",
            None,
        )
        .await;
    assert_eq!(status(&r), 204);

    let st: api::TwoFactorStatus = json(&s.call(&alice, "GET", "/v1/me/two-factor", None).await);
    assert!(!st.enabled);
    let r = send(
        &s.app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(json!({"username": "alice", "password": PASSWORD})),
        &[],
    )
    .await;
    assert_eq!(
        status(&r),
        200,
        "no code is asked once the factors are gone"
    );
    let _ = seed;

    let notices: Vec<api::Notice> = json(&s.call(&alice, "GET", "/v1/me/notices", None).await);
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].kind, "two_factor_reset");
    assert!(notices[0].detail.contains("root"));
    let uri = format!("/v1/me/notices/{}", notices[0].id);
    assert_eq!(status(&s.call(&alice, "DELETE", &uri, None).await), 204);
    assert_eq!(
        code_of(&s.call(&alice, "DELETE", &uri, None).await),
        "notice_not_found"
    );
    let notices: Vec<api::Notice> = json(&s.call(&alice, "GET", "/v1/me/notices", None).await);
    assert!(notices.is_empty());
    assert!(
        json::<Vec<api::Notice>>(&s.call(&bob, "GET", "/v1/me/notices", None).await).is_empty()
    );

    let audit = s
        .call(&root, "GET", "/v1/admin/audit?limit=100", None)
        .await;
    let text = String::from_utf8_lossy(&audit.body);
    assert!(text.contains("two_factor_reset") && text.contains("alice"));
}

#[tokio::test]
async fn a_reset_account_that_must_enrol_is_let_in_to_enrol() {
    let s = server();
    let root = s.admin().await;
    s.signup("alice").await;
    let alice = s.session("alice", None).await;
    s.enrol_session(&alice).await;
    s.enrol_session(&root).await;
    s.require(&root, 0).await;
    assert_eq!(status(&s.call(&alice, "GET", "/v1/me", None).await), 200);

    let r = s
        .call(
            &root,
            "POST",
            "/v1/admin/users/alice/two-factor/reset",
            None,
        )
        .await;
    assert_eq!(status(&r), 204);
    let alice = s.session("alice", None).await;
    assert_eq!(
        code_of(&s.call(&alice, "GET", "/v1/me", None).await),
        "two_factor_enrolment_required"
    );
    assert_eq!(
        status(&s.call(&alice, "GET", "/v1/me/notices", None).await),
        200
    );
    s.enrol_session(&alice).await;
    assert_eq!(status(&s.call(&alice, "GET", "/v1/me", None).await), 200);
}

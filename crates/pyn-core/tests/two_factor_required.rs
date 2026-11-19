//! Required second factors (#1106, decision #1097) through the services over the in-memory stores: the grace period,
//! enforcement after it, who may require it and the administrator's reset.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use hmac::{Hmac, KeyInit, Mac};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditAction, AuditQuery, AuditScope, AuditStore,
    Clock, Credential, Identity, ManualClock, OrgRole, PynError, Registration, RegistrationMode,
    RequiredBy, SecretKey, UserId, WebhookSettings, fixtures::sign_in_record,
};
use sha1::Sha1;

const PASSWORD: &str = "correct horse battery";

struct World {
    access: Arc<AccessService>,
    clock: Arc<ManualClock>,
    audit: Arc<MemoryAuditStore>,
    store: Arc<MemoryAccessStore>,
}

fn service(
    store: &Arc<MemoryAccessStore>,
    clock: &Arc<ManualClock>,
    audit: &Arc<MemoryAuditStore>,
    key: Option<SecretKey>,
) -> Arc<AccessService> {
    Arc::new(
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
        .with_webhooks(
            Arc::new(pyn_core::memory::MemoryWebhookStore::new()),
            WebhookSettings {
                secret_key: key,
                allow_http: false,
            },
        )
        .with_registry(Arc::new(MemoryMetadataStore::new()))
        .with_audit(audit.clone()),
    )
}

fn world(key: Option<SecretKey>) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    World {
        access: service(&store, &clock, &audit, key),
        clock,
        audit,
        store,
    }
}

fn user(name: &str) -> UserId {
    UserId::new(name)
}

fn session(name: &str) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Session,
    }
}

async fn signup(w: &World, name: &str) {
    w.access
        .register(Registration::new(name, PASSWORD))
        .await
        .unwrap();
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

/// The six digits an authenticator app shows for `seed` at `unix` seconds.
fn app_code(seed: &[u8], unix: i64) -> String {
    let counter = (unix / 30) as u64;
    let mut mac = <Hmac<Sha1> as KeyInit>::new_from_slice(seed).unwrap();
    mac.update(&counter.to_be_bytes());
    let h = mac.finalize().into_bytes();
    let o = (h[19] & 15) as usize;
    let n = (u32::from(h[o] & 127) << 24)
        | (u32::from(h[o + 1]) << 16)
        | (u32::from(h[o + 2]) << 8)
        | u32::from(h[o + 3]);
    format!("{:06}", n % 1_000_000)
}

struct Enrolled {
    seed: Vec<u8>,
}

impl World {
    fn code_now(&self, e: &Enrolled) -> String {
        app_code(&e.seed, self.clock.now().timestamp())
    }

    /// The code for the step after the current one, which a used code cannot repeat.
    fn code_next(&self, e: &Enrolled) -> String {
        self.clock.advance(Duration::seconds(30));
        self.code_now(e)
    }
}

async fn enrol(w: &World, name: &str) -> Enrolled {
    let who = session(name);
    let begun = w
        .access
        .begin_totp(&who, Some("cookie"), Some("phone"), Some(PASSWORD))
        .await
        .unwrap();
    let seed = base32_decode(&begun.secret);
    let code = app_code(&seed, w.clock.now().timestamp());
    w.access
        .confirm_totp(&who, Some("cookie"), &begun.factor.id, &code)
        .await
        .unwrap();
    Enrolled { seed }
}

fn is(e: Result<impl std::fmt::Debug, PynError>, code: &str) {
    match e {
        Err(e) => assert_eq!(e.code(), code, "{e}"),
        Ok(v) => panic!("expected {code}, got {v:?}"),
    }
}

fn token(name: &str, w: &World) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Token(pyn_core::fixtures::token_record(
            "t1",
            name,
            &[],
            Vec::new(),
            w.clock.now(),
        )),
    }
}

fn sign_in_token(name: &str, w: &World) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Token(sign_in_record("s1", name, w.clock.now())),
    }
}

async fn admin(w: &World) -> Identity {
    signup(w, "root").await;
    w.store.set_admin(&user("root"), true).await.unwrap();
    session("root")
}

async fn org_with_owner(w: &World, org: &str, owner: &str) {
    w.access.create_org(&session(owner), org).await.unwrap();
}

async fn member(w: &World, org: &str, name: &str) {
    signup(w, name).await;
    w.store
        .add_org_member(&user(org), &user(name), OrgRole::Member)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_required_account_without_a_factor_is_let_in_during_the_grace_period_then_shut_out() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "alice").await;
    w.access
        .set_server_two_factor(&root, true, Some(7))
        .await
        .unwrap();

    w.access
        .enforce_two_factor(&session("alice"))
        .await
        .unwrap();
    let r = w
        .access
        .two_factor_requirement(&user("alice"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.by, RequiredBy::Server);
    assert_eq!(r.deadline, w.clock.now() + Duration::days(7));

    w.clock.advance(Duration::days(7) - Duration::seconds(1));
    w.access
        .enforce_two_factor(&session("alice"))
        .await
        .unwrap();
    w.clock.advance(Duration::seconds(1));
    is(
        w.access.enforce_two_factor(&session("alice")).await,
        "two_factor_enrolment_required",
    );
    is(
        w.access
            .enforce_two_factor(&sign_in_token("alice", &w))
            .await,
        "two_factor_enrolment_required",
    );
}

#[tokio::test]
async fn enrolling_a_factor_meets_the_requirement_at_any_time() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "alice").await;
    w.access
        .set_server_two_factor(&root, true, Some(0))
        .await
        .unwrap();
    is(
        w.access.enforce_two_factor(&session("alice")).await,
        "two_factor_enrolment_required",
    );
    enrol(&w, "alice").await;
    w.access
        .enforce_two_factor(&session("alice"))
        .await
        .unwrap();
    assert!(w.access.has_second_factor(&user("alice")).await.unwrap());
    assert!(
        w.access
            .unmet_two_factor_requirement(&user("alice"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_new_account_gets_the_full_grace_period() {
    let w = world(None);
    let root = admin(&w).await;
    w.access
        .set_server_two_factor(&root, true, Some(7))
        .await
        .unwrap();
    w.clock.advance(Duration::days(30));
    signup(&w, "bob").await;
    w.clock.advance(Duration::days(6));
    w.access.enforce_two_factor(&session("bob")).await.unwrap();
    w.clock.advance(Duration::days(1));
    is(
        w.access.enforce_two_factor(&session("bob")).await,
        "two_factor_enrolment_required",
    );
}

#[tokio::test]
async fn tokens_and_service_credentials_are_not_challenged_by_the_requirement() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "alice").await;
    w.access
        .set_server_two_factor(&root, true, Some(0))
        .await
        .unwrap();
    w.access
        .enforce_two_factor(&token("alice", &w))
        .await
        .unwrap();
    w.access
        .enforce_two_factor(&Identity {
            user: user("alice"),
            credential: Credential::Unrestricted,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn lifting_the_requirement_lets_everyone_back_in() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "alice").await;
    w.access
        .set_server_two_factor(&root, true, Some(0))
        .await
        .unwrap();
    is(
        w.access.enforce_two_factor(&session("alice")).await,
        "two_factor_enrolment_required",
    );
    w.access
        .set_server_two_factor(&root, false, None)
        .await
        .unwrap();
    w.access
        .enforce_two_factor(&session("alice"))
        .await
        .unwrap();
    assert!(w.access.server_two_factor().is_none());
}

#[tokio::test]
async fn only_a_server_administrator_sets_the_server_requirement_and_the_grace_is_bounded() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "alice").await;
    is(
        w.access
            .set_server_two_factor(&session("alice"), true, None)
            .await,
        "server_admin_required",
    );
    is(
        w.access.set_server_two_factor(&root, true, Some(91)).await,
        "invalid_request",
    );
    assert!(w.access.server_two_factor().is_none());
    let p = w
        .access
        .set_server_two_factor(&root, true, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(p.grace_days, 7);
}

#[tokio::test]
async fn an_owner_requires_two_factor_of_their_own_members_only() {
    let w = world(None);
    signup(&w, "owner1").await;
    signup(&w, "owner2").await;
    org_with_owner(&w, "acme", "owner1").await;
    org_with_owner(&w, "globex", "owner2").await;
    member(&w, "acme", "mia").await;
    member(&w, "globex", "gus").await;

    is(
        w.access
            .set_org_two_factor(&session("owner2"), &user("acme"), true, Some(0))
            .await,
        "not_org_owner",
    );
    is(
        w.access
            .set_org_two_factor(&session("mia"), &user("acme"), true, Some(0))
            .await,
        "not_org_owner",
    );
    assert!(
        w.access
            .org_two_factor_policy(&session("owner1"), &user("acme"))
            .await
            .unwrap()
            .is_none()
    );

    w.access
        .set_org_two_factor(&session("owner1"), &user("acme"), true, Some(0))
        .await
        .unwrap();
    is(
        w.access.enforce_two_factor(&session("mia")).await,
        "two_factor_enrolment_required",
    );
    w.access.enforce_two_factor(&session("gus")).await.unwrap();
    let r = w
        .access
        .two_factor_requirement(&user("mia"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.by, RequiredBy::Org(user("acme")));

    w.access
        .remove_org_member(&session("owner1"), &user("acme"), &user("mia"))
        .await
        .unwrap();
    w.access.enforce_two_factor(&session("mia")).await.unwrap();
}

#[tokio::test]
async fn the_earliest_deadline_among_requirements_decides() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "owner1").await;
    org_with_owner(&w, "acme", "owner1").await;
    member(&w, "acme", "mia").await;
    w.access
        .set_server_two_factor(&root, true, Some(30))
        .await
        .unwrap();
    w.access
        .set_org_two_factor(&session("owner1"), &user("acme"), true, Some(3))
        .await
        .unwrap();
    let r = w
        .access
        .two_factor_requirement(&user("mia"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r.by, RequiredBy::Org(user("acme")));
    assert_eq!(r.deadline, w.clock.now() + Duration::days(3));
}

#[tokio::test]
async fn requirement_changes_are_audited_where_they_belong() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "owner1").await;
    org_with_owner(&w, "acme", "owner1").await;
    w.access
        .set_server_two_factor(&root, true, Some(5))
        .await
        .unwrap();
    w.access
        .set_server_two_factor(&root, true, Some(5))
        .await
        .unwrap();
    w.access
        .set_org_two_factor(&session("owner1"), &user("acme"), true, Some(5))
        .await
        .unwrap();
    let q = AuditQuery {
        limit: 50,
        ..Default::default()
    };
    let server = w.audit.list(&AuditScope::Server, &q).await.unwrap();
    assert_eq!(
        server
            .iter()
            .filter(|e| e.action == AuditAction::ServerTwoFactorRequirementChanged)
            .count(),
        1
    );
    let org = w
        .audit
        .list(&AuditScope::Org(user("acme")), &q)
        .await
        .unwrap();
    assert_eq!(
        org.iter()
            .filter(|e| e.action == AuditAction::OrgTwoFactorRequirementChanged)
            .count(),
        1
    );
}

#[tokio::test]
async fn an_administrator_reset_removes_the_factors_audits_and_leaves_a_notice() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "alice").await;
    enrol(&w, "alice").await;
    assert_eq!(
        w.access
            .reset_two_factor(&root, Some("c"), &user("alice"))
            .await
            .unwrap(),
        1
    );
    assert!(!w.access.has_second_factor(&user("alice")).await.unwrap());
    assert_eq!(
        w.access
            .two_factor_status(&session("alice"))
            .await
            .unwrap()
            .recovery_codes_left,
        0
    );
    w.access.login("alice", PASSWORD, None).await.unwrap();

    let notices = w.access.notices(&session("alice")).await.unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].kind, "two_factor_reset");
    assert!(notices[0].detail.contains("root"));
    let log = w
        .audit
        .list(
            &AuditScope::Server,
            &AuditQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        log.iter()
            .any(|e| e.action == AuditAction::TwoFactorReset && e.detail.contains("alice"))
    );

    w.access
        .dismiss_notice(&session("alice"), &notices[0].id)
        .await
        .unwrap();
    assert!(
        w.access
            .notices(&session("alice"))
            .await
            .unwrap()
            .is_empty()
    );
    is(
        w.access
            .dismiss_notice(&session("alice"), &notices[0].id)
            .await,
        "notice_not_found",
    );
}

#[tokio::test]
async fn a_reset_needs_a_server_administrator_a_fresh_check_and_a_non_administrator_target() {
    let w = world(None);
    let root = admin(&w).await;
    signup(&w, "alice").await;
    signup(&w, "bob").await;
    enrol(&w, "alice").await;
    is(
        w.access
            .reset_two_factor(&session("bob"), None, &user("alice"))
            .await,
        "server_admin_required",
    );
    is(
        w.access.reset_two_factor(&root, None, &user("root")).await,
        "invalid_request",
    );
    is(
        w.access.reset_two_factor(&root, None, &user("ghost")).await,
        "user_not_found",
    );
    assert!(w.access.has_second_factor(&user("alice")).await.unwrap());

    let e = enrol(&w, "root").await;
    is(
        w.access
            .reset_two_factor(&root, Some("fresh?"), &user("alice"))
            .await,
        "fresh_check_required",
    );
    w.access
        .verify_second_factor(&root, Some("fresh?"), &w.code_next(&e))
        .await
        .unwrap();
    w.access
        .reset_two_factor(&root, Some("fresh?"), &user("alice"))
        .await
        .unwrap();
}

//! Two-factor authentication (#1104, decision #1097) through the services over the in-memory stores. Codes are
//! computed here from the seed the server hands out, by an HMAC-SHA1 written apart from the server's.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use hmac::{Hmac, KeyInit, Mac};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Clock,
    Credential, Identity, ManualClock, PynError, Registration, RegistrationMode, SecretKey,
    TotpConfirmed, UserId, WebhookSettings,
};
use sha1::Sha1;

const PASSWORD: &str = "correct horse battery";
const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

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
    factor_id: String,
    codes: Vec<String>,
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
    let TotpConfirmed { recovery_codes, .. } = w
        .access
        .confirm_totp(&who, Some("cookie"), &begun.factor.id, &code)
        .await
        .unwrap();
    Enrolled {
        seed,
        factor_id: begun.factor.id,
        codes: recovery_codes.expect("the first factor brings recovery codes"),
    }
}

fn is(e: Result<impl std::fmt::Debug, PynError>, code: &str) {
    match e {
        Err(e) => assert_eq!(e.code(), code, "{e}"),
        Ok(v) => panic!("expected {code}, got {v:?}"),
    }
}

#[tokio::test]
async fn an_account_without_a_factor_signs_in_with_the_password_alone() {
    let w = world(None);
    signup(&w, "alice").await;
    w.access.login("alice", PASSWORD, None).await.unwrap();
    w.access
        .start_session("alice", PASSWORD, None)
        .await
        .unwrap();
    w.access
        .require_fresh(&session("alice"), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn enrolment_needs_the_password_and_a_code_from_the_app() {
    let w = world(None);
    signup(&w, "alice").await;
    let who = session("alice");
    is(
        w.access.begin_totp(&who, Some("c"), None, None).await,
        "unauthenticated",
    );
    is(
        w.access
            .begin_totp(&who, Some("c"), None, Some("wrong wrong wrong"))
            .await,
        "unauthenticated",
    );

    let begun = w
        .access
        .begin_totp(&who, Some("c"), None, Some(PASSWORD))
        .await
        .unwrap();
    assert!(begun.uri.starts_with("otpauth://totp/pyn:alice?secret="));
    assert!(begun.uri.contains(&begun.secret));
    assert_eq!(begun.factor.label, "Authenticator app");
    let status = w.access.two_factor_status(&who).await.unwrap();
    assert!(status.factors.is_empty(), "a pending factor is not on yet");
    w.access.login("alice", PASSWORD, None).await.unwrap();

    let seed = base32_decode(&begun.secret);
    let now = w.clock.now().timestamp();
    let wrong = app_code(&seed, now - 600);
    is(
        w.access
            .confirm_totp(&who, Some("c"), &begun.factor.id, &wrong)
            .await,
        "second_factor_invalid",
    );
    is(
        w.access
            .confirm_totp(&who, Some("c"), "nope", &app_code(&seed, now))
            .await,
        "factor_not_found",
    );
    let done = w
        .access
        .confirm_totp(&who, Some("c"), &begun.factor.id, &app_code(&seed, now))
        .await
        .unwrap();
    assert_eq!(done.recovery_codes.unwrap().len(), 10);
    let status = w.access.two_factor_status(&who).await.unwrap();
    assert_eq!(status.factors.len(), 1);
    assert_eq!(status.recovery_codes_left, 10);
    assert!(status.factors[0].confirmed_at.is_some());
}

#[tokio::test]
async fn sign_in_asks_for_the_code_after_a_right_password_and_a_code_works_once() {
    let w = world(None);
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;

    is(
        w.access.login("alice", PASSWORD, None).await,
        "second_factor_required",
    );
    is(
        w.access.start_session("alice", PASSWORD, None).await,
        "second_factor_required",
    );
    is(
        w.access
            .login_with("alice", "wrong wrong wrong", None, None)
            .await,
        "unauthenticated",
    );
    is(
        w.access
            .login_with("alice", PASSWORD, Some("000000"), None)
            .await,
        "second_factor_invalid",
    );

    let code = w.code_next(&e);
    let (token, _) = w
        .access
        .login_with("alice", PASSWORD, Some(&code), None)
        .await
        .unwrap();
    assert!(token.sign_in);
    is(
        w.access
            .start_session_with("alice", PASSWORD, Some(&code), None)
            .await,
        "second_factor_invalid",
    );
    let next = w.code_next(&e);
    w.access
        .start_session_with("alice", PASSWORD, Some(&next), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_recovery_code_signs_in_once_and_is_audited() {
    let w = world(None);
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;
    let code = e.codes[3].to_uppercase();

    w.access
        .login_with("alice", PASSWORD, Some(&code), None)
        .await
        .unwrap();
    is(
        w.access
            .login_with("alice", PASSWORD, Some(&e.codes[3]), None)
            .await,
        "second_factor_invalid",
    );
    w.access
        .login_with("alice", PASSWORD, Some(&e.codes[4]), None)
        .await
        .unwrap();
    assert_eq!(
        w.access
            .two_factor_status(&session("alice"))
            .await
            .unwrap()
            .recovery_codes_left,
        8
    );

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
    let used = log
        .iter()
        .filter(|e| e.action == AuditAction::RecoveryCodeUsed)
        .count();
    assert_eq!(used, 2);
    for code in &e.codes {
        let plain = code.replace('-', "");
        assert!(
            log.iter()
                .all(|e| !e.detail.contains(code.as_str()) && !e.detail.contains(&plain))
        );
    }
}

#[tokio::test]
async fn failed_codes_lock_the_attempt_for_a_growing_delay_and_not_the_account() {
    let w = world(None);
    signup(&w, "alice").await;
    signup(&w, "bob").await;
    let e = enrol(&w, "alice").await;
    for _ in 0..5 {
        is(
            w.access
                .login_with("alice", PASSWORD, Some("000000"), None)
                .await,
            "second_factor_invalid",
        );
    }
    let code = w.code_now(&e);
    match w
        .access
        .login_with("alice", PASSWORD, Some(&code), None)
        .await
    {
        Err(PynError::TooManyAttempts { retry_after_secs }) => {
            assert!((1..=30).contains(&retry_after_secs));
        }
        other => panic!("expected a delay, got {other:?}"),
    }
    w.access
        .login_with("bob", PASSWORD, None, None)
        .await
        .unwrap();

    w.clock.advance(Duration::seconds(31));
    let code = app_code(&e.seed, w.clock.now().timestamp());
    w.access
        .login_with("alice", PASSWORD, Some(&code), None)
        .await
        .unwrap();
    for _ in 0..5 {
        let _ = w
            .access
            .login_with("alice", PASSWORD, Some("000000"), None)
            .await;
    }
    w.clock.advance(Duration::seconds(31));
    is(
        w.access
            .login_with("alice", PASSWORD, Some("000000"), None)
            .await,
        "second_factor_invalid",
    );
    match w
        .access
        .login_with("alice", PASSWORD, Some("000000"), None)
        .await
    {
        Err(PynError::TooManyAttempts { retry_after_secs }) => assert!(retry_after_secs > 30),
        other => panic!("the delay grows, got {other:?}"),
    }
}

#[tokio::test]
async fn sensitive_actions_need_a_fresh_check_that_ends_and_is_per_credential() {
    let w = world(None);
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;
    let who = session("alice");

    w.access.require_fresh(&who, Some("cookie")).await.unwrap();
    is(
        w.access.require_fresh(&who, Some("other")).await,
        "fresh_check_required",
    );
    is(
        w.access.require_fresh(&who, None).await,
        "fresh_check_required",
    );

    w.clock.advance(Duration::minutes(16));
    is(
        w.access.require_fresh(&who, Some("cookie")).await,
        "fresh_check_required",
    );
    is(
        w.access
            .verify_second_factor(&who, Some("cookie"), "000000")
            .await,
        "second_factor_invalid",
    );
    let code = w.code_next(&e);
    let until = w
        .access
        .verify_second_factor(&who, Some("cookie"), &code)
        .await
        .unwrap();
    assert_eq!(until, w.clock.now() + Duration::minutes(15));
    w.access.require_fresh(&who, Some("cookie")).await.unwrap();
    is(
        w.access.require_fresh(&who, Some("other")).await,
        "fresh_check_required",
    );

    w.clock.advance(Duration::minutes(15));
    is(
        w.access.require_fresh(&who, Some("cookie")).await,
        "fresh_check_required",
    );
}

#[tokio::test]
async fn a_sign_in_that_passed_the_second_factor_is_fresh_and_tokens_are_never_challenged() {
    let w = world(None);
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;
    let code = w.code_next(&e);
    let (record, cookie) = w
        .access
        .start_session_with("alice", PASSWORD, Some(&code), None)
        .await
        .unwrap();
    let (who, _) = w.access.authenticate_session(&cookie).await.unwrap();
    w.access.require_fresh(&who, Some(&cookie)).await.unwrap();
    assert_eq!(record.user, user("alice"));

    let code = w.code_next(&e);
    let (token, secret) = w
        .access
        .login_with("alice", PASSWORD, Some(&code), None)
        .await
        .unwrap();
    let signed_in = Identity {
        user: user("alice"),
        credential: Credential::Token(token),
    };
    w.access.require_fresh(&signed_in, None).await.unwrap();
    w.clock.advance(Duration::minutes(16));
    is(
        w.access.require_fresh(&signed_in, None).await,
        "fresh_check_required",
    );
    assert!(!secret.is_empty());

    let fine_grained = Identity {
        user: user("alice"),
        credential: Credential::Token(pyn_core::fixtures::token_record(
            "aaaaaaaaaaaa",
            "alice",
            &[pyn_core::Permission::Read],
            vec![],
            w.clock.now(),
        )),
    };
    w.access.require_fresh(&fine_grained, None).await.unwrap();
}

#[tokio::test]
async fn removing_factors_needs_a_fresh_check_and_the_last_one_turns_it_off() {
    let w = world(None);
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;
    let who = session("alice");

    w.clock.advance(Duration::minutes(20));
    is(
        w.access
            .remove_factor(&who, Some("cookie"), &e.factor_id)
            .await,
        "fresh_check_required",
    );
    is(
        w.access
            .regenerate_recovery_codes(&who, Some("cookie"))
            .await,
        "fresh_check_required",
    );
    is(
        w.access
            .begin_totp(&who, Some("cookie"), None, Some(PASSWORD))
            .await,
        "fresh_check_required",
    );
    let code = w.code_next(&e);
    w.access
        .verify_second_factor(&who, Some("cookie"), &code)
        .await
        .unwrap();

    let fresh = w
        .access
        .regenerate_recovery_codes(&who, Some("cookie"))
        .await
        .unwrap();
    assert_eq!(fresh.len(), 10);
    assert_ne!(fresh, e.codes);
    is(
        w.access
            .login_with("alice", PASSWORD, Some(&e.codes[0]), None)
            .await,
        "second_factor_invalid",
    );

    is(
        w.access.remove_factor(&who, Some("cookie"), "nope").await,
        "factor_not_found",
    );
    w.access
        .remove_factor(&who, Some("cookie"), &e.factor_id)
        .await
        .unwrap();
    let status = w.access.two_factor_status(&who).await.unwrap();
    assert!((status.factors.len(), status.recovery_codes_left) == (0, 0));
    w.access.login("alice", PASSWORD, None).await.unwrap();
    w.access.require_fresh(&who, Some("cookie")).await.unwrap();
    is(
        w.access
            .regenerate_recovery_codes(&who, Some("cookie"))
            .await,
        "invalid_request",
    );
}

#[tokio::test]
async fn a_second_app_can_be_added_after_a_fresh_check_and_either_signs_in() {
    let w = world(None);
    signup(&w, "alice").await;
    let first = enrol(&w, "alice").await;
    let who = session("alice");
    let begun = w
        .access
        .begin_totp(&who, Some("cookie"), Some("laptop"), None)
        .await
        .unwrap();
    let seed = base32_decode(&begun.secret);
    let now = w.clock.now().timestamp();
    let done = w
        .access
        .confirm_totp(
            &who,
            Some("cookie"),
            &begun.factor.id,
            &app_code(&seed, now),
        )
        .await
        .unwrap();
    assert!(
        done.recovery_codes.is_none(),
        "only the first factor brings codes"
    );
    assert_eq!(
        w.access
            .two_factor_status(&who)
            .await
            .unwrap()
            .factors
            .len(),
        2
    );

    w.clock.advance(Duration::seconds(30));
    let now = w.clock.now().timestamp();
    w.access
        .login_with("alice", PASSWORD, Some(&app_code(&seed, now)), None)
        .await
        .unwrap();
    w.clock.advance(Duration::seconds(30));
    w.access
        .login_with("alice", PASSWORD, Some(&w.code_now(&first)), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_server_key_keeps_the_seed_out_of_the_stored_row() {
    let w = world(Some(SecretKey::from_hex(KEY).unwrap()));
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;
    let rows = pyn_core::AccessStore::factors_of(w.store.as_ref(), &user("alice"))
        .await
        .unwrap();
    assert!(rows[0].encrypted);
    assert!(
        !rows[0]
            .secret
            .windows(e.seed.len())
            .any(|win| win == e.seed.as_slice())
    );
    let code = w.code_next(&e);
    w.access
        .login_with("alice", PASSWORD, Some(&code), None)
        .await
        .unwrap();

    let plain = world(None);
    signup(&plain, "alice").await;
    let e = enrol(&plain, "alice").await;
    let rows = pyn_core::AccessStore::factors_of(plain.store.as_ref(), &user("alice"))
        .await
        .unwrap();
    assert!(!rows[0].encrypted);
    assert_eq!(
        rows[0].secret, e.seed,
        "without a key the seed is stored as is"
    );
}

#[tokio::test]
async fn rotating_the_server_key_keeps_every_factor_working() {
    let old = SecretKey::from_hex(KEY).unwrap();
    let new = SecretKey::from_hex(&"ab".repeat(32)).unwrap();
    let w = world(Some(old.clone()));
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;

    assert!(
        pyn_core::rotate_seed_key(w.store.as_ref(), None, &new)
            .await
            .is_err()
    );
    let rewritten = pyn_core::rotate_seed_key(w.store.as_ref(), Some(&old), &new)
        .await
        .unwrap();
    assert_eq!(rewritten, 1);
    let rows = pyn_core::AccessStore::factors_of(w.store.as_ref(), &user("alice"))
        .await
        .unwrap();
    assert_eq!(rows[0].key_id.as_deref(), Some(new.key_id()));

    let code = w.code_next(&e);
    is(
        w.access
            .login_with("alice", PASSWORD, Some(&code), None)
            .await,
        "storage",
    );
    let rekeyed = service(&w.store, &w.clock, &w.audit, Some(new));
    rekeyed
        .login_with("alice", PASSWORD, Some(&code), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_server_audit_log_names_each_factor_change() {
    let w = world(None);
    signup(&w, "alice").await;
    let e = enrol(&w, "alice").await;
    let who = session("alice");
    w.access
        .regenerate_recovery_codes(&who, Some("cookie"))
        .await
        .unwrap();
    w.access
        .remove_factor(&who, Some("cookie"), &e.factor_id)
        .await
        .unwrap();
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
    let actions: Vec<_> = log
        .iter()
        .rev()
        .map(|e| e.action)
        .filter(|a| {
            matches!(
                a,
                AuditAction::TwoFactorAdded
                    | AuditAction::RecoveryCodesRegenerated
                    | AuditAction::TwoFactorRemoved
            )
        })
        .collect();
    assert_eq!(
        actions,
        [
            AuditAction::TwoFactorAdded,
            AuditAction::RecoveryCodesRegenerated,
            AuditAction::TwoFactorRemoved
        ]
    );
}

#[tokio::test]
async fn host_recovery_removes_the_factors_so_a_lost_device_cannot_lock_the_administrator_out() {
    let w = world(None);
    signup(&w, "root").await;
    let e = enrol(&w, "root").await;
    assert_eq!(e.codes.len(), 10);
    is(
        w.access.login("root", PASSWORD, None).await,
        "second_factor_required",
    );

    let done = w
        .access
        .recover_admin("root", "a brand new password")
        .await
        .unwrap();
    assert_eq!(done.factors_removed, 1);
    w.access
        .login("root", "a brand new password", None)
        .await
        .unwrap();
    let status = w.access.two_factor_status(&session("root")).await.unwrap();
    assert_eq!((status.factors.len(), status.recovery_codes_left), (0, 0));
}

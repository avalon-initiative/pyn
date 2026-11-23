//! Passkeys and security keys (#1105, decision #1127) through the services over the in-memory stores, with a software
//! authenticator standing in for the browser.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Clock,
    Credential, Identity, ManualClock, PynError, Registration, RegistrationMode, UserId,
    WebhookSettings,
};
use pyn_core::{
    AccessStore, PasskeyAssertion, PasskeyAttestation, PasskeyRegistered, PasskeyRequest,
};
use pyn_webauthn::software::SoftwareAuthenticator;

const PASSWORD: &str = "correct horse battery";
const ORIGIN: &str = "https://pyn.example";
const RP: &str = "pyn.example";

struct World {
    access: Arc<AccessService>,
    clock: Arc<ManualClock>,
    audit: Arc<MemoryAuditStore>,
    store: Arc<MemoryAccessStore>,
}

fn world_at(public_url: &str, rp_id: Option<&str>) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 11, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
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
            public_url: public_url.into(),
            webauthn_rp_id: rp_id.map(str::to_string),
            ..AccessConfig::default()
        })
        .with_webhooks(
            Arc::new(pyn_core::memory::MemoryWebhookStore::new()),
            WebhookSettings {
                secret_key: None,
                allow_http: false,
            },
        )
        .with_registry(Arc::new(MemoryMetadataStore::new()))
        .with_audit(audit.clone()),
    );
    World {
        access,
        clock,
        audit,
        store,
    }
}

fn world() -> World {
    world_at(ORIGIN, None)
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

fn is<T: std::fmt::Debug>(r: Result<T, PynError>, code: &str) {
    match r {
        Err(e) => assert_eq!(e.code(), code, "{e}"),
        Ok(v) => panic!("expected {code}, got {v:?}"),
    }
}

/// Registers `device` as a passkey of `name`, proving the password the first time.
async fn register(
    w: &World,
    name: &str,
    device: &SoftwareAuthenticator,
    password: Option<&str>,
) -> PasskeyRegistered {
    let who = session(name);
    let begun = w
        .access
        .begin_passkey_registration(&who, Some("cookie"), password)
        .await
        .unwrap();
    assert_eq!(begun.rp_id, RP);
    let made = device.create(RP, ORIGIN, &begun.challenge);
    w.access
        .finish_passkey_registration(
            &who,
            Some("cookie"),
            &begun.ceremony,
            Some("laptop"),
            &PasskeyAttestation {
                client_data_json: made.client_data_json,
                attestation_object: made.attestation_object,
                transports: vec!["internal".into()],
            },
        )
        .await
        .unwrap()
}

fn answer(device: &mut SoftwareAuthenticator, ask: &PasskeyRequest) -> PasskeyAssertion {
    let got = device.get(RP, ORIGIN, &ask.challenge);
    PasskeyAssertion {
        credential_id: device.credential_id.clone(),
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
        user_handle: None,
    }
}

#[tokio::test]
async fn a_discoverable_sign_in_carries_the_user_handle_the_browser_stored_at_registration() {
    let w = world();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    let who = session("alice");
    let begun = w
        .access
        .begin_passkey_registration(&who, Some("cookie"), Some(PASSWORD))
        .await
        .unwrap();
    let made = device.create(RP, ORIGIN, &begun.challenge);
    w.access
        .finish_passkey_registration(
            &who,
            Some("cookie"),
            &begun.ceremony,
            None,
            &PasskeyAttestation {
                client_data_json: made.client_data_json,
                attestation_object: made.attestation_object,
                transports: vec![],
            },
        )
        .await
        .unwrap();

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let mut reply = answer(&mut device, &ask);
    reply.user_handle = Some(begun.user_handle.clone());
    w.access
        .finish_passkey_sign_in(&ask.ceremony, &reply, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_first_passkey_brings_recovery_codes_and_counts_as_a_second_factor() {
    let w = world();
    signup(&w, "alice").await;
    let alice = user("alice");
    assert!(!w.access.has_second_factor(&alice).await.unwrap());

    let first = register(&w, "alice", &SoftwareAuthenticator::new(1), Some(PASSWORD)).await;
    assert_eq!(first.recovery_codes.as_ref().map(Vec::len), Some(10));
    assert_eq!(first.passkey.name, "laptop");
    assert_eq!(first.passkey.transports, ["internal"]);
    assert!(w.access.has_second_factor(&alice).await.unwrap());

    let second = register(&w, "alice", &SoftwareAuthenticator::new(2), None).await;
    assert_eq!(
        second.recovery_codes, None,
        "only the first factor brings codes"
    );
    let listed = w.access.list_passkeys(&session("alice")).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].user_handle, listed[1].user_handle);

    let status = w.access.two_factor_status(&session("alice")).await.unwrap();
    assert_eq!((status.factors.len(), status.passkeys.len()), (0, 2));
    assert_eq!(status.recovery_codes_left, 10);
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
    assert_eq!(
        log.iter()
            .filter(|e| e.action == AuditAction::PasskeyAdded)
            .count(),
        2
    );
}

#[tokio::test]
async fn registration_needs_the_password_until_a_second_factor_exists_then_a_fresh_check() {
    let w = world();
    signup(&w, "alice").await;
    let who = session("alice");
    is(
        w.access
            .begin_passkey_registration(&who, Some("c"), None)
            .await,
        "unauthenticated",
    );
    register(&w, "alice", &SoftwareAuthenticator::new(1), Some(PASSWORD)).await;

    let other = Some("another-cookie");
    is(
        w.access.begin_passkey_registration(&who, other, None).await,
        "fresh_check_required",
    );
    let mut device = SoftwareAuthenticator::new(1);
    let ask = w.access.begin_passkey_verify(&who).await.unwrap();
    let until = w
        .access
        .finish_passkey_verify(&who, other, &ask.ceremony, &answer(&mut device, &ask))
        .await
        .unwrap();
    assert_eq!(until, w.clock.now() + Duration::minutes(15));
    w.access
        .begin_passkey_registration(&who, other, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_server_without_a_usable_public_address_refuses_passkeys_with_a_typed_error() {
    for url in [
        "",
        "http://127.0.0.1:7878",
        "https://10.0.0.5",
        "http://pyn.example",
    ] {
        let w = world_at(url, None);
        signup(&w, "alice").await;
        assert!(!w.access.passkeys_available(), "{url}");
        let who = session("alice");
        is(
            w.access
                .begin_passkey_registration(&who, Some("c"), Some(PASSWORD))
                .await,
            "passkeys_unavailable",
        );
        is(
            w.access.begin_passkey_sign_in(None, None).await,
            "passkeys_unavailable",
        );
        is(
            w.access.begin_passkey_verify(&who).await,
            "passkeys_unavailable",
        );
    }
    assert!(
        world_at("http://localhost:5173", None)
            .access
            .passkeys_available()
    );
    let widened = world_at("https://git.pyn.example", Some("pyn.example"));
    assert_eq!(widened.access.relying_party().unwrap().id, "pyn.example");
    assert!(
        !world_at("https://git.pyn.example", Some("other.example"))
            .access
            .passkeys_available()
    );
}

#[tokio::test]
async fn a_passkey_signs_in_without_a_password_with_the_fresh_check_open() {
    let w = world();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    register(&w, "alice", &device, Some(PASSWORD)).await;
    w.clock.advance(Duration::minutes(30));

    let ask = w
        .access
        .begin_passkey_sign_in(None, Some("1.2.3.4"))
        .await
        .unwrap();
    assert!(ask.allow.is_empty());
    let (record, cookie) = w
        .access
        .finish_passkey_sign_in(
            &ask.ceremony,
            &answer(&mut device, &ask),
            false,
            Some("1.2.3.4"),
        )
        .await
        .unwrap();
    assert_eq!(record.user, user("alice"));
    w.access
        .require_fresh(&session("alice"), Some(&cookie))
        .await
        .unwrap();
    let held = &w.access.list_passkeys(&session("alice")).await.unwrap()[0];
    assert_eq!(held.last_used_at, Some(w.clock.now()));
    assert_eq!(held.sign_count, 2);

    w.clock.advance(Duration::minutes(16));
    is(
        w.access
            .require_fresh(&session("alice"), Some(&cookie))
            .await,
        "fresh_check_required",
    );

    let named = w
        .access
        .begin_passkey_sign_in(Some("alice"), None)
        .await
        .unwrap();
    assert_eq!(named.allow.len(), 1);
    let unknown = w
        .access
        .begin_passkey_sign_in(Some("nobody"), None)
        .await
        .unwrap();
    assert!(unknown.allow.is_empty());
}

#[tokio::test]
async fn a_challenge_works_once_and_only_for_a_few_minutes() {
    let w = world();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    register(&w, "alice", &device, Some(PASSWORD)).await;

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let reply = answer(&mut device, &ask);
    w.access
        .finish_passkey_sign_in(&ask.ceremony, &reply, false, None)
        .await
        .unwrap();
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &reply, false, None)
            .await,
        "passkey_ceremony_expired",
    );

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let reply = answer(&mut device, &ask);
    w.clock.advance(Duration::minutes(6));
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &reply, false, None)
            .await,
        "passkey_ceremony_expired",
    );

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    is(
        w.access
            .finish_passkey_sign_in("not-a-ceremony", &answer(&mut device, &ask), false, None)
            .await,
        "passkey_ceremony_expired",
    );
    let wrong_purpose = w
        .access
        .begin_passkey_verify(&session("alice"))
        .await
        .unwrap();
    is(
        w.access
            .finish_passkey_sign_in(
                &wrong_purpose.ceremony,
                &answer(&mut device, &wrong_purpose),
                false,
                None,
            )
            .await,
        "passkey_ceremony_expired",
    );
}

#[tokio::test]
async fn a_bad_answer_is_refused_and_spends_its_challenge() {
    let w = world();
    signup(&w, "alice").await;
    signup(&w, "bob").await;
    let mut device = SoftwareAuthenticator::new(1);
    register(&w, "alice", &device, Some(PASSWORD)).await;

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let mut stranger = SoftwareAuthenticator::new(9);
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &answer(&mut stranger, &ask), false, None)
            .await,
        "passkey_not_registered",
    );

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let mut forged = answer(&mut device, &ask);
    forged.signature[10] ^= 1;
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &forged, false, None)
            .await,
        "passkey_signature_invalid",
    );

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let got = device.get(RP, "https://evil.example", &ask.challenge);
    let wrong_origin = PasskeyAssertion {
        credential_id: device.credential_id.clone(),
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
        user_handle: None,
    };
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &wrong_origin, false, None)
            .await,
        "passkey_origin_not_allowed",
    );

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let mut wrong_handle = answer(&mut device, &ask);
    wrong_handle.user_handle = Some(b"someone else".to_vec());
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &wrong_handle, false, None)
            .await,
        "passkey_user_handle_mismatch",
    );

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let mut right_handle = answer(&mut device, &ask);
    right_handle.user_handle = Some(
        w.access.list_passkeys(&session("alice")).await.unwrap()[0]
            .user_handle
            .clone(),
    );
    w.access
        .finish_passkey_sign_in(&ask.ceremony, &right_handle, false, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_passkey_cannot_pass_another_accounts_fresh_check() {
    let w = world();
    signup(&w, "alice").await;
    signup(&w, "bob").await;
    let mut a = SoftwareAuthenticator::new(1);
    let mut b = SoftwareAuthenticator::new(2);
    register(&w, "alice", &a, Some(PASSWORD)).await;
    register(&w, "bob", &b, Some(PASSWORD)).await;

    let ask = w
        .access
        .begin_passkey_verify(&session("alice"))
        .await
        .unwrap();
    is(
        w.access
            .finish_passkey_verify(
                &session("alice"),
                Some("c"),
                &ask.ceremony,
                &answer(&mut b, &ask),
            )
            .await,
        "passkey_not_registered",
    );
    let ask = w
        .access
        .begin_passkey_verify(&session("alice"))
        .await
        .unwrap();
    is(
        w.access
            .finish_passkey_verify(
                &session("bob"),
                Some("c"),
                &ask.ceremony,
                &answer(&mut b, &ask),
            )
            .await,
        "passkey_ceremony_expired",
        // the challenge belongs to alice
    );
    let ask = w
        .access
        .begin_passkey_verify(&session("alice"))
        .await
        .unwrap();
    w.access
        .finish_passkey_verify(
            &session("alice"),
            Some("c"),
            &ask.ceremony,
            &answer(&mut a, &ask),
        )
        .await
        .unwrap();
    is(
        w.access.begin_passkey_verify(&session("carol")).await,
        "user_not_found",
    );
}

#[tokio::test]
async fn a_counter_that_does_not_advance_is_refused_reported_and_not_stored() {
    let w = world();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    register(&w, "alice", &device, Some(PASSWORD)).await;

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    w.access
        .finish_passkey_sign_in(&ask.ceremony, &answer(&mut device, &ask), false, None)
        .await
        .unwrap();

    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let got = device.get_with_counter(RP, ORIGIN, &ask.challenge, 1);
    let replayed = PasskeyAssertion {
        credential_id: device.credential_id.clone(),
        client_data_json: got.client_data_json,
        authenticator_data: got.authenticator_data,
        signature: got.signature,
        user_handle: None,
    };
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &replayed, false, None)
            .await,
        "passkey_counter_regressed",
    );
    assert_eq!(
        w.access.list_passkeys(&session("alice")).await.unwrap()[0].sign_count,
        2
    );
    let notices = w.access.notices(&session("alice")).await.unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].kind, "passkey_clone_suspected");
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
            .any(|e| e.action == AuditAction::PasskeyCloneSuspected)
    );
}

#[tokio::test]
async fn an_authenticator_that_keeps_no_counter_signs_in_repeatedly() {
    let w = world();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    device.counter = 0;
    register(&w, "alice", &device, Some(PASSWORD)).await;
    for _ in 0..3 {
        let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
        let got = device.get_with_counter(RP, ORIGIN, &ask.challenge, 0);
        let reply = PasskeyAssertion {
            credential_id: device.credential_id.clone(),
            client_data_json: got.client_data_json,
            authenticator_data: got.authenticator_data,
            signature: got.signature,
            user_handle: None,
        };
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &reply, false, None)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn the_same_authenticator_registers_once() {
    let w = world();
    signup(&w, "alice").await;
    signup(&w, "bob").await;
    let device = SoftwareAuthenticator::new(1);
    register(&w, "alice", &device, Some(PASSWORD)).await;

    let who = session("bob");
    let begun = w
        .access
        .begin_passkey_registration(&who, Some("c"), Some(PASSWORD))
        .await
        .unwrap();
    let made = device.create(RP, ORIGIN, &begun.challenge);
    is(
        w.access
            .finish_passkey_registration(
                &who,
                Some("c"),
                &begun.ceremony,
                None,
                &PasskeyAttestation {
                    client_data_json: made.client_data_json,
                    attestation_object: made.attestation_object,
                    transports: Vec::new(),
                },
            )
            .await,
        "passkey_already_registered",
    );
    assert!(!w.access.has_second_factor(&user("bob")).await.unwrap());
}

#[tokio::test]
async fn a_passkey_that_is_not_user_verified_is_refused() {
    let w = world();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    device.user_verified = false;
    let who = session("alice");
    let begun = w
        .access
        .begin_passkey_registration(&who, Some("c"), Some(PASSWORD))
        .await
        .unwrap();
    let made = device.create(RP, ORIGIN, &begun.challenge);
    is(
        w.access
            .finish_passkey_registration(
                &who,
                Some("c"),
                &begun.ceremony,
                None,
                &PasskeyAttestation {
                    client_data_json: made.client_data_json,
                    attestation_object: made.attestation_object,
                    transports: Vec::new(),
                },
            )
            .await,
        "passkey_user_verification_required",
    );
}

#[tokio::test]
async fn passkeys_are_renamed_and_removed_on_a_fresh_check() {
    let w = world();
    signup(&w, "alice").await;
    let who = session("alice");
    let first = register(&w, "alice", &SoftwareAuthenticator::new(1), Some(PASSWORD)).await;
    let id = first.passkey.id.clone();

    let renamed = w
        .access
        .rename_passkey(&who, &id, "  yubikey ")
        .await
        .unwrap();
    assert_eq!(renamed.name, "yubikey");
    is(
        w.access.rename_passkey(&who, &id, "  ").await,
        "invalid_request",
    );
    is(
        w.access.rename_passkey(&who, "zz", "x").await,
        "passkey_not_found",
    );
    is(
        w.access.rename_passkey(&session("bob"), &id, "x").await,
        "user_not_found",
    );

    is(
        w.access
            .remove_passkey(&who, Some("another-cookie"), &id)
            .await,
        "fresh_check_required",
    );
    w.access
        .remove_passkey(&who, Some("cookie"), &id)
        .await
        .unwrap();
    assert!(!w.access.has_second_factor(&user("alice")).await.unwrap());
    assert_eq!(
        w.store.unused_recovery_codes(&user("alice")).await.unwrap(),
        0,
        "the last second factor takes the recovery codes"
    );
    is(
        w.access.remove_passkey(&who, Some("cookie"), &id).await,
        "passkey_not_found",
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
    assert!(log.iter().any(|e| e.action == AuditAction::PasskeyRemoved));
}

#[tokio::test]
async fn a_passkey_meets_the_requirement_and_removing_it_unmeets_it() {
    let w = world();
    signup(&w, "root").await;
    w.store.set_admin(&user("root"), true).await.unwrap();
    signup(&w, "alice").await;
    w.access
        .set_server_two_factor(&session("root"), true, Some(0))
        .await
        .unwrap();
    is(
        w.access.enforce_two_factor(&session("alice")).await,
        "two_factor_enrolment_required",
    );
    let added = register(&w, "alice", &SoftwareAuthenticator::new(1), Some(PASSWORD)).await;
    w.access
        .enforce_two_factor(&session("alice"))
        .await
        .unwrap();
    assert!(
        w.access
            .unmet_two_factor_requirement(&user("alice"))
            .await
            .unwrap()
            .is_none()
    );

    w.access
        .remove_passkey(&session("alice"), Some("cookie"), &added.passkey.id)
        .await
        .unwrap();
    is(
        w.access.enforce_two_factor(&session("alice")).await,
        "two_factor_enrolment_required",
    );
}

#[tokio::test]
async fn a_passkey_makes_sensitive_actions_need_a_fresh_check_and_an_administrator_reset_removes_it()
 {
    let w = world();
    signup(&w, "root").await;
    w.store.set_admin(&user("root"), true).await.unwrap();
    signup(&w, "alice").await;
    register(&w, "alice", &SoftwareAuthenticator::new(1), Some(PASSWORD)).await;
    is(
        w.access
            .require_fresh(&session("alice"), Some("elsewhere"))
            .await,
        "fresh_check_required",
    );
    let removed = w
        .access
        .reset_two_factor(&session("root"), Some("r"), &user("alice"))
        .await
        .unwrap();
    assert_eq!(removed, 1);
    assert!(!w.access.has_second_factor(&user("alice")).await.unwrap());
    assert!(
        w.access
            .list_passkeys(&session("alice"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_account_holds_a_bounded_number_of_passkeys() {
    let w = world();
    signup(&w, "alice").await;
    register(&w, "alice", &SoftwareAuthenticator::new(1), Some(PASSWORD)).await;
    for seed in 2..=10 {
        register(&w, "alice", &SoftwareAuthenticator::new(seed), None).await;
    }
    is(
        w.access
            .begin_passkey_registration(&session("alice"), Some("cookie"), None)
            .await,
        "invalid_request",
    );
}

#[tokio::test]
async fn a_disabled_account_cannot_sign_in_with_a_passkey() {
    let w = world();
    signup(&w, "root").await;
    w.store.set_admin(&user("root"), true).await.unwrap();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    register(&w, "alice", &device, Some(PASSWORD)).await;
    w.access
        .disable_account(&session("root"), &user("alice"), None)
        .await
        .unwrap();
    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    is(
        w.access
            .finish_passkey_sign_in(&ask.ceremony, &answer(&mut device, &ask), false, None)
            .await,
        "account_disabled",
    );
}

#[tokio::test]
async fn staying_signed_in_lengthens_a_session_by_the_remember_setting() {
    let w = world();
    signup(&w, "alice").await;
    let mut device = SoftwareAuthenticator::new(1);
    let now = w.clock.now();

    let (usual, _) = w
        .access
        .start_session_with("alice", PASSWORD, None, false, None)
        .await
        .unwrap();
    assert_eq!(usual.expires_at, now + Duration::days(30));
    let (kept, _) = w
        .access
        .start_session_with("alice", PASSWORD, None, true, None)
        .await
        .unwrap();
    assert_eq!(kept.expires_at, now + Duration::days(90));

    register(&w, "alice", &device, Some(PASSWORD)).await;
    let ask = w.access.begin_passkey_sign_in(None, None).await.unwrap();
    let (by_key, cookie) = w
        .access
        .finish_passkey_sign_in(&ask.ceremony, &answer(&mut device, &ask), true, None)
        .await
        .unwrap();
    assert_eq!(by_key.expires_at, now + Duration::days(90));

    // Using the session never moves its end.
    w.clock.advance(Duration::days(40));
    let (_, again) = w.access.authenticate_session(&cookie).await.unwrap();
    assert_eq!(again.expires_at, by_key.expires_at);
}

#[tokio::test]
async fn the_remember_lifetime_is_never_shorter_than_the_usual_one() {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 11, 8, 9, 0, 0).unwrap(),
    ));
    let access = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        clock,
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(AccessConfig {
        session_days: 30,
        session_remember_days: 7,
        ..AccessConfig::default()
    });
    assert_eq!(access.session_lifetime(true), Duration::days(30));
    assert_eq!(access.session_lifetime(false), Duration::days(30));
}

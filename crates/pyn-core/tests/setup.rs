//! First-run setup: the token, the one-time administrator and the settings it records.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditAction, AuditQuery, AuditScope, AuditStore,
    Credential, Identity, ManualClock, PynError, Registration, RegistrationMode, SetupRequest,
    UserId,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

struct World {
    svc: AccessService,
    store: Arc<MemoryAccessStore>,
    audit: Arc<MemoryAuditStore>,
}

fn world() -> World {
    let store = Arc::new(MemoryAccessStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let svc = AccessService::new(store.clone(), clock)
        .with_config(AccessConfig {
            registration: RegistrationMode::Closed,
            ..AccessConfig::default()
        })
        .with_audit(audit.clone())
        .with_setup_token(TOKEN)
        .unwrap();
    World { svc, store, audit }
}

fn request<'a>(token: &'a str, username: &'a str) -> SetupRequest<'a> {
    SetupRequest {
        token,
        username,
        password: PASSWORD,
        email: None,
        server_name: None,
        public_url: None,
        registration: RegistrationMode::Open,
        client: Some("10.0.0.1"),
    }
}

#[tokio::test]
async fn a_fresh_server_is_uninitialised_and_reports_its_defaults() {
    let w = world();
    assert!(!w.svc.is_initialised().await.unwrap());
    let status = w.svc.setup_status().await.unwrap();
    assert!(!status.initialised && status.server_name.is_none());
    assert_eq!(status.registration, RegistrationMode::Closed);
}

#[tokio::test]
async fn without_a_setup_token_the_server_counts_as_initialised() {
    let svc = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        Arc::new(ManualClock::new(Utc::now())),
    );
    assert!(svc.is_initialised().await.unwrap());
    let err = svc
        .complete_setup(request(TOKEN, "root"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::AlreadyInitialised));
}

#[tokio::test]
async fn setup_creates_the_administrator_and_applies_the_settings() {
    let w = world();
    let mut req = request(TOKEN, "root");
    req.email = Some("Root@Example.org");
    req.server_name = Some("  Studio  ");
    req.public_url = Some("https://pyn.example/");
    let user = w.svc.complete_setup(req).await.unwrap();
    assert_eq!(user, UserId::new("root"));

    assert!(w.svc.is_initialised().await.unwrap());
    assert!(!w.svc.setup_token_pending());
    let status = w.svc.setup_status().await.unwrap();
    assert_eq!(status.server_name.as_deref(), Some("Studio"));
    assert_eq!(status.public_url, "https://pyn.example");
    assert_eq!(status.registration, RegistrationMode::Open);
    assert_eq!(w.svc.registration_mode(), RegistrationMode::Open);

    let account = w.store.account(&user).await.unwrap().unwrap();
    assert!(account.is_admin);
    assert_eq!(account.email.as_deref(), Some("root@example.org"));
    let admin = Identity {
        user: user.clone(),
        credential: Credential::Unrestricted,
    };
    assert!(w.svc.is_server_admin(&admin).await.unwrap());
    let signed_in = w.svc.login("root", PASSWORD, None).await;
    assert!(
        signed_in.is_ok(),
        "the administrator signs in with the password"
    );
}

#[tokio::test]
async fn setup_can_never_run_again() {
    let w = world();
    w.svc.complete_setup(request(TOKEN, "root")).await.unwrap();
    let err = w
        .svc
        .complete_setup(request(TOKEN, "another"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::AlreadyInitialised));
    assert!(
        w.store
            .account(&UserId::new("another"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_wrong_token_is_refused_and_creates_nothing() {
    let w = world();
    let err = w
        .svc
        .complete_setup(request("ffffffffffffffffffffffffffffffff", "root"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidSetupToken));
    assert!(!w.svc.is_initialised().await.unwrap());
    assert!(
        w.store
            .account(&UserId::new("root"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn repeated_wrong_tokens_lock_the_client_out_even_with_the_right_one() {
    let w = world();
    for _ in 0..10 {
        let err = w
            .svc
            .complete_setup(request("nope", "root"))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidSetupToken));
    }
    let err = w
        .svc
        .complete_setup(request(TOKEN, "root"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::TooManyAttempts { .. }));
    let mut elsewhere = request(TOKEN, "root");
    elsewhere.client = Some("10.0.0.2");
    w.svc.complete_setup(elsewhere).await.unwrap();
}

#[tokio::test]
async fn invalid_essentials_are_refused_without_spending_the_token() {
    let w = world();
    let mut weak = request(TOKEN, "root");
    weak.password = "short";
    assert!(matches!(
        w.svc.complete_setup(weak).await.unwrap_err(),
        PynError::InvalidRequest(_)
    ));
    let mut url = request(TOKEN, "root");
    url.public_url = Some("ftp://pyn.example");
    assert!(matches!(
        w.svc.complete_setup(url).await.unwrap_err(),
        PynError::InvalidRequest(_)
    ));
    let mut name = request(TOKEN, "root");
    let long = "x".repeat(101);
    name.server_name = Some(&long);
    assert!(matches!(
        w.svc.complete_setup(name).await.unwrap_err(),
        PynError::InvalidRequest(_)
    ));
    assert!(matches!(
        w.svc
            .complete_setup(request(TOKEN, "Not Valid"))
            .await
            .unwrap_err(),
        PynError::InvalidRequest(_)
    ));
    assert!(!w.svc.is_initialised().await.unwrap());
    w.svc.complete_setup(request(TOKEN, "root")).await.unwrap();
}

#[tokio::test]
async fn the_chosen_registration_mode_replaces_the_configured_one() {
    let w = world();
    assert_eq!(w.svc.registration_mode(), RegistrationMode::Closed);
    let err = w
        .svc
        .register(Registration::new("carol", PASSWORD))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::RegistrationClosed));
    let mut open = request(TOKEN, "root");
    open.registration = RegistrationMode::Open;
    w.svc.complete_setup(open).await.unwrap();
    let up = w
        .svc
        .register(Registration::new("carol", PASSWORD).email("carol@example.org"))
        .await
        .unwrap();
    assert_eq!(up.user, UserId::new("carol"));
}

#[tokio::test]
async fn setup_is_recorded_in_the_server_audit_log() {
    let w = world();
    w.svc.complete_setup(request(TOKEN, "root")).await.unwrap();
    let events = w
        .audit
        .list(
            &AuditScope::Server,
            &AuditQuery {
                limit: 10,
                ..AuditQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, AuditAction::ServerSetupCompleted);
    assert_eq!(events[0].actor, UserId::new("root"));
    assert!(!events[0].detail.contains(PASSWORD) && !events[0].detail.contains(TOKEN));
}

#[tokio::test]
async fn a_restarted_server_reads_setup_from_the_store() {
    let w = world();
    w.svc.complete_setup(request(TOKEN, "root")).await.unwrap();
    let restarted = AccessService::new(w.store.clone(), Arc::new(ManualClock::new(Utc::now())))
        .with_setup_token("another-token-of-enough-length")
        .unwrap();
    assert!(restarted.load_setup().await.unwrap());
    assert_eq!(restarted.registration_mode(), RegistrationMode::Open);
    let err = restarted
        .complete_setup(request("another-token-of-enough-length", "second"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::AlreadyInitialised));
}

#[test]
fn a_supplied_token_must_be_long_enough() {
    let svc = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        Arc::new(ManualClock::new(Utc::now())),
    );
    assert!(svc.with_setup_token("short").is_err());
}

//! Welcome and confirmation mail on every path that creates an account, and mail failures showing in the status.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use pyn_core::memory::MemoryAccessStore;
use pyn_core::{
    AccessConfig, AccessService, AccessStore, EmailMessage, EmailSender, ManualClock,
    MemoryEmailSender, NewServerInvitation, PynError, Registration, RegistrationMode, SetupRequest,
    UserId,
};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

struct World {
    svc: AccessService,
    store: Arc<MemoryAccessStore>,
    mail: Arc<MemoryEmailSender>,
}

fn world(mode: RegistrationMode, verification: bool, setup: bool) -> World {
    let store = Arc::new(MemoryAccessStore::new());
    let mail = Arc::new(MemoryEmailSender::new());
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let mut svc = AccessService::new(
        store.clone(),
        clock,
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(AccessConfig {
        registration: mode,
        public_url: "https://pyn.example".into(),
        require_email_verification: verification,
        ..AccessConfig::default()
    })
    .with_email(mail.clone());
    if setup {
        svc = svc.with_setup_token(TOKEN).unwrap();
    }
    World { svc, store, mail }
}

fn setup_request(email: Option<&str>) -> SetupRequest<'_> {
    SetupRequest {
        token: TOKEN,
        username: "root",
        password: PASSWORD,
        email,
        server_name: None,
        public_url: None,
        registration: RegistrationMode::Open,
        access_mode: None,
        client: None,
    }
}

fn token_in(body: &str) -> String {
    let (_, rest) = body.split_once("verify-email?token=").expect("a link");
    rest.split_whitespace().next().unwrap().to_string()
}

fn subjects(mail: &MemoryEmailSender) -> Vec<String> {
    mail.sent().into_iter().map(|m| m.subject).collect()
}

#[tokio::test]
async fn setup_mails_a_welcome_and_a_confirmation_when_verification_is_on() {
    let w = world(RegistrationMode::Closed, true, true);
    w.svc
        .complete_setup(setup_request(Some("Root@Example.org")))
        .await
        .unwrap();
    let sent = w.mail.sent();
    assert_eq!(
        subjects(&w.mail),
        ["Welcome to pyn", "Confirm your email address"]
    );
    assert!(sent.iter().all(|m| m.to == "root@example.org"));
    assert!(sent[0].body.contains("Hello root"));
    assert!(!sent[0].body.contains("is confirmed"));

    w.svc.verify_email(&token_in(&sent[1].body)).await.unwrap();
    let account = w
        .store
        .account(&UserId::new("root"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account.email.as_deref(), Some("root@example.org"));
    assert!(account.email_verified_at.is_some());
}

#[tokio::test]
async fn setup_mails_only_a_welcome_when_verification_is_off() {
    let w = world(RegistrationMode::Closed, false, true);
    w.svc
        .complete_setup(setup_request(Some("root@example.org")))
        .await
        .unwrap();
    assert_eq!(subjects(&w.mail), ["Welcome to pyn"]);
}

#[tokio::test]
async fn setup_without_an_address_or_a_mailer_sends_nothing() {
    let w = world(RegistrationMode::Closed, true, true);
    w.svc.complete_setup(setup_request(None)).await.unwrap();
    assert!(w.mail.sent().is_empty());

    let store = Arc::new(MemoryAccessStore::new());
    let svc = AccessService::new(
        store.clone(),
        Arc::new(ManualClock::new(Utc::now())),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_setup_token(TOKEN)
    .unwrap();
    svc.complete_setup(setup_request(Some("root@example.org")))
        .await
        .unwrap();
    let account = store.account(&UserId::new("root")).await.unwrap().unwrap();
    assert_eq!(account.email.as_deref(), Some("root@example.org"));
}

#[tokio::test]
async fn open_sign_up_without_verification_still_mails_a_welcome() {
    let w = world(RegistrationMode::Open, false, false);
    w.svc
        .register(Registration::new("maya", PASSWORD).email("maya@example.org"))
        .await
        .unwrap();
    let sent = w.mail.sent();
    assert_eq!(subjects(&w.mail), ["Welcome to pyn"]);
    assert_eq!(sent[0].to, "maya@example.org");

    w.svc
        .register(Registration::new("noaddress", PASSWORD))
        .await
        .unwrap();
    assert_eq!(w.mail.sent().len(), 1);
}

#[tokio::test]
async fn open_sign_up_with_verification_mails_the_link_then_the_welcome() {
    let w = world(RegistrationMode::Open, true, false);
    w.svc
        .register(Registration::new("maya", PASSWORD).email("maya@example.org"))
        .await
        .unwrap();
    assert_eq!(subjects(&w.mail), ["Confirm your email address"]);
    w.svc
        .verify_email(&token_in(&w.mail.sent()[0].body))
        .await
        .unwrap();
    assert_eq!(
        subjects(&w.mail),
        ["Confirm your email address", "Welcome to pyn"]
    );
}

#[tokio::test]
async fn an_invited_sign_up_with_an_address_gets_a_welcome() {
    let w = world(RegistrationMode::InviteOnly, false, false);
    w.svc.admin_for_tests(&UserId::new("root")).await.unwrap();
    let root = pyn_core::Identity {
        user: UserId::new("root"),
        credential: pyn_core::Credential::Session,
    };
    let made = w
        .svc
        .create_server_invitation(&root, NewServerInvitation::default())
        .await
        .unwrap();
    w.svc
        .register(
            Registration::new("alice", PASSWORD)
                .invite(&made.code)
                .email("alice@example.org"),
        )
        .await
        .unwrap();
    let sent = w.mail.sent();
    assert_eq!(subjects(&w.mail), ["Welcome to pyn"]);
    assert_eq!(sent[0].to, "alice@example.org");
}

struct Broken;

#[async_trait]
impl EmailSender for Broken {
    async fn send(&self, _message: EmailMessage) -> pyn_core::Result<()> {
        Err(PynError::EmailDeliveryFailed(
            "authentication failed".into(),
        ))
    }
}

#[tokio::test]
async fn a_failed_send_shows_in_the_status_and_a_later_success_is_recorded() {
    let w = world(RegistrationMode::Open, false, false);
    assert!(w.svc.email_status().last_failure.is_none());

    let svc = w.svc.with_email(Arc::new(Broken));
    svc.register(Registration::new("maya", PASSWORD).email("maya@example.org"))
        .await
        .expect("sign-up still succeeds");
    let status = svc.email_status();
    let failure = status.last_failure.expect("the failure is kept");
    assert!(failure.error.contains("authentication failed"));
    assert!(!failure.error.contains("maya@example.org"));
    assert!(status.last_sent_at.is_none());

    let ok = world(RegistrationMode::Open, false, false);
    ok.svc
        .register(Registration::new("maya", PASSWORD).email("maya@example.org"))
        .await
        .unwrap();
    assert!(ok.svc.email_status().last_sent_at.is_some());
}

struct Failing;

#[async_trait]
impl EmailSender for Failing {
    async fn send(&self, _message: EmailMessage) -> pyn_core::Result<()> {
        Err(PynError::EmailDeliveryFailed(
            "535 credentials invalid".into(),
        ))
    }

    async fn check(&self) -> Result<(), String> {
        Err("the mail server refused the user name or password".into())
    }
}

fn admin() -> pyn_core::Identity {
    pyn_core::Identity {
        user: UserId::new("root"),
        credential: pyn_core::Credential::Session,
    }
}

#[tokio::test]
async fn a_test_email_goes_to_the_administrators_verified_address_and_reports_the_outcome() {
    let w = world(RegistrationMode::Closed, true, true);
    w.svc
        .complete_setup(setup_request(Some("root@example.org")))
        .await
        .unwrap();
    let err = w.svc.send_test_email(&admin()).await.unwrap_err();
    assert!(
        matches!(err, PynError::InvalidRequest(_)),
        "no verified address yet: {err}"
    );

    let link = w.mail.sent().pop().unwrap();
    w.svc.verify_email(&token_in(&link.body)).await.unwrap();
    let sent = w.svc.send_test_email(&admin()).await.unwrap();
    assert!(sent.delivered && sent.error.is_none());
    let last = w.mail.sent().pop().unwrap();
    assert_eq!(
        (last.subject.as_str(), last.to.as_str()),
        ("pyn test email", "root@example.org")
    );
    assert!(w.svc.email_status().last_sent_at.is_some());
}

#[tokio::test]
async fn a_failing_mailer_shows_in_the_test_and_the_check() {
    let store = Arc::new(MemoryAccessStore::new());
    let svc = AccessService::new(
        store,
        Arc::new(ManualClock::new(
            Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
        )),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(AccessConfig {
        require_email_verification: false,
        ..AccessConfig::default()
    })
    .with_email(Arc::new(Failing))
    .with_setup_token(TOKEN)
    .unwrap();
    svc.complete_setup(setup_request(Some("root@example.org")))
        .await
        .unwrap();
    assert!(svc.email_status().last_check.is_none());
    let why = svc.check_mailer().await.unwrap_err();
    assert!(why.contains("user name or password"));
    let check = svc.email_status().last_check.unwrap();
    assert_eq!(check.error.as_deref(), Some(why.as_str()));

    assert!(
        svc.email_status().last_failure.is_some(),
        "the welcome already failed"
    );
}

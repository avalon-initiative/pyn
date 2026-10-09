//! Protection for open registration: email verification, approval, rate limits, disabling accounts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore};
use pyn_core::{
    AccessConfig, AccessService, AccountStatus, AuditAction, AuditQuery, AuditStore, Credential,
    Identity, InlinePasswords, ManualClock, MemoryEmailSender, NullEmailSender, PasswordWorker,
    PynError, RateLimits, Registration, RegistrationMode, UserId,
};

const PASSWORD: &str = "correct horse battery";

#[derive(Default)]
struct CountingPasswords {
    hashes: AtomicUsize,
    verifies: AtomicUsize,
}

#[async_trait]
impl PasswordWorker for CountingPasswords {
    async fn hash(&self, password: &str) -> pyn_core::Result<String> {
        self.hashes.fetch_add(1, Ordering::SeqCst);
        InlinePasswords.hash(password).await
    }

    async fn verify(&self, password: &str, hash: &str) -> bool {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        InlinePasswords.verify(password, hash).await
    }
}

struct World {
    svc: AccessService,
    clock: Arc<ManualClock>,
    mail: Arc<MemoryEmailSender>,
    passwords: Arc<CountingPasswords>,
    audit: Arc<MemoryAuditStore>,
}

fn world_with(config: impl FnOnce(&mut AccessConfig)) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let mail = Arc::new(MemoryEmailSender::new());
    let passwords = Arc::new(CountingPasswords::default());
    let audit = Arc::new(MemoryAuditStore::new());
    let mut cfg = AccessConfig {
        registration: RegistrationMode::Open,
        public_url: "https://pyn.example/".into(),
        ..AccessConfig::default()
    };
    config(&mut cfg);
    let svc = AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
        .with_config(cfg)
        .with_email(mail.clone())
        .with_passwords(passwords.clone())
        .with_audit(audit.clone());
    World {
        svc,
        clock,
        mail,
        passwords,
        audit,
    }
}

fn world() -> World {
    world_with(|_| {})
}

fn token_in(body: &str) -> String {
    let (_, rest) = body.split_once("verify-email?token=").expect("a link");
    rest.split_whitespace().next().unwrap().to_string()
}

fn last_token(w: &World) -> String {
    token_in(&w.mail.sent().last().expect("a message was sent").body)
}

fn identity(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

async fn admin(w: &World) -> Identity {
    w.svc.bootstrap_admin(&UserId::new("root")).await.unwrap();
    identity("root")
}

#[tokio::test]
async fn an_open_server_needs_an_email_address_to_sign_up() {
    let w = world();
    let err = w
        .svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    for bad in [
        "nobody",
        "a@b",
        "@example.org",
        "a b@example.org",
        "a@@example.org",
    ] {
        let err = w
            .svc
            .register(Registration::new("alice", PASSWORD).email(bad))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad}: {err}");
    }
    assert!(w.mail.sent().is_empty());
}

#[tokio::test]
async fn an_account_is_inactive_until_its_email_is_verified() {
    let w = world();
    let up = w
        .svc
        .register(Registration::new("alice", PASSWORD).email("Alice@Example.org "))
        .await
        .unwrap();
    assert_eq!(up.status, AccountStatus::PendingVerification);

    let sent = w.mail.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "alice@example.org");
    assert!(
        sent[0]
            .body
            .contains("https://pyn.example/verify-email?token=")
    );

    let err = w.svc.login("alice", PASSWORD, None).await.unwrap_err();
    assert!(
        matches!(
            err,
            PynError::AccountInactive(AccountStatus::PendingVerification)
        ),
        "{err}"
    );
    assert_eq!(err.code(), "email_not_verified");
    let wrong = w
        .svc
        .login("alice", "not the password", None)
        .await
        .unwrap_err();
    assert!(
        matches!(wrong, PynError::Unauthenticated(_)),
        "a wrong password does not reveal the account's state"
    );

    let verified = w.svc.verify_email(&last_token(&w)).await.unwrap();
    assert_eq!(verified.user, UserId::new("alice"));
    assert_eq!(verified.status, AccountStatus::Active);
    w.svc.login("alice", PASSWORD, None).await.unwrap();
}

#[tokio::test]
async fn a_verification_link_works_once_and_expires() {
    let w = world();
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
    let token = last_token(&w);
    w.svc.verify_email(&token).await.unwrap();
    let again = w.svc.verify_email(&token).await.unwrap_err();
    assert!(matches!(again, PynError::InvalidVerification(_)), "{again}");

    w.svc
        .register(Registration::new("bob", PASSWORD).email("bob@example.org"))
        .await
        .unwrap();
    let late = last_token(&w);
    w.clock.advance(Duration::hours(25));
    let err = w.svc.verify_email(&late).await.unwrap_err();
    assert!(matches!(err, PynError::InvalidVerification(_)), "{err}");

    for junk in ["", "nonsense", &"0".repeat(64)] {
        let err = w.svc.verify_email(junk).await.unwrap_err();
        assert_eq!(err.code(), "invalid_verification");
    }
}

#[tokio::test]
async fn an_unverified_account_gives_its_name_back_when_the_link_lapses() {
    let w = world();
    w.svc
        .register(Registration::new("alice", PASSWORD).email("squatter@example.org"))
        .await
        .unwrap();
    let err = w
        .svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserExists(_)), "{err}");

    w.clock.advance(Duration::hours(25));
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
}

#[tokio::test]
async fn signing_up_with_a_taken_address_looks_the_same_and_creates_nothing() {
    let w = world();
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
    w.svc.verify_email(&last_token(&w)).await.unwrap();
    let hashes_before = w.passwords.hashes.load(Ordering::SeqCst);
    let mails_before = w.mail.sent().len();

    let fresh = w
        .svc
        .register(Registration::new("mallory", PASSWORD).email("new@example.org"))
        .await
        .unwrap();
    let hashes_fresh = w.passwords.hashes.load(Ordering::SeqCst) - hashes_before;
    let taken = w
        .svc
        .register(Registration::new("eve", PASSWORD).email("ALICE@example.org"))
        .await
        .unwrap();
    let hashes_taken = w.passwords.hashes.load(Ordering::SeqCst) - hashes_before - hashes_fresh;

    assert_eq!(fresh.status, taken.status, "same status in the response");
    assert_eq!(hashes_fresh, 1);
    assert_eq!(hashes_taken, 1, "the taken path does the same hashing work");
    let sent = w.mail.sent();
    assert_eq!(
        sent.len() - mails_before,
        2,
        "both addresses get one message"
    );
    assert_eq!(sent.last().unwrap().to, "alice@example.org");
    assert!(
        !sent.last().unwrap().body.contains("verify-email"),
        "the owner gets a notice, never a link"
    );
    let err = w.svc.login("eve", PASSWORD, None).await.unwrap_err();
    assert!(
        matches!(err, PynError::Unauthenticated(_)),
        "no account was created: {err}"
    );
}

#[tokio::test]
async fn signing_in_costs_the_same_for_unknown_users_and_wrong_passwords() {
    let w = world_with(|c| c.require_email_verification = false);
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    w.svc.login("nobody", PASSWORD, None).await.unwrap_err();
    let verifies = w.passwords.verifies.load(Ordering::SeqCst);
    let unknown = w.svc.login("ghost", PASSWORD, None).await.unwrap_err();
    let after_unknown = w.passwords.verifies.load(Ordering::SeqCst);
    let wrong = w
        .svc
        .login("alice", "not the password", None)
        .await
        .unwrap_err();
    let after_wrong = w.passwords.verifies.load(Ordering::SeqCst);

    assert_eq!(after_unknown - verifies, 1);
    assert_eq!(after_wrong - after_unknown, 1);
    assert_eq!(unknown.to_string(), wrong.to_string());
    assert_eq!(unknown.code(), wrong.code());
}

#[tokio::test]
async fn verification_emails_to_one_address_are_limited() {
    let w = world();
    for name in ["a1", "a2", "a3", "a4"] {
        let up = w
            .svc
            .register(Registration::new(name, PASSWORD).email("target@example.org"))
            .await
            .unwrap();
        assert_eq!(up.status, AccountStatus::PendingVerification, "same answer");
    }
    assert_eq!(w.mail.sent().len(), 3, "the fourth is not sent");

    w.svc
        .resend_verification("target@example.org", None)
        .await
        .unwrap();
    assert_eq!(
        w.mail.sent().len(),
        3,
        "resend counts against the same limit"
    );

    w.clock.advance(Duration::hours(1));
    w.svc
        .resend_verification("target@example.org", None)
        .await
        .unwrap();
    assert_eq!(w.mail.sent().len(), 7, "one message per waiting account");
}

#[tokio::test]
async fn resending_replaces_the_old_link_and_says_nothing_about_unknown_addresses() {
    let w = world();
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
    let first = last_token(&w);

    w.svc
        .resend_verification("nobody@example.org", None)
        .await
        .unwrap();
    w.svc
        .resend_verification("not an address", None)
        .await
        .unwrap();
    assert_eq!(
        w.mail.sent().len(),
        1,
        "nothing is sent for an unknown address"
    );

    w.svc
        .resend_verification("Alice@example.org", None)
        .await
        .unwrap();
    assert_eq!(w.mail.sent().len(), 2);
    let second = last_token(&w);
    assert_ne!(first, second);
    assert!(
        w.svc.verify_email(&first).await.is_err(),
        "the old link is dead"
    );
    w.svc.verify_email(&second).await.unwrap();

    w.svc
        .resend_verification("alice@example.org", None)
        .await
        .unwrap();
    assert_eq!(
        w.mail.sent().len(),
        2,
        "a verified account is not mailed again"
    );
}

#[tokio::test]
async fn only_one_account_can_verify_an_address() {
    let w = world();
    w.svc
        .register(Registration::new("alice", PASSWORD).email("shared@example.org"))
        .await
        .unwrap();
    let alice = last_token(&w);
    w.svc
        .register(Registration::new("bob", PASSWORD).email("shared@example.org"))
        .await
        .unwrap();
    let bob = last_token(&w);
    w.svc.verify_email(&alice).await.unwrap();
    let err = w.svc.verify_email(&bob).await.unwrap_err();
    assert!(matches!(err, PynError::InvalidVerification(_)), "{err}");
}

#[tokio::test]
async fn sign_ups_from_one_client_address_are_limited() {
    let w = world_with(|c| c.require_email_verification = false);
    for i in 0..10 {
        w.svc
            .register(Registration::new(&format!("user{i}"), PASSWORD).client("203.0.113.9"))
            .await
            .unwrap();
    }
    let err = w
        .svc
        .register(Registration::new("user10", PASSWORD).client("203.0.113.9"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::TooManyAttempts { retry_after_secs } if retry_after_secs > 0),
        "{err}"
    );
    w.svc
        .register(Registration::new("user10", PASSWORD).client("198.51.100.1"))
        .await
        .unwrap();

    w.clock.advance(Duration::hours(1));
    w.svc
        .register(Registration::new("user11", PASSWORD).client("203.0.113.9"))
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_sign_ins_from_one_client_lock_that_client_out_whatever_the_user_name() {
    let w = world_with(|c| {
        c.require_email_verification = false;
        c.limits = RateLimits {
            sign_in_per_client: 3,
            ..RateLimits::default()
        };
    });
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    for name in ["x1", "x2", "x3"] {
        w.svc
            .login(name, "wrong password", Some("203.0.113.9"))
            .await
            .unwrap_err();
    }
    let err = w
        .svc
        .login("alice", PASSWORD, Some("203.0.113.9"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::TooManyAttempts { .. }), "{err}");
    w.svc
        .login("alice", PASSWORD, Some("198.51.100.1"))
        .await
        .unwrap();

    w.clock.advance(Duration::minutes(15));
    w.svc
        .login("alice", PASSWORD, Some("203.0.113.9"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_success_does_not_clear_the_client_address_count() {
    let w = world_with(|c| {
        c.require_email_verification = false;
        c.limits = RateLimits {
            sign_in_per_client: 2,
            ..RateLimits::default()
        };
    });
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    w.svc
        .login("x1", "wrong password", Some("203.0.113.9"))
        .await
        .unwrap_err();
    w.svc
        .login("alice", PASSWORD, Some("203.0.113.9"))
        .await
        .unwrap();
    w.svc
        .login("x2", "wrong password", Some("203.0.113.9"))
        .await
        .unwrap_err();
    let err = w
        .svc
        .login("alice", PASSWORD, Some("203.0.113.9"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::TooManyAttempts { .. }), "{err}");
}

#[tokio::test]
async fn approval_holds_a_verified_account_until_an_administrator_lets_it_in() {
    let w = world_with(|c| c.require_approval = true);
    let root = admin(&w).await;
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
    let up = w.svc.verify_email(&last_token(&w)).await.unwrap();
    assert_eq!(up.status, AccountStatus::PendingApproval);

    let err = w.svc.login("alice", PASSWORD, None).await.unwrap_err();
    assert_eq!(err.code(), "approval_pending");
    let waiting = w
        .svc
        .list_accounts(&root, Some(AccountStatus::PendingApproval), 10)
        .await
        .unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].user, UserId::new("alice"));

    let err = w
        .svc
        .approve_account(&identity("alice"), &UserId::new("alice"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServerAdminRequired), "{err}");

    let approved = w
        .svc
        .approve_account(&root, &UserId::new("alice"))
        .await
        .unwrap();
    assert_eq!(approved.status(), AccountStatus::Active);
    w.svc.login("alice", PASSWORD, None).await.unwrap();

    let err = w
        .svc
        .approve_account(&root, &UserId::new("alice"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let events = w.svc.server_audit(&root, &audit_query()).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, AuditAction::AccountApproved);
    assert_eq!(events[0].actor, UserId::new("root"));
}

#[tokio::test]
async fn approval_without_email_verification_applies_at_sign_up() {
    let w = world_with(|c| {
        c.require_email_verification = false;
        c.require_approval = true;
    });
    let up = w
        .svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    assert_eq!(up.status, AccountStatus::PendingApproval);
    assert_eq!(
        w.svc
            .login("alice", PASSWORD, None)
            .await
            .unwrap_err()
            .code(),
        "approval_pending"
    );
}

fn audit_query() -> AuditQuery {
    AuditQuery {
        limit: 50,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_disabled_account_loses_every_way_in_and_can_be_enabled_again() {
    let w = world_with(|c| c.require_email_verification = false);
    let root = admin(&w).await;
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let (_, token) = w.svc.login("alice", PASSWORD, None).await.unwrap();
    let (_, cookie) = w.svc.start_session("alice", PASSWORD, None).await.unwrap();
    w.svc.identify(&token).await.unwrap();

    let alice = UserId::new("alice");
    let disabled = w
        .svc
        .disable_account(&root, &alice, Some("  spam  "))
        .await
        .unwrap();
    assert_eq!(disabled.status(), AccountStatus::Disabled);
    assert_eq!(disabled.disabled_reason.as_deref(), Some("spam"));

    assert_eq!(
        w.svc.identify(&token).await.unwrap_err().code(),
        "account_disabled"
    );
    assert!(
        w.svc.authenticate_session(&cookie).await.is_err(),
        "the session is gone"
    );
    assert_eq!(
        w.svc
            .login("alice", PASSWORD, None)
            .await
            .unwrap_err()
            .code(),
        "account_disabled"
    );
    assert_eq!(
        w.svc
            .login("alice", "not the password", None)
            .await
            .unwrap_err()
            .code(),
        "unauthenticated",
        "a wrong password still learns nothing"
    );

    let enabled = w.svc.enable_account(&root, &alice).await.unwrap();
    assert_eq!(enabled.status(), AccountStatus::Active);
    assert!(enabled.disabled_reason.is_none());
    w.svc.identify(&token).await.unwrap();
    w.svc.login("alice", PASSWORD, None).await.unwrap();

    let events = w.svc.server_audit(&root, &audit_query()).await.unwrap();
    let actions: Vec<_> = events.iter().map(|e| e.action).collect();
    assert_eq!(
        actions,
        [AuditAction::AccountEnabled, AuditAction::AccountDisabled]
    );
    assert!(events[1].detail.contains("spam"));
    assert!(
        w.audit
            .list(&pyn_core::RepoId::new("@server"), &audit_query())
            .await
            .unwrap()
            .len()
            == 2
    );
}

#[tokio::test]
async fn disabling_is_for_server_administrators_and_never_for_oneself() {
    let w = world_with(|c| c.require_email_verification = false);
    let root = admin(&w).await;
    for name in ["alice", "bob"] {
        w.svc
            .register(Registration::new(name, PASSWORD))
            .await
            .unwrap();
    }
    let alice = UserId::new("alice");
    let bob = UserId::new("bob");

    let err = w
        .svc
        .disable_account(&identity("bob"), &alice, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
    assert!(
        w.svc
            .list_accounts(&identity("bob"), None, 10)
            .await
            .is_err()
    );
    assert!(
        w.svc
            .server_audit(&identity("bob"), &audit_query())
            .await
            .is_err()
    );
    assert!(w.svc.enable_account(&identity("bob"), &bob).await.is_err());

    let err = w
        .svc
        .disable_account(&root, &UserId::new("root"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = w
        .svc
        .disable_account(&root, &UserId::new("nobody"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserNotFound(_)), "{err}");
    let err = w.svc.enable_account(&root, &alice).await.unwrap_err();
    assert!(
        matches!(err, PynError::InvalidRequest(_)),
        "not disabled: {err}"
    );

    assert!(w.svc.is_server_admin(&root).await.unwrap());
    assert!(!w.svc.is_server_admin(&identity("bob")).await.unwrap());
}

#[tokio::test]
async fn invite_only_servers_need_no_email_and_skip_verification() {
    let w = world_with(|c| c.registration = RegistrationMode::InviteOnly);
    let root = admin(&w).await;
    let repo = pyn_core::RepoId::new("game");
    w.svc
        .add_creator(&repo, &UserId::new("root"))
        .await
        .unwrap();
    let principal = w.svc.principal(&repo, &root.user).await.unwrap();
    let (_, code) = w
        .svc
        .create_invite(&principal, &repo, pyn_core::Role::Writer, Duration::days(1))
        .await
        .unwrap();
    let up = w
        .svc
        .register(Registration::new("alice", PASSWORD).invite(&code))
        .await
        .unwrap();
    assert_eq!(up.status, AccountStatus::Active);
    assert!(w.mail.sent().is_empty());
    w.svc.login("alice", PASSWORD, None).await.unwrap();
}

#[tokio::test]
async fn passwords_have_an_upper_bound() {
    let w = world_with(|c| c.require_email_verification = false);
    let long = "x".repeat(257);
    let err = w
        .svc
        .register(Registration::new("alice", &long))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    assert_eq!(
        w.passwords.hashes.load(Ordering::SeqCst),
        0,
        "rejected before hashing"
    );
}

#[tokio::test]
async fn an_open_server_without_protection_warns() {
    let open = || AccessConfig {
        registration: RegistrationMode::Open,
        ..AccessConfig::default()
    };
    let service = |config: AccessConfig, mail: Arc<dyn pyn_core::EmailSender>| {
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            Arc::new(ManualClock::new(Utc::now())),
        )
        .with_config(config)
        .with_email(mail)
    };

    let log_only = service(open(), Arc::new(NullEmailSender)).startup_warnings();
    assert!(log_only[0].contains("registration is open"), "{log_only:?}");
    assert!(
        log_only.iter().any(|m| m.contains("no email is delivered")),
        "{log_only:?}"
    );

    let off = service(
        AccessConfig {
            require_email_verification: false,
            ..open()
        },
        Arc::new(MemoryEmailSender::new()),
    )
    .startup_warnings();
    assert!(
        off.iter().any(|m| m.contains("email verification is off")),
        "{off:?}"
    );

    let protected = service(open(), Arc::new(MemoryEmailSender::new())).startup_warnings();
    assert_eq!(protected.len(), 1, "only the notice that it is open");

    for mode in [RegistrationMode::InviteOnly, RegistrationMode::Closed] {
        let config = AccessConfig {
            registration: mode,
            require_email_verification: false,
            ..AccessConfig::default()
        };
        assert!(
            service(config, Arc::new(NullEmailSender))
                .startup_warnings()
                .is_empty()
        );
    }
}

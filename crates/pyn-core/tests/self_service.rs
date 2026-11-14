//! Account self-service (#982, decisions #983 and #986): email addresses and username changes, through the
//! services over the in-memory stores.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccountStatus, AuditAction, AuditQuery, AuditScope, AuditStore,
    Credential, Identity, ManualClock, MemoryEmailSender, PynError, Registration, RegistrationMode,
    Repositories, Rules, UserId,
};

const PASSWORD: &str = "correct horse battery";

struct World {
    access: Arc<AccessService>,
    repos: Repositories,
    mail: Arc<MemoryEmailSender>,
    audit: Arc<MemoryAuditStore>,
    clock: Arc<ManualClock>,
}

fn world_with(config: impl FnOnce(&mut AccessConfig)) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let mail = Arc::new(MemoryEmailSender::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let mut cfg = AccessConfig {
        registration: RegistrationMode::Open,
        public_url: "https://pyn.example".into(),
        ..AccessConfig::default()
    };
    config(&mut cfg);
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(cfg)
        .with_email(mail.clone())
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Repositories::new(
        meta,
        Arc::new(MemoryObjectStore::new()),
        audit.clone(),
        access.clone(),
        clock.clone(),
        Rules::empty(),
    );
    World {
        access,
        repos,
        mail,
        audit,
        clock,
    }
}

fn world() -> World {
    world_with(|_| {})
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

fn link_token(body: &str) -> String {
    let (_, rest) = body.split_once("verify-email?token=").expect("a link");
    rest.split_whitespace().next().unwrap().to_string()
}

fn last_token(w: &World) -> String {
    link_token(&w.mail.sent().last().expect("a message").body)
}

/// Signs up with an address and finishes verification.
async fn signup(w: &World, name: &str, address: &str) {
    w.access
        .register(Registration::new(name, PASSWORD).email(address))
        .await
        .unwrap();
    w.access.verify_email(&last_token(w)).await.unwrap();
}

async fn server_log(w: &World) -> Vec<(String, String, String)> {
    w.audit
        .list(
            &AuditScope::Server,
            &AuditQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .rev()
        .map(|e| (e.action.to_string(), e.actor.to_string(), e.detail))
        .collect()
}

#[tokio::test]
async fn an_added_address_waits_for_its_link_and_then_is_verified_but_not_primary() {
    let w = world();
    signup(&w, "alice", "a@example.org").await;
    let before = w.mail.sent().len();

    w.access
        .add_email(&session("alice"), " Second@Example.ORG ")
        .await
        .unwrap();
    let sent = w.mail.sent();
    assert_eq!(sent.len(), before + 1);
    assert_eq!(
        sent[before].to, "second@example.org",
        "addresses are lowercased"
    );
    assert!(
        sent[before]
            .body
            .contains("https://pyn.example/verify-email?token=")
    );

    let held = w.access.list_emails(&session("alice")).await.unwrap();
    assert_eq!(held.len(), 2);
    assert!(held[0].primary && held[0].address == "a@example.org");
    assert!(
        held[1].verified_at.is_none(),
        "pending until the link is used"
    );

    let signed = w.access.verify_email(&last_token(&w)).await.unwrap();
    assert_eq!(
        (signed.user, signed.status),
        (user("alice"), AccountStatus::Active)
    );
    let held = w.access.list_emails(&session("alice")).await.unwrap();
    assert!(held[1].verified_at.is_some() && !held[1].primary);
    let err = w.access.verify_email(&last_token(&w)).await.unwrap_err();
    assert!(
        matches!(err, PynError::InvalidVerification(_)),
        "a link works once: {err}"
    );
}

#[tokio::test]
async fn an_address_another_account_holds_or_has_pending_is_refused_with_one_generic_error() {
    let w = world();
    signup(&w, "alice", "a@example.org").await;
    signup(&w, "bob", "b@example.org").await;
    w.access
        .add_email(&session("bob"), "pending@example.org")
        .await
        .unwrap();
    let sent = w.mail.sent().len();

    let mut errors = Vec::new();
    for address in ["B@example.org", "pending@example.org"] {
        let err = w
            .access
            .add_email(&session("alice"), address)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::EmailUnavailable), "{err}");
        errors.push((err.code(), err.to_string()));
    }
    assert_eq!(errors[0], errors[1], "verified and pending read the same");
    assert_eq!(
        w.mail.sent().len(),
        sent,
        "nothing is mailed for a refused address"
    );
    assert_eq!(
        w.access.list_emails(&session("alice")).await.unwrap().len(),
        1
    );

    let own = w
        .access
        .add_email(&session("alice"), "a@example.org")
        .await
        .unwrap_err();
    assert!(matches!(own, PynError::EmailRefused(_)), "{own}");
}

#[tokio::test]
async fn only_a_verified_address_can_be_primary() {
    let w = world();
    signup(&w, "alice", "a@example.org").await;
    let alice = session("alice");
    w.access.add_email(&alice, "b@example.org").await.unwrap();

    let err = w
        .access
        .set_primary_email(&alice, "b@example.org")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::EmailRefused(_)), "{err}");
    let err = w
        .access
        .set_primary_email(&alice, "zzz@example.org")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::EmailNotFound), "{err}");

    w.access.verify_email(&last_token(&w)).await.unwrap();
    w.access
        .set_primary_email(&alice, "B@example.org")
        .await
        .unwrap();
    let held = w.access.list_emails(&alice).await.unwrap();
    assert_eq!(
        (held[0].address.as_str(), held[0].primary),
        ("b@example.org", true)
    );
    assert_eq!(held[1].address, "a@example.org");
}

#[tokio::test]
async fn the_last_verified_address_stays_where_the_server_requires_one() {
    let w = world();
    signup(&w, "alice", "a@example.org").await;
    let alice = session("alice");
    let err = w
        .access
        .remove_email(&alice, "a@example.org")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::EmailRefused(_)), "{err}");

    w.access.add_email(&alice, "b@example.org").await.unwrap();
    w.access.verify_email(&last_token(&w)).await.unwrap();
    w.access
        .remove_email(&alice, "a@example.org")
        .await
        .unwrap();
    let held = w.access.list_emails(&alice).await.unwrap();
    assert_eq!(held.len(), 1);
    assert!(held[0].primary, "the other verified address took over");
    let err = w
        .access
        .remove_email(&alice, "b@example.org")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::EmailRefused(_)), "{err}");
    let err = w
        .access
        .remove_email(&alice, "zzz@example.org")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::EmailNotFound), "{err}");

    let relaxed = world_with(|c| c.require_email_verification = false);
    relaxed
        .access
        .register(Registration::new("carol", PASSWORD).email("c@example.org"))
        .await
        .unwrap();
    relaxed
        .access
        .add_email(&session("carol"), "d@example.org")
        .await
        .unwrap();
    relaxed
        .access
        .remove_email(&session("carol"), "c@example.org")
        .await
        .unwrap();
}

#[tokio::test]
async fn a_removed_address_is_free_for_anyone_and_a_resend_sends_a_new_link() {
    let w = world();
    signup(&w, "alice", "a@example.org").await;
    signup(&w, "bob", "b@example.org").await;
    let alice = session("alice");
    w.access.add_email(&alice, "x@example.org").await.unwrap();
    let first = last_token(&w);
    w.access
        .resend_email(&alice, "x@example.org")
        .await
        .unwrap();
    assert_ne!(last_token(&w), first, "a resend is a fresh link");
    let err = w
        .access
        .resend_email(&alice, "a@example.org")
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::EmailNotFound),
        "a verified address needs none: {err}"
    );

    w.access
        .remove_email(&alice, "x@example.org")
        .await
        .unwrap();
    w.access
        .add_email(&session("bob"), "x@example.org")
        .await
        .unwrap();
    let err = w.access.verify_email(&first).await;
    assert!(err.is_err(), "the old link is dead");
}

#[tokio::test]
async fn address_changes_are_audited() {
    let w = world();
    signup(&w, "alice", "a@example.org").await;
    let alice = session("alice");
    w.access.add_email(&alice, "b@example.org").await.unwrap();
    w.access.verify_email(&last_token(&w)).await.unwrap();
    w.access
        .set_primary_email(&alice, "b@example.org")
        .await
        .unwrap();
    w.access
        .remove_email(&alice, "a@example.org")
        .await
        .unwrap();
    let actions: Vec<_> = server_log(&w)
        .await
        .into_iter()
        .map(|(a, _, _)| a)
        .collect();
    assert_eq!(
        actions,
        [
            "email_added",
            "email_verified",
            "email_primary_changed",
            "email_removed"
        ]
    );
}

async fn alice_with_repos(w: &World) {
    signup(w, "alice", "a@example.org").await;
    for name in ["game", "tools"] {
        w.repos
            .create(&session("alice"), &user("alice"), name, None, None)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_rename_needs_the_password_a_free_name_and_an_unfrozen_account() {
    let w = world();
    alice_with_repos(&w).await;
    signup(&w, "bob", "b@example.org").await;
    w.access.create_org(&session("bob"), "acme").await.unwrap();
    let alice = session("alice");

    let err = w
        .access
        .rename_account(&alice, "alicia", Some("wrong password!"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    let err = w
        .access
        .rename_account(&alice, "alicia", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    for taken in ["bob", "acme"] {
        let err = w
            .access
            .rename_account(&alice, taken, Some(PASSWORD))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::UserExists(_)), "{taken}: {err}");
    }
    for bad in ["alice", "A", "Alicia", "admin"] {
        let err = w
            .access
            .rename_account(&alice, bad, Some(PASSWORD))
            .await
            .unwrap_err();
        assert!(
            matches!(err, PynError::InvalidRequest(_) | PynError::ReservedName(_)),
            "{bad}: {err}"
        );
    }

    w.access.admin_for_tests(&user("root")).await.unwrap();
    w.access
        .freeze_owner(&session("root"), &user("alice"), "plan lapsed")
        .await
        .unwrap();
    let err = w
        .access
        .rename_account(&alice, "alicia", Some(PASSWORD))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::OwnerFrozen { .. }), "{err}");
    assert!(
        w.repos.open("alice", "game").await.is_ok()
            && w.repos.moved_to("alice", "game").await.unwrap().is_none(),
        "a refusal changes nothing"
    );
}

#[tokio::test]
async fn a_rename_keeps_the_account_signed_in_and_its_repositories_reachable() {
    let w = world();
    alice_with_repos(&w).await;
    let (_, token) = w.access.login("alice", PASSWORD, None).await.unwrap();
    let before = w.access.identify(&token).await.unwrap();
    assert_eq!(before.user, user("alice"));

    let done = w
        .access
        .rename_account(&session("alice"), "alicia", Some(PASSWORD))
        .await
        .unwrap();
    assert_eq!((done.old.as_str(), done.new.as_str()), ("alice", "alicia"));
    assert_eq!(done.repositories, ["game", "tools"]);

    let after = w.access.identify(&token).await.unwrap();
    assert_eq!(
        after.user,
        user("alicia"),
        "a token stays valid and follows the account"
    );
    assert!(w.access.login("alicia", PASSWORD, None).await.is_ok());
    assert!(w.access.login("alice", PASSWORD, None).await.is_err());

    let direct = w.repos.open("alicia", "game").await.unwrap();
    assert_eq!(direct.record.address(), "alicia/game");
    let old = w.repos.open("alice", "Game").await.unwrap();
    assert_eq!(
        old.record.id, direct.record.id,
        "the old address serves the renamed repository"
    );
    assert_eq!(
        w.repos.moved_to("alice", "game").await.unwrap().as_deref(),
        Some("alicia/game")
    );
    assert_eq!(w.repos.moved_to("alicia", "game").await.unwrap(), None);
    assert!(matches!(
        w.repos
            .open("alice", "nothing")
            .await
            .map(|_| ())
            .unwrap_err(),
        PynError::RepoNotFound(_)
    ));
}

#[tokio::test]
async fn a_live_repository_under_the_old_name_wins_over_the_redirect_one_repository_at_a_time() {
    let w = world();
    alice_with_repos(&w).await;
    w.access
        .rename_account(&session("alice"), "alicia", Some(PASSWORD))
        .await
        .unwrap();

    signup(&w, "alice", "new@example.org").await;
    let theirs = w
        .repos
        .create(&session("alice"), &user("alice"), "game", None, None)
        .await
        .unwrap();
    let got = w.repos.open("alice", "game").await.unwrap();
    assert_eq!(
        got.record.id, theirs.id,
        "the new holder's repository answers"
    );
    assert_eq!(w.repos.moved_to("alice", "game").await.unwrap(), None);

    let kept = w.repos.open("alice", "tools").await.unwrap();
    assert_eq!(
        kept.record.address(),
        "alicia/tools",
        "the other address still redirects"
    );
    assert_eq!(
        w.repos.moved_to("alice", "tools").await.unwrap().as_deref(),
        Some("alicia/tools")
    );
}

#[tokio::test]
async fn a_private_repository_stays_hidden_behind_its_redirect() {
    let w = world();
    alice_with_repos(&w).await;
    signup(&w, "bob", "b@example.org").await;
    w.access
        .rename_account(&session("alice"), "alicia", Some(PASSWORD))
        .await
        .unwrap();
    let bob = session("bob");
    let err = w
        .repos
        .open_visible("alice", "game", Some(&bob))
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(err, PynError::RepoNotFound(_)), "{err}");
    let mine = session("alicia");
    assert!(
        w.repos
            .open_visible("alice", "game", Some(&mine))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_rename_is_audited_with_both_names_and_the_old_name_is_not_reserved() {
    let w = world();
    alice_with_repos(&w).await;
    w.clock.advance(chrono::Duration::minutes(5));
    w.access
        .rename_account(&session("alice"), "alicia", Some(PASSWORD))
        .await
        .unwrap();
    let log = server_log(&w).await;
    let (action, actor, detail) = log.last().unwrap();
    assert_eq!(
        (action.as_str(), actor.as_str(), detail.as_str()),
        ("account_renamed", "alice", "alice renamed to alicia")
    );
    assert_eq!(AuditAction::AccountRenamed.to_string(), "account_renamed");

    signup(&w, "alice", "x@example.org").await;
    let again = w
        .access
        .rename_account(&session("alicia"), "alice", Some(PASSWORD))
        .await;
    assert!(matches!(again, Err(PynError::UserExists(_))), "{again:?}");

    let spans = w
        .access
        .current_names(&[(
            user("alice"),
            Utc.with_ymd_and_hms(2026, 10, 8, 9, 1, 0).unwrap(),
        )])
        .await
        .unwrap();
    assert_eq!(
        spans,
        [Some(user("alicia"))],
        "history written as alice shows the current name"
    );
    let after = w
        .access
        .current_names(&[(
            user("alice"),
            Utc.with_ymd_and_hms(2026, 10, 8, 9, 6, 0).unwrap(),
        )])
        .await
        .unwrap();
    assert_eq!(after, [None], "later entries by the new alice are theirs");
}

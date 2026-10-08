use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::MemoryAccessStore;
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Clock, ManualClock, Permission, Principal, PynError,
    RegistrationMode, RepoId, Role, UserId,
};

struct World {
    svc: AccessService,
    clock: Arc<ManualClock>,
    repo: RepoId,
}

fn world(mode: RegistrationMode) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let config = AccessConfig {
        registration: mode,
        ..AccessConfig::default()
    };
    let svc =
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone()).with_config(config);
    World {
        svc,
        clock,
        repo: RepoId::new("game"),
    }
}

const PASSWORD: &str = "correct horse battery";

async fn admin(w: &World) -> Principal {
    w.svc
        .bootstrap_admin(&w.repo, &UserId::new("root"))
        .await
        .unwrap();
    w.svc
        .principal(&w.repo, &UserId::new("root"))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_closed_server_refuses_registration_but_administrators_can_add_people() {
    let w = world(RegistrationMode::Closed);
    let err = w
        .svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::RegistrationClosed), "{err}");

    let root = admin(&w).await;
    w.svc
        .add_user(&root, &w.repo, "alice", PASSWORD, Role::Writer)
        .await
        .unwrap();
    let (_, token) = w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap();
    let who = w.svc.authenticate(&w.repo, &token).await.unwrap();
    assert!(who.has(Permission::Checkin) && !who.has(Permission::Restore));
}

#[tokio::test]
async fn an_open_server_lets_anyone_register_as_a_reader() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap();
    let (_, token) = w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap();
    let who = w.svc.authenticate(&w.repo, &token).await.unwrap();
    assert_eq!(who.permissions, [Permission::Read].into());

    let err = w
        .svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserExists(_)), "{err}");
}

#[tokio::test]
async fn an_invite_only_server_needs_a_valid_one_time_invitation() {
    let w = world(RegistrationMode::InviteOnly);
    let root = admin(&w).await;
    let err = w
        .svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::InvalidInvite(_)),
        "no invitation: {err}"
    );

    let (record, code) = w
        .svc
        .create_invite(&root, &w.repo, Role::Writer, Duration::days(2))
        .await
        .unwrap();
    assert_ne!(record.secret_hash, code, "the code itself is never stored");
    let tampered = format!("{}0", &code[..code.len() - 1]);
    let tampered = if tampered == code {
        format!("{}1", &code[..code.len() - 1])
    } else {
        tampered
    };
    let err = w
        .svc
        .register(&w.repo, "alice", PASSWORD, Some(&tampered))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::InvalidInvite(_)),
        "wrong secret: {err}"
    );

    w.svc
        .register(&w.repo, "alice", PASSWORD, Some(&code))
        .await
        .unwrap();
    let (_, token) = w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap();
    assert!(
        w.svc
            .authenticate(&w.repo, &token)
            .await
            .unwrap()
            .has(Permission::Checkin),
        "the invitation's role applies"
    );

    let err = w
        .svc
        .register(&w.repo, "bob", PASSWORD, Some(&code))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("already been used"),
        "single use: {err}"
    );
}

#[tokio::test]
async fn invitations_expire_and_can_be_revoked() {
    let w = world(RegistrationMode::InviteOnly);
    let root = admin(&w).await;
    let (_, expiring) = w
        .svc
        .create_invite(&root, &w.repo, Role::Reader, Duration::hours(1))
        .await
        .unwrap();
    let (revoked, revoked_code) = w
        .svc
        .create_invite(&root, &w.repo, Role::Reader, Duration::days(1))
        .await
        .unwrap();
    w.svc.revoke_invite(&root, &revoked.id).await.unwrap();
    w.clock.advance(Duration::hours(2));

    let err = w
        .svc
        .register(&w.repo, "alice", PASSWORD, Some(&expiring))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
    let err = w
        .svc
        .register(&w.repo, "bob", PASSWORD, Some(&revoked_code))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("revoked"), "{err}");
    assert_eq!(w.svc.list_invites(&root, &w.repo).await.unwrap().len(), 2);
}

#[tokio::test]
async fn only_administrators_manage_invitations_and_nobody_invites_above_their_own_level() {
    let w = world(RegistrationMode::InviteOnly);
    let root = admin(&w).await;
    w.svc
        .add_user(&root, &w.repo, "wendy", PASSWORD, Role::Writer)
        .await
        .unwrap();
    let wendy = w
        .svc
        .principal(&w.repo, &UserId::new("wendy"))
        .await
        .unwrap();
    let err = w
        .svc
        .create_invite(&wendy, &w.repo, Role::Reader, Duration::days(1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::ManageUsers)),
        "{err}"
    );
    let err = w
        .svc
        .add_user(&wendy, &w.repo, "x1", PASSWORD, Role::Reader)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::ManageUsers)),
        "{err}"
    );

    let err = w
        .svc
        .create_invite(&root, &w.repo, Role::Reader, Duration::zero())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

#[tokio::test]
async fn user_names_and_passwords_must_be_acceptable() {
    let w = world(RegistrationMode::Open);
    for bad in [
        "",
        "a",
        "Alice",
        "al ice",
        "-alice",
        "al/ice",
        &"a".repeat(40),
    ] {
        let err = w
            .svc
            .register(&w.repo, bad, PASSWORD, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad:?}: {err}");
    }
    let err = w
        .svc
        .register(&w.repo, "alice", "short", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    w.svc
        .register(&w.repo, "al-ice_9", PASSWORD, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn signing_in_needs_the_right_password_and_does_not_reveal_which_part_was_wrong() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap();
    let wrong = w
        .svc
        .login(&w.repo, "alice", "not the password")
        .await
        .unwrap_err();
    let unknown = w.svc.login(&w.repo, "nobody", PASSWORD).await.unwrap_err();
    assert!(
        matches!(wrong, PynError::Unauthenticated(_))
            && matches!(unknown, PynError::Unauthenticated(_))
    );
    assert_eq!(wrong.to_string(), unknown.to_string());
}

#[tokio::test]
async fn repeated_failures_lock_the_name_out_for_a_while_and_success_resets_the_count() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap();
    for _ in 0..4 {
        w.svc
            .login(&w.repo, "alice", "wrong password")
            .await
            .unwrap_err();
    }
    w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap();

    for _ in 0..5 {
        w.svc
            .login(&w.repo, "alice", "wrong password")
            .await
            .unwrap_err();
    }
    let err = w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap_err();
    assert!(
        matches!(err, PynError::TooManyAttempts { retry_after_secs } if retry_after_secs > 0),
        "{err}"
    );
    let err = w.svc.login(&w.repo, "ALICE", PASSWORD).await.unwrap_err();
    assert!(
        matches!(err, PynError::TooManyAttempts { .. }),
        "case does not dodge it: {err}"
    );

    w.clock.advance(Duration::minutes(16));
    w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap();
}

#[tokio::test]
async fn a_sign_in_expires_and_follows_the_users_role() {
    let w = world(RegistrationMode::Open);
    let root = admin(&w).await;
    w.svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap();
    let (record, token) = w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap();
    assert_eq!(record.name, "sign-in");

    w.svc
        .set_user_role(&root, &w.repo, &UserId::new("alice"), Role::Writer)
        .await
        .unwrap();
    assert_eq!(
        w.svc
            .authenticate(&w.repo, &token)
            .await
            .unwrap()
            .permissions,
        [Permission::Read].into(),
        "a session never exceeds what it was issued with"
    );

    w.clock.advance(Duration::days(31));
    let err = w.svc.authenticate(&w.repo, &token).await.unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
}

#[tokio::test]
async fn an_account_with_no_role_cannot_sign_in() {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let store = Arc::new(MemoryAccessStore::new());
    let svc = AccessService::new(store.clone(), clock.clone());
    let ghost = UserId::new("ghost");
    AccessStore::create_user(&*store, &ghost, clock.now())
        .await
        .unwrap();
    svc.set_password_for_operator(&ghost, PASSWORD)
        .await
        .unwrap();
    let err = svc
        .login(&RepoId::new("game"), "ghost", PASSWORD)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no access"), "{err}");
}

#[tokio::test]
async fn changing_a_password_needs_the_current_one() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(&w.repo, "alice", PASSWORD, None)
        .await
        .unwrap();
    let alice = w
        .svc
        .principal(&w.repo, &UserId::new("alice"))
        .await
        .unwrap();

    let err = w
        .svc
        .change_password(&alice, Some("not the password"), "another long password")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    let err = w
        .svc
        .change_password(&alice, None, "another long password")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    let err = w
        .svc
        .change_password(&alice, Some(PASSWORD), "short")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");

    w.svc
        .change_password(&alice, Some(PASSWORD), "another long password")
        .await
        .unwrap();
    w.svc
        .login(&w.repo, "alice", "another long password")
        .await
        .unwrap();
    w.svc.login(&w.repo, "alice", PASSWORD).await.unwrap_err();
}

#[tokio::test]
async fn a_session_follows_the_current_role_and_ends_on_sign_out_or_expiry() {
    let w = world(RegistrationMode::Closed);
    let root = admin(&w).await;
    w.svc
        .add_user(&root, &w.repo, "alice", PASSWORD, Role::Writer)
        .await
        .unwrap();

    let err = w
        .svc
        .start_session(&w.repo, "alice", "not the password")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");

    let (record, cookie) = w
        .svc
        .start_session(&w.repo, "alice", PASSWORD)
        .await
        .unwrap();
    assert_ne!(record.id_hash, cookie, "only a hash is stored");
    let (who, _) = w.svc.authenticate_session(&w.repo, &cookie).await.unwrap();
    assert!(who.has(Permission::Checkin));

    w.svc
        .set_user_role(&root, &w.repo, &UserId::new("alice"), Role::Reader)
        .await
        .unwrap();
    let (who, _) = w.svc.authenticate_session(&w.repo, &cookie).await.unwrap();
    assert_eq!(
        who.permissions,
        [Permission::Read].into(),
        "role applies at once"
    );

    w.svc.end_session(&cookie).await.unwrap();
    w.svc.end_session(&cookie).await.unwrap();
    let err = w
        .svc
        .authenticate_session(&w.repo, &cookie)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");

    let (_, cookie) = w
        .svc
        .start_session(&w.repo, "alice", PASSWORD)
        .await
        .unwrap();
    w.clock.advance(Duration::days(31));
    assert!(w.svc.find_session(&cookie).await.unwrap().is_none());
}

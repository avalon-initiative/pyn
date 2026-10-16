use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::MemoryAccessStore;
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Clock, Credential, Identity, ManualClock, Permission,
    Principal, PynError, Registration, RegistrationMode, RepoId, Role, UserId,
};

struct World {
    svc: AccessService,
    store: Arc<MemoryAccessStore>,
    clock: Arc<ManualClock>,
    repo: RepoId,
}

fn world(mode: RegistrationMode) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let config = AccessConfig {
        registration: mode,
        require_email_verification: false,
        ..AccessConfig::default()
    };
    let store = Arc::new(MemoryAccessStore::new());
    let svc = AccessService::new(store.clone(), clock.clone()).with_config(config);
    World {
        svc,
        store,
        clock,
        repo: RepoId::new("game"),
    }
}

const PASSWORD: &str = "correct horse battery";

fn identity(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

async fn admin(w: &World) -> Principal {
    w.svc.admin_for_tests(&UserId::new("root")).await.unwrap();
    w.svc
        .add_creator(&w.repo, &UserId::new("root"))
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
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::RegistrationClosed), "{err}");

    let root = admin(&w).await;
    w.svc
        .add_user(&root, &w.repo, "alice", PASSWORD, Role::Writer)
        .await
        .unwrap();
    let (_, token) = w.svc.login("alice", PASSWORD, None).await.unwrap();
    let who = w.svc.authenticate(&w.repo, &token).await.unwrap();
    assert!(who.has(Permission::Checkin) && !who.has(Permission::Restore));
}

#[tokio::test]
async fn an_open_server_lets_anyone_register_but_the_account_has_no_repository_access() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let (_, token) = w.svc.login("alice", PASSWORD, None).await.unwrap();
    let who = w.svc.authenticate(&w.repo, &token).await.unwrap();
    assert!(who.permissions.is_empty());

    let err = w
        .svc
        .register(Registration::new("alice", PASSWORD))
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
        .register(Registration::new("alice", PASSWORD))
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
        .register(Registration::new("alice", PASSWORD).invite(&tampered))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::InvalidInvite(_)),
        "wrong secret: {err}"
    );

    w.svc
        .register(Registration::new("alice", PASSWORD).invite(&code))
        .await
        .unwrap();
    let (_, token) = w.svc.login("alice", PASSWORD, None).await.unwrap();
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
        .register(Registration::new("bob", PASSWORD).invite(&code))
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
    w.svc
        .revoke_invite(&root, &w.repo, &revoked.id)
        .await
        .unwrap();
    w.clock.advance(Duration::hours(2));

    let err = w
        .svc
        .register(Registration::new("alice", PASSWORD).invite(&expiring))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
    let err = w
        .svc
        .register(Registration::new("bob", PASSWORD).invite(&revoked_code))
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
            .register(Registration::new(bad, PASSWORD))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad:?}: {err}");
    }
    let err = w
        .svc
        .register(Registration::new("alice", "short"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    w.svc
        .register(Registration::new("al-ice_9", PASSWORD))
        .await
        .unwrap();
}

#[tokio::test]
async fn signing_in_needs_the_right_password_and_does_not_reveal_which_part_was_wrong() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let wrong = w
        .svc
        .login("alice", "not the password", None)
        .await
        .unwrap_err();
    let unknown = w.svc.login("nobody", PASSWORD, None).await.unwrap_err();
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
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    for _ in 0..4 {
        w.svc
            .login("alice", "wrong password", None)
            .await
            .unwrap_err();
    }
    w.svc.login("alice", PASSWORD, None).await.unwrap();

    for _ in 0..5 {
        w.svc
            .login("alice", "wrong password", None)
            .await
            .unwrap_err();
    }
    let err = w.svc.login("alice", PASSWORD, None).await.unwrap_err();
    assert!(
        matches!(err, PynError::TooManyAttempts { retry_after_secs } if retry_after_secs > 0),
        "{err}"
    );
    let err = w.svc.login("ALICE", PASSWORD, None).await.unwrap_err();
    assert!(
        matches!(err, PynError::TooManyAttempts { .. }),
        "case does not dodge it: {err}"
    );

    w.clock.advance(Duration::minutes(16));
    w.svc.login("alice", PASSWORD, None).await.unwrap();
}

#[tokio::test]
async fn a_sign_in_expires_and_follows_the_users_role_in_each_repository() {
    let w = world(RegistrationMode::Open);
    let root = admin(&w).await;
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let (record, token) = w.svc.login("alice", PASSWORD, None).await.unwrap();
    assert_eq!(record.name, "sign-in");
    assert!(
        record.repos.is_empty(),
        "a sign-in reaches every repository"
    );

    let alice = UserId::new("alice");
    w.svc
        .set_user_role(&root, &w.repo, &alice, Role::Reader)
        .await
        .unwrap();
    assert_eq!(
        w.svc
            .authenticate(&w.repo, &token)
            .await
            .unwrap()
            .permissions,
        [Permission::Read].into()
    );
    w.svc
        .set_user_role(&root, &w.repo, &alice, Role::Writer)
        .await
        .unwrap();
    assert!(
        w.svc
            .authenticate(&w.repo, &token)
            .await
            .unwrap()
            .has(Permission::Checkin)
    );
    assert!(
        w.svc
            .authenticate(&RepoId::new("elsewhere"), &token)
            .await
            .unwrap()
            .permissions
            .is_empty()
    );

    w.clock.advance(Duration::days(31));
    let err = w.svc.authenticate(&w.repo, &token).await.unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
}

#[tokio::test]
async fn an_account_with_no_role_can_sign_in_but_holds_no_permissions() {
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
    let (_, token) = svc.login("ghost", PASSWORD, None).await.unwrap();
    let who = svc
        .authenticate(&RepoId::new("game"), &token)
        .await
        .unwrap();
    assert!(who.permissions.is_empty());
}

#[tokio::test]
async fn changing_a_password_needs_the_current_one() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let alice = identity("alice");

    let err = w
        .svc
        .change_password(
            &alice,
            Some("not the password"),
            "another long password",
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    let err = w
        .svc
        .change_password(&alice, None, "another long password", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    let err = w
        .svc
        .change_password(&alice, Some(PASSWORD), "short", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");

    w.svc
        .change_password(&alice, Some(PASSWORD), "another long password", None)
        .await
        .unwrap();
    w.svc
        .login("alice", "another long password", None)
        .await
        .unwrap();
    w.svc.login("alice", PASSWORD, None).await.unwrap_err();
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
        .start_session("alice", "not the password", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");

    let (record, cookie) = w.svc.start_session("alice", PASSWORD, None).await.unwrap();
    assert_ne!(record.id_hash, cookie, "only a hash is stored");
    let (who, _) = w.svc.authenticate_session(&cookie).await.unwrap();
    let who = w.svc.principal_in(&w.repo, &who).await.unwrap();
    assert!(who.has(Permission::Checkin));

    w.svc
        .set_user_role(&root, &w.repo, &UserId::new("alice"), Role::Reader)
        .await
        .unwrap();
    let (who, _) = w.svc.authenticate_session(&cookie).await.unwrap();
    let who = w.svc.principal_in(&w.repo, &who).await.unwrap();
    assert_eq!(
        who.permissions,
        [Permission::Read].into(),
        "role applies at once"
    );

    w.svc.end_session(&cookie).await.unwrap();
    w.svc.end_session(&cookie).await.unwrap();
    let err = w.svc.authenticate_session(&cookie).await.unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");

    let (_, cookie) = w.svc.start_session("alice", PASSWORD, None).await.unwrap();
    w.clock.advance(Duration::days(31));
    assert!(w.svc.find_session(&cookie).await.unwrap().is_none());
}

#[tokio::test]
async fn changing_a_password_ends_the_other_sessions_and_keeps_the_current_one() {
    let w = world(RegistrationMode::Open);
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let (_, here) = w.svc.start_session("alice", PASSWORD, None).await.unwrap();
    let (_, elsewhere) = w.svc.start_session("alice", PASSWORD, None).await.unwrap();

    w.svc
        .change_password(
            &identity("alice"),
            Some(PASSWORD),
            "another long password",
            Some(&here),
        )
        .await
        .unwrap();
    assert!(w.svc.find_session(&here).await.unwrap().is_some());
    assert!(w.svc.find_session(&elsewhere).await.unwrap().is_none());
}

async fn alice_with_a_created_token(w: &World) -> (String, String) {
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    w.svc
        .add_creator(&w.repo, &UserId::new("alice"))
        .await
        .unwrap();
    let (_, created) = w
        .svc
        .create_token(
            &identity("alice"),
            "ci",
            [Permission::Read].into(),
            vec![w.repo.clone()],
            None,
        )
        .await
        .unwrap();
    let (_, sign_in) = w.svc.login("alice", PASSWORD, None).await.unwrap();
    (sign_in, created)
}

#[tokio::test]
async fn a_password_change_ends_sign_in_tokens_but_not_created_ones() {
    let w = world(RegistrationMode::Open);
    let (sign_in, created) = alice_with_a_created_token(&w).await;
    w.svc
        .change_password(
            &identity("alice"),
            Some(PASSWORD),
            "another long password",
            None,
        )
        .await
        .unwrap();
    assert!(w.svc.identify(&sign_in).await.is_err());
    assert!(w.svc.authenticate(&w.repo, &created).await.is_ok());
}

#[tokio::test]
async fn an_operator_reset_ends_sessions_and_sign_in_tokens() {
    let w = world(RegistrationMode::Open);
    let (sign_in, created) = alice_with_a_created_token(&w).await;
    let (_, cookie) = w.svc.start_session("alice", PASSWORD, None).await.unwrap();
    w.svc
        .set_password_for_operator(&UserId::new("alice"), "another long password")
        .await
        .unwrap();
    assert!(w.svc.identify(&sign_in).await.is_err());
    assert!(w.svc.find_session(&cookie).await.unwrap().is_none());
    assert!(w.svc.authenticate(&w.repo, &created).await.is_ok());
}

#[tokio::test]
async fn a_first_password_is_set_only_from_a_web_session() {
    let w = world(RegistrationMode::Open);
    let user = UserId::new("ghost");
    AccessStore::create_user(&*w.store, &user, w.clock.now())
        .await
        .unwrap();
    let full_token = Identity {
        user: user.clone(),
        credential: Credential::Token(pyn_core::TokenRecord {
            id: pyn_core::TokenId("tk".into()),
            user: user.clone(),
            name: "t".into(),
            secret_hash: String::new(),
            permissions: Permission::ALL.into(),
            repos: Vec::new(),
            created_at: w.clock.now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            sign_in: true,
        }),
    };
    let err = w
        .svc
        .change_password(&full_token, None, "a long enough password", None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    w.svc
        .change_password(&identity("ghost"), None, "a long enough password", None)
        .await
        .unwrap();
}

async fn repo_admin_not_server_admin(w: &World) -> Principal {
    let root = admin(w).await;
    w.svc
        .add_user(&root, &w.repo, "wendy", PASSWORD, Role::Admin)
        .await
        .unwrap();
    w.svc
        .principal(&w.repo, &UserId::new("wendy"))
        .await
        .unwrap()
}

#[tokio::test]
async fn adding_accounts_follows_the_registration_policy() {
    for mode in [RegistrationMode::Closed, RegistrationMode::InviteOnly] {
        let w = world(mode);
        let wendy = repo_admin_not_server_admin(&w).await;
        let err = w
            .svc
            .add_user(&wendy, &w.repo, "x1", PASSWORD, Role::Reader)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::Forbidden(_)), "{mode:?}: {err}");
    }

    let w = world(RegistrationMode::Open);
    let wendy = repo_admin_not_server_admin(&w).await;
    w.svc
        .add_user(&wendy, &w.repo, "x1", PASSWORD, Role::Reader)
        .await
        .unwrap();

    for (approval, verification) in [(true, false), (false, true)] {
        let mut w = world(RegistrationMode::Open);
        let wendy = repo_admin_not_server_admin(&w).await;
        w.svc = AccessService::new(w.store.clone(), w.clock.clone()).with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_approval: approval,
            require_email_verification: verification,
            ..AccessConfig::default()
        });
        let err = w
            .svc
            .add_user(&wendy, &w.repo, "x2", PASSWORD, Role::Reader)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::Forbidden(_)), "{err}");
        let root = w
            .svc
            .principal(&w.repo, &UserId::new("root"))
            .await
            .unwrap();
        w.svc
            .add_user(&root, &w.repo, "x3", PASSWORD, Role::Reader)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn adding_accounts_as_a_non_admin_is_rate_limited() {
    let w = world(RegistrationMode::Open);
    let wendy = repo_admin_not_server_admin(&w).await;
    let max = AccessConfig::default().limits.register_per_client;
    let mut refused = false;
    for i in 0..=max {
        let r = w
            .svc
            .add_user(&wendy, &w.repo, &format!("u{i}"), PASSWORD, Role::Reader)
            .await;
        if matches!(r, Err(PynError::TooManyAttempts { .. })) {
            refused = true;
        }
    }
    assert!(refused);
}

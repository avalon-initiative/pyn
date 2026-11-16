//! Creating an invitation is server-wide account creation, so it needs a full-scope credential.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::MemoryAccessStore;
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Clock, Credential, Identity, ManualClock, Permission,
    PynError, RegistrationMode, RepoId, Role, ServiceScope, TokenId, TokenRecord, UserId,
};

struct World {
    svc: AccessService,
    store: Arc<MemoryAccessStore>,
    clock: Arc<ManualClock>,
    repo: RepoId,
}

async fn world(mode: RegistrationMode, require_approval: bool) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let store = Arc::new(MemoryAccessStore::new());
    let svc = AccessService::new(
        store.clone(),
        clock.clone(),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(AccessConfig {
        registration: mode,
        require_approval,
        require_email_verification: false,
        ..AccessConfig::default()
    });
    let w = World {
        svc,
        store,
        clock,
        repo: RepoId::new("game"),
    };
    let root = UserId::new("root");
    w.svc.admin_for_tests(&root).await.unwrap();
    w.svc.add_creator(&w.repo, &root).await.unwrap();
    for name in ["wendy", "boss", "carol"] {
        let user = UserId::new(name);
        w.store.ensure_user(&user, w.clock.now()).await.unwrap();
        w.svc.add_creator(&w.repo, &user).await.unwrap();
    }
    w.svc.admin_for_tests(&UserId::new("boss")).await.unwrap();
    w
}

fn session(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

fn token(w: &World, user: &str, permissions: &[Permission], repos: Vec<RepoId>) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Token(TokenRecord {
            id: TokenId("tk".into()),
            user: UserId::new(user),
            name: "t".into(),
            secret_hash: String::new(),
            permissions: permissions.iter().copied().collect(),
            repos,
            created_at: w.clock.now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            sign_in: false,
        }),
    }
}

async fn invite(w: &World, who: &Identity) -> pyn_core::Result<()> {
    w.svc
        .create_invite(who, &w.repo, Role::Reader, Duration::days(1))
        .await
        .map(|_| ())
}

#[tokio::test]
async fn creating_an_invitation_needs_a_full_scope_credential_in_every_server_config() {
    for (mode, approval) in [
        (RegistrationMode::Open, false),
        (RegistrationMode::Closed, false),
        (RegistrationMode::InviteOnly, false),
        (RegistrationMode::Open, true),
    ] {
        let w = world(mode, approval).await;
        let all = Permission::ALL;
        let manage = [Permission::Read, Permission::ManageUsers];
        let repo = vec![w.repo.clone()];

        let refused = [
            (
                "scoped token, normal user",
                token(&w, "wendy", &all, repo.clone()),
            ),
            (
                "scoped token, administrator",
                token(&w, "boss", &all, repo.clone()),
            ),
            (
                "narrow token, normal user",
                token(&w, "wendy", &manage, Vec::new()),
            ),
            (
                "narrow token, administrator",
                token(&w, "boss", &manage, Vec::new()),
            ),
        ];
        for (what, who) in refused {
            let err = invite(&w, &who).await.unwrap_err();
            assert!(
                matches!(err, PynError::Forbidden(_)),
                "{mode:?} {what}: {err}"
            );
        }

        let (_, secret) = w
            .svc
            .create_service_credential(
                &session("root"),
                "svc",
                [ServiceScope::ManageAccounts].into(),
            )
            .await
            .unwrap();
        let service = w.svc.identify(&secret).await.unwrap();
        assert!(matches!(service.credential, Credential::Service(_)));
        let err = invite(&w, &service).await.unwrap_err();
        assert!(
            matches!(err, PynError::ServiceCredentialNotAllowed),
            "{mode:?}: {err}"
        );
        assert!(
            w.store.list_invites(&w.repo).await.unwrap().is_empty(),
            "{mode:?}: a refused call created an invitation"
        );

        let allowed = [
            ("session of a member", session("wendy")),
            ("full-scope token", token(&w, "wendy", &all, Vec::new())),
            ("administrator session", session("boss")),
            (
                "administrator full-scope token",
                token(&w, "boss", &all, Vec::new()),
            ),
            (
                "dev identity",
                Identity {
                    user: UserId::new("dev"),
                    credential: Credential::Unrestricted,
                },
            ),
        ];
        let n = allowed.len();
        for (what, who) in allowed {
            invite(&w, &who)
                .await
                .unwrap_or_else(|e| panic!("{mode:?} {what}: {e}"));
        }
        assert_eq!(w.store.list_invites(&w.repo).await.unwrap().len(), n);
    }
}

#[tokio::test]
async fn a_full_scope_member_below_the_role_cannot_invite_above_their_own_level() {
    let w = world(RegistrationMode::InviteOnly, false).await;
    w.svc
        .set_user_role(
            &w.svc.principal_in(&w.repo, &session("root")).await.unwrap(),
            &w.repo,
            &UserId::new("wendy"),
            Role::Writer,
        )
        .await
        .unwrap();
    let err = invite(&w, &session("wendy")).await.unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::ManageUsers)),
        "{err}"
    );
}

#[tokio::test]
async fn a_scoped_token_cannot_list_or_revoke_a_members_keys_and_tokens() {
    let w = world(RegistrationMode::Open, false).await;
    let (carol, mine) = (UserId::new("carol"), session("wendy"));
    w.svc
        .set_user_role(
            &w.svc.principal_in(&w.repo, &mine).await.unwrap(),
            &w.repo,
            &carol,
            Role::Reader,
        )
        .await
        .unwrap();
    let scoped = token(&w, "wendy", &Permission::ALL, vec![w.repo.clone()]);
    let (record, _) = w
        .svc
        .create_token(
            &session("carol"),
            "t",
            [Permission::Read].into(),
            vec![w.repo.clone()],
            None,
        )
        .await
        .unwrap();
    assert!(w.svc.list_ssh_keys(&mine, &carol).await.is_ok());
    assert!(w.svc.list_tokens(&mine, &carol).await.is_ok());
    for err in [
        w.svc.list_ssh_keys(&scoped, &carol).await.unwrap_err(),
        w.svc.list_tokens(&scoped, &carol).await.unwrap_err(),
        w.svc.revoke_token(&scoped, &record.id).await.unwrap_err(),
    ] {
        assert!(matches!(err, PynError::Forbidden(_)), "{err}");
    }
}

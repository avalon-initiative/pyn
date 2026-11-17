use std::collections::BTreeSet;
use std::sync::Arc;

use Permission::*;
use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore};
use pyn_core::{
    AccessService, AccessStore, AuditQuery, AuditScope, AuditStore, Credential, Identity,
    ManualClock, Permission, Principal, PynError, RepoId, Role, UserId,
};

struct World {
    svc: AccessService,
    clock: Arc<ManualClock>,
    repo: RepoId,
}

async fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let svc = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        clock.clone(),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    );
    World {
        svc,
        clock,
        repo: RepoId::new("game"),
    }
}

fn user(s: &str) -> UserId {
    UserId::new(s)
}

fn perms(p: &[Permission]) -> BTreeSet<Permission> {
    p.iter().copied().collect()
}

fn session(name: &str) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Session,
    }
}

async fn admin(w: &World) -> Principal {
    w.svc.admin_for_tests(&user("root")).await.unwrap();
    w.svc.add_creator(&w.repo, &user("root")).await.unwrap();
    w.svc.principal(&w.repo, &user("root")).await.unwrap()
}

async fn accounts(w: &World, names: &[&str]) {
    for name in names {
        w.svc.account_for_tests(&user(name)).await.unwrap();
    }
}

#[tokio::test]
async fn default_roles_grow_in_capability() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["r", "w", "m"]).await;
    for (name, role) in [
        ("r", Role::Reader),
        ("w", Role::Writer),
        ("m", Role::Maintainer),
    ] {
        w.svc
            .set_user_role(&root, &w.repo, &user(name), role)
            .await
            .unwrap();
    }
    let has = |p: &Principal, x| p.has(x);
    let reader = w.svc.principal(&w.repo, &user("r")).await.unwrap();
    let writer = w.svc.principal(&w.repo, &user("w")).await.unwrap();
    let maint = w.svc.principal(&w.repo, &user("m")).await.unwrap();
    assert!(has(&reader, Read) && !has(&reader, Lock));
    assert!(has(&writer, Lock) && has(&writer, Checkin) && !has(&writer, Restore));
    assert!(
        has(&maint, Restore)
            && has(&maint, ForceUnlock)
            && has(&maint, EditPolicy)
            && !has(&maint, ManageUsers)
    );
    assert_eq!(root.permissions.len(), Permission::ALL.len());
}

#[tokio::test]
async fn a_token_authenticates_with_the_intersection_of_token_and_role() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["wendy"]).await;
    w.svc
        .set_user_role(&root, &w.repo, &user("wendy"), Role::Writer)
        .await
        .unwrap();

    let (record, full) = w
        .svc
        .create_token(
            &session("wendy"),
            "ci",
            perms(&[Read, Checkin]),
            vec![w.repo.clone()],
            None,
        )
        .await
        .unwrap();
    assert_ne!(
        record.secret_hash, full,
        "the secret itself is never stored"
    );
    let p = w.svc.authenticate(&w.repo, &full).await.unwrap();
    assert_eq!(
        (p.user, p.permissions),
        (user("wendy"), perms(&[Read, Checkin]))
    );

    w.svc
        .set_user_role(&root, &w.repo, &user("wendy"), Role::Reader)
        .await
        .unwrap();
    let demoted = w.svc.authenticate(&w.repo, &full).await.unwrap();
    assert_eq!(
        demoted.permissions,
        perms(&[Read]),
        "demoting the user narrows existing tokens"
    );
}

#[tokio::test]
async fn a_token_cannot_hold_more_than_its_owner() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["wendy"]).await;
    w.svc
        .set_user_role(&root, &w.repo, &user("wendy"), Role::Writer)
        .await
        .unwrap();
    let wendy = session("wendy");
    let repos = vec![w.repo.clone()];
    let err = w
        .svc
        .create_token(
            &wendy,
            "sneaky",
            perms(&[Read, Restore]),
            repos.clone(),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Forbidden(Restore)), "{err}");
    let err = w
        .svc
        .create_token(&wendy, "empty", perms(&[]), repos, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = w
        .svc
        .create_token(&wendy, "nowhere", perms(&[Read]), vec![], None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = w
        .svc
        .create_token(
            &wendy,
            "elsewhere",
            perms(&[Read]),
            vec![RepoId::new("other")],
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Read)),
        "a token cannot reach a repository its owner is not in: {err}"
    );
}

#[tokio::test]
async fn bad_revoked_expired_and_out_of_scope_tokens_are_rejected() {
    let w = world().await;
    admin(&w).await;
    let root = session("root");
    w.svc
        .add_creator(&RepoId::new("elsewhere"), &user("root"))
        .await
        .unwrap();
    for raw in ["", "pyn_x", "Bearer abc", "pyn_aaaaaaaaaaaa_short"] {
        let err = w.svc.authenticate(&w.repo, raw).await.unwrap_err();
        assert!(
            matches!(err, PynError::Unauthenticated(_)),
            "{raw:?}: {err}"
        );
    }

    let (_, full) = w
        .svc
        .create_token(
            &root,
            "t",
            perms(&[Read]),
            vec![w.repo.clone()],
            Some(Utc.with_ymd_and_hms(2026, 10, 8, 10, 0, 0).unwrap()),
        )
        .await
        .unwrap();
    let flipped = if full.ends_with('0') { '1' } else { '0' };
    let tampered = format!("{}{flipped}", &full[..full.len() - 1]);
    let err = w.svc.authenticate(&w.repo, &tampered).await.unwrap_err();
    assert!(
        matches!(err, PynError::Unauthenticated(_)),
        "wrong secret: {err}"
    );

    w.svc.authenticate(&w.repo, &full).await.unwrap();
    w.clock.advance(Duration::hours(2));
    let err = w.svc.authenticate(&w.repo, &full).await.unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");

    let (rec2, full2) = w
        .svc
        .create_token(&root, "t2", perms(&[Read]), vec![w.repo.clone()], None)
        .await
        .unwrap();
    w.svc.revoke_token(&root, &rec2.id).await.unwrap();
    let err = w.svc.authenticate(&w.repo, &full2).await.unwrap_err();
    assert!(err.to_string().contains("revoked"), "{err}");

    let (_, scoped) = w
        .svc
        .create_token(
            &root,
            "scoped",
            perms(&[Read]),
            vec![RepoId::new("elsewhere")],
            None,
        )
        .await
        .unwrap();
    let err = w.svc.authenticate(&w.repo, &scoped).await.unwrap_err();
    assert!(err.to_string().contains("repository"), "{err}");
}

#[tokio::test]
async fn tokens_belong_to_their_owner_unless_an_admin_steps_in() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["alice", "bob"]).await;
    for name in ["alice", "bob"] {
        w.svc
            .set_user_role(&root, &w.repo, &user(name), Role::Writer)
            .await
            .unwrap();
    }
    let (alice, bob, root) = (session("alice"), session("bob"), session("root"));
    let (rec, _) = w
        .svc
        .create_token(&alice, "mine", perms(&[Read]), vec![w.repo.clone()], None)
        .await
        .unwrap();

    assert_eq!(
        w.svc
            .list_tokens(&alice, &user("alice"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        w.svc.list_tokens(&bob, &user("alice")).await,
        Err(PynError::Forbidden(ManageUsers))
    ));
    assert!(matches!(
        w.svc.revoke_token(&bob, &rec.id).await,
        Err(PynError::TokenNotFound(_))
    ));
    assert_eq!(
        w.svc
            .list_tokens(&root, &user("alice"))
            .await
            .unwrap()
            .len(),
        1
    );
    w.svc.revoke_token(&root, &rec.id).await.unwrap();
    let unknown = pyn_core::TokenId("ffffffffffff".into());
    assert!(matches!(
        w.svc.revoke_token(&root, &unknown).await,
        Err(PynError::TokenNotFound(_))
    ));
}

#[tokio::test]
async fn only_admins_manage_users_and_nobody_grants_more_than_they_hold() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["maya", "new", "sly"]).await;
    w.svc
        .set_user_role(&root, &w.repo, &user("maya"), Role::Maintainer)
        .await
        .unwrap();
    let maya = w.svc.principal(&w.repo, &user("maya")).await.unwrap();
    let err = w
        .svc
        .set_user_role(&maya, &w.repo, &user("new"), Role::Reader)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Forbidden(ManageUsers)), "{err}");

    w.svc
        .set_role_permissions(
            &root,
            &w.repo,
            Role::Maintainer,
            perms(&[
                Read,
                Lock,
                Checkin,
                Restore,
                ForceUnlock,
                EditPolicy,
                ManageUsers,
            ]),
        )
        .await
        .unwrap();
    let maya = w.svc.principal(&w.repo, &user("maya")).await.unwrap();
    w.svc
        .set_user_role(&maya, &w.repo, &user("new"), Role::Writer)
        .await
        .unwrap();
    let err = w
        .svc
        .set_user_role(&maya, &w.repo, &user("sly"), Role::Admin)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(ManageRoles)),
        "cannot grant admin without holding it: {err}"
    );
}

#[tokio::test]
async fn the_owner_can_adjust_roles_but_admin_keeps_access_management() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["wendy"]).await;
    w.svc
        .set_user_role(&root, &w.repo, &user("wendy"), Role::Writer)
        .await
        .unwrap();
    w.svc
        .set_role_permissions(
            &root,
            &w.repo,
            Role::Writer,
            perms(&[Read, Lock, Checkin, Restore]),
        )
        .await
        .unwrap();
    let wendy = w.svc.principal(&w.repo, &user("wendy")).await.unwrap();
    assert!(wendy.has(Restore));

    let err = w
        .svc
        .set_role_permissions(&wendy, &w.repo, Role::Reader, perms(&[Read]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Forbidden(ManageRoles)), "{err}");
    let err = w
        .svc
        .set_role_permissions(&root, &w.repo, Role::Admin, perms(&[Read, Lock]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

#[tokio::test]
async fn role_definitions_need_a_role_in_the_repository() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["r"]).await;
    w.svc
        .set_user_role(&root, &w.repo, &user("r"), Role::Reader)
        .await
        .unwrap();
    let reader = w.svc.principal(&w.repo, &user("r")).await.unwrap();
    assert!(
        w.svc
            .role_definitions(&reader, &w.repo, "a/game")
            .await
            .is_ok()
    );

    let outsider = Principal {
        user: user("x"),
        permissions: perms(&[Read]),
        email: None,
    };
    assert!(matches!(
        w.svc.role_definitions(&outsider, &w.repo, "a/game").await,
        Err(PynError::NotRepoMember(_))
    ));
}

struct Audited {
    svc: AccessService,
    store: Arc<MemoryAccessStore>,
    audit: Arc<MemoryAuditStore>,
    repo: RepoId,
}

async fn audited() -> Audited {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let store = Arc::new(MemoryAccessStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let svc = AccessService::new(
        store.clone(),
        clock,
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_audit(audit.clone());
    Audited {
        svc,
        store,
        audit,
        repo: RepoId::new("game"),
    }
}

async fn audit_len(a: &Audited) -> usize {
    let query = AuditQuery {
        limit: 100,
        ..Default::default()
    };
    a.audit
        .list(&AuditScope::Repo(a.repo.clone()), &query)
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_role_never_creates_an_account() {
    let a = audited().await;
    a.svc.admin_for_tests(&user("root")).await.unwrap();
    a.svc.add_creator(&a.repo, &user("root")).await.unwrap();
    let root = a.svc.principal(&a.repo, &user("root")).await.unwrap();
    let before = audit_len(&a).await;

    let err = a
        .svc
        .set_user_role(&root, &a.repo, &user("ghost"), Role::Reader)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserNotFound(_)), "{err}");
    assert_eq!(err.code(), "user_not_found");
    assert!(!a.store.user_exists(&user("ghost")).await.unwrap());
    assert_eq!(
        a.store.role_of(&a.repo, &user("ghost")).await.unwrap(),
        None
    );
    assert_eq!(audit_len(&a).await, before);

    a.svc.account_for_tests(&user("bob")).await.unwrap();
    a.svc
        .set_user_role(&root, &a.repo, &user("bob"), Role::Writer)
        .await
        .unwrap();
    assert_eq!(
        a.store.role_of(&a.repo, &user("bob")).await.unwrap(),
        Some(Role::Writer)
    );
    assert_eq!(audit_len(&a).await, before + 1);
}

#[tokio::test]
async fn a_scoped_token_with_manage_users_adds_only_existing_accounts() {
    let w = world().await;
    let root = admin(&w).await;
    accounts(&w, &["wendy", "bob"]).await;
    w.svc
        .set_user_role(&root, &w.repo, &user("wendy"), Role::Admin)
        .await
        .unwrap();
    let (_, secret) = w
        .svc
        .create_token(
            &session("wendy"),
            "manage",
            perms(&[Read, ManageUsers]),
            vec![w.repo.clone()],
            None,
        )
        .await
        .unwrap();
    let scoped = w.svc.authenticate(&w.repo, &secret).await.unwrap();

    w.svc
        .set_user_role(&scoped, &w.repo, &user("bob"), Role::Reader)
        .await
        .unwrap();
    let err = w
        .svc
        .set_user_role(&scoped, &w.repo, &user("ghost"), Role::Reader)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserNotFound(_)), "{err}");
}

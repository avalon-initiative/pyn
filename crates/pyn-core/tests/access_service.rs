use std::collections::BTreeSet;
use std::sync::Arc;

use Permission::*;
use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::MemoryAccessStore;
use pyn_core::{AccessService, ManualClock, Permission, Principal, PynError, RepoId, Role, UserId};

struct World {
    svc: AccessService,
    clock: Arc<ManualClock>,
    repo: RepoId,
}

async fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let svc = AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone());
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

async fn admin(w: &World) -> Principal {
    w.svc.bootstrap_admin(&w.repo, &user("root")).await.unwrap();
    w.svc.principal(&w.repo, &user("root")).await.unwrap()
}

#[tokio::test]
async fn default_roles_grow_in_capability() {
    let w = world().await;
    let root = admin(&w).await;
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
    w.svc
        .set_user_role(&root, &w.repo, &user("wendy"), Role::Writer)
        .await
        .unwrap();
    let wendy = w.svc.principal(&w.repo, &user("wendy")).await.unwrap();

    let (record, full) = w
        .svc
        .create_token(&wendy, "ci", perms(&[Read, Checkin]), vec![], None)
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
    w.svc
        .set_user_role(&root, &w.repo, &user("wendy"), Role::Writer)
        .await
        .unwrap();
    let wendy = w.svc.principal(&w.repo, &user("wendy")).await.unwrap();
    let err = w
        .svc
        .create_token(&wendy, "sneaky", perms(&[Read, Restore]), vec![], None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Forbidden(Restore)), "{err}");
    let err = w
        .svc
        .create_token(&wendy, "empty", perms(&[]), vec![], None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

#[tokio::test]
async fn bad_revoked_expired_and_out_of_scope_tokens_are_rejected() {
    let w = world().await;
    let root = admin(&w).await;
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
            vec![],
            Some(Utc.with_ymd_and_hms(2026, 10, 8, 10, 0, 0).unwrap()),
        )
        .await
        .unwrap();
    let tampered = format!("{}0", &full[..full.len() - 1]);
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
        .create_token(&root, "t2", perms(&[Read]), vec![], None)
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
    for name in ["alice", "bob"] {
        w.svc
            .set_user_role(&root, &w.repo, &user(name), Role::Writer)
            .await
            .unwrap();
    }
    let alice = w.svc.principal(&w.repo, &user("alice")).await.unwrap();
    let bob = w.svc.principal(&w.repo, &user("bob")).await.unwrap();
    let (rec, _) = w
        .svc
        .create_token(&alice, "mine", perms(&[Read]), vec![], None)
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
        Err(PynError::Forbidden(ManageUsers))
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

//! A frozen owner (decision #429): administrators and `manage_freeze` credentials set it, members see it, and every
//! write under the owner is refused with `owner_frozen` while reads, sign-in and releasing locks continue.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::repo_policy::{
    CreationEffect, CreationRule, CreationScope, CreationSubject, MemberCreation,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditAction, AuditQuery, AuditScope, AuditStore,
    Credential, Identity, ManualClock, OrgRole, Permission, PynError, Registration,
    RegistrationMode, RepoPath, RepoRecord, RepoUpdate, Repositories, Role, Rules, ServiceScope,
    UserId, Visibility,
};

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    audit: Arc<MemoryAuditStore>,
    store: Arc<MemoryAccessStore>,
}

fn world_in(mode: RegistrationMode) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: mode,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Repositories::new(
        meta,
        Arc::new(MemoryObjectStore::new()),
        audit.clone(),
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n")
            .unwrap(),
    );
    World {
        repos,
        access,
        audit,
        store,
    }
}

fn world() -> World {
    world_in(RegistrationMode::Open)
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

async fn signup(w: &World, names: &[&str]) {
    for name in names {
        w.access
            .register(Registration::new(name, "correct horse battery"))
            .await
            .unwrap();
    }
}

async fn root(w: &World) -> Identity {
    w.access.admin_for_tests(&user("root")).await.unwrap();
    session("root")
}

async fn service(w: &World, name: &str, scopes: &[ServiceScope]) -> Identity {
    let root = root(w).await;
    let (_, secret) = w
        .access
        .create_service_credential(&root, name, scopes.iter().copied().collect::<BTreeSet<_>>())
        .await
        .unwrap();
    w.access.identify(&secret).await.unwrap()
}

fn frozen<T: std::fmt::Debug>(result: Result<T, PynError>, owner: &str) {
    let err = result.unwrap_err();
    assert!(
        matches!(&err, PynError::OwnerFrozen { owner: o, .. } if o == owner),
        "{err}"
    );
    assert_eq!(err.code(), "owner_frozen");
}

fn attempt<'a>(name: &'a str, code: &'a str) -> Registration<'a> {
    Registration::new(name, "correct horse battery").invite(code)
}

async fn server_log(w: &World) -> Vec<(String, String)> {
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
        .map(|e| (e.action.to_string(), e.detail))
        .collect()
}

async fn write_everywhere(w: &World, who: &str, repo: &RepoRecord, content: pyn_core::ObjectId) {
    let id = session(who);
    let open = w.repos.open(repo.owner.as_str(), &repo.name).await.unwrap();
    let principal = w.access.principal_in(&repo.id, &id).await.unwrap();
    let owner = repo.owner.as_str();
    let src = RepoPath::new("Source/a.cpp").unwrap();

    frozen(open.service.upload_blob(b"x".to_vec()).await, owner);
    frozen(
        open.service
            .checkin(&principal, &src, content, None, "m".into())
            .await,
        owner,
    );
    frozen(
        open.service
            .checkout(&RepoPath::new("Content/x.umap").unwrap(), &user(who), None)
            .await,
        owner,
    );
    frozen(
        open.service.create_branch(&principal, "feat", None).await,
        owner,
    );
}

async fn admin_writes(w: &World, who: &str, repo: &RepoRecord) {
    let id = session(who);
    let principal = w.access.principal_in(&repo.id, &id).await.unwrap();
    let owner = repo.owner.as_str();
    frozen(
        w.repos
            .update(
                &id,
                repo,
                RepoUpdate {
                    visibility: Some(Visibility::Public),
                    ..Default::default()
                },
            )
            .await,
        owner,
    );
    frozen(
        w.repos
            .update(
                &id,
                repo,
                RepoUpdate {
                    name: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .await,
        owner,
    );
    frozen(
        w.access
            .set_user_role(&principal, &repo.id, &user("dave"), Role::Reader)
            .await,
        owner,
    );
    frozen(
        w.access
            .add_user(
                &principal,
                &repo.id,
                "erin",
                "correct horse battery",
                Role::Reader,
            )
            .await,
        owner,
    );
    frozen(
        w.access
            .create_invite(&principal, &repo.id, Role::Reader, Duration::hours(2))
            .await,
        owner,
    );
    frozen(
        w.access
            .set_role_permissions(
                &principal,
                &repo.id,
                Role::Reader,
                BTreeSet::from([Permission::Read]),
            )
            .await,
        owner,
    );
    frozen(w.repos.delete(&id, repo).await, owner);
}

#[tokio::test]
async fn only_administrators_and_manage_freeze_credentials_set_it() {
    let w = world();
    signup(&w, &["alice"]).await;
    let root = root(&w).await;

    let err = w
        .access
        .freeze_owner(&session("alice"), &user("alice"), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
    let wrong = service(&w, "wrong", &[ServiceScope::ManageLimits]).await;
    let err = w
        .access
        .freeze_owner(&wrong, &user("alice"), "x")
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            PynError::ServiceScopeRequired(ServiceScope::ManageFreeze)
        ),
        "{err}"
    );
    assert!(matches!(
        w.access
            .unfreeze_owner(&wrong, &user("alice"))
            .await
            .unwrap_err(),
        PynError::ServiceScopeRequired(ServiceScope::ManageFreeze)
    ));

    let billing = service(&w, "billing", &[ServiceScope::ManageFreeze]).await;
    let state = w
        .access
        .freeze_owner(&billing, &user("alice"), "  plan lapsed ")
        .await
        .unwrap();
    assert!(state.frozen_at.is_some());
    assert_eq!(state.reason.as_deref(), Some("plan lapsed"));
    w.access
        .unfreeze_owner(&root, &user("alice"))
        .await
        .unwrap();

    for reason in ["", "   ", &"r".repeat(513)] {
        let err = w
            .access
            .freeze_owner(&root, &user("alice"), reason)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    }
    let err = w
        .access
        .freeze_owner(&root, &user("nobody"), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserNotFound(_)), "{err}");
}

#[tokio::test]
async fn the_state_is_visible_to_the_owner_and_members_only() {
    let w = world();
    signup(&w, &["alice", "bob", "mallory"]).await;
    let root = root(&w).await;
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    w.access
        .add_org_member(
            &session("alice"),
            &user("acme"),
            &user("bob"),
            OrgRole::Member,
        )
        .await
        .unwrap();
    w.access
        .freeze_owner(&root, &user("acme"), "abuse report")
        .await
        .unwrap();

    for viewer in ["alice", "bob"] {
        let state = w
            .access
            .owner_freeze(&session(viewer), &user("acme"))
            .await
            .unwrap();
        assert_eq!(state.reason.as_deref(), Some("abuse report"), "{viewer}");
        assert!(state.frozen_at.is_some());
        assert_eq!(state.kind, pyn_core::AccountKind::Org);
    }
    let err = w
        .access
        .owner_freeze(&session("mallory"), &user("acme"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotNamespaceOwner(_)), "{err}");
    let err = w
        .access
        .owner_freeze(&session("bob"), &user("alice"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotNamespaceOwner(_)), "{err}");

    let own = w
        .access
        .owner_freeze(&session("alice"), &user("alice"))
        .await
        .unwrap();
    assert!(own.frozen_at.is_none() && own.reason.is_none());
    let reader = service(&w, "reader", &[ServiceScope::ManageFreeze]).await;
    assert!(w.access.owner_freeze(&reader, &user("acme")).await.is_ok());
    let blind = service(&w, "blind", &[ServiceScope::ManageLimits]).await;
    assert!(w.access.owner_freeze(&blind, &user("acme")).await.is_err());
}

#[tokio::test]
async fn a_frozen_user_owner_refuses_every_write_under_it_and_stays_readable() {
    let w = world();
    signup(&w, &["alice", "bob"]).await;
    let root = root(&w).await;
    let alice = session("alice");
    let repo = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    let principal = w.access.principal_in(&repo.id, &alice).await.unwrap();
    w.access
        .set_user_role(&principal, &repo.id, &user("bob"), Role::Writer)
        .await
        .unwrap();
    let (invite, _) = w
        .access
        .create_invite(&principal, &repo.id, Role::Reader, Duration::hours(2))
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let lock_path = RepoPath::new("Content/held.umap").unwrap();
    open.service
        .checkout(&lock_path, &user("bob"), None)
        .await
        .unwrap();

    let content = open.service.upload_blob(b"before".to_vec()).await.unwrap();
    let state = w
        .access
        .freeze_owner(&root, &user("alice"), "plan lapsed")
        .await
        .unwrap();
    assert_eq!(state.reason.as_deref(), Some("plan lapsed"));

    write_everywhere(&w, "alice", &repo, content).await;
    admin_writes(&w, "alice", &repo).await;
    write_everywhere(&w, "bob", &repo, content).await;
    frozen(
        w.access
            .revoke_invite(&principal, &repo.id, &invite.id)
            .await,
        "alice",
    );
    frozen(
        w.repos
            .create(&alice, &user("alice"), "second", None, None)
            .await,
        "alice",
    );

    let bob = session("bob");
    assert_eq!(w.repos.list(&bob, None).await.unwrap().len(), 1);
    let again = w.repos.open("alice", "game").await.unwrap();
    assert!(again.service.files(None, 10).await.is_ok());
    assert!(
        again
            .service
            .history(&RepoPath::new("Source/a.cpp").unwrap())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(again.service.locks().await.unwrap().len(), 1);
    again
        .service
        .release(&lock_path, &user("bob"))
        .await
        .unwrap();
    assert!(again.service.locks().await.unwrap().is_empty());
    assert!(w.access.members(&principal, &repo.id).await.is_ok());

    w.access
        .login("alice", "correct horse battery", None)
        .await
        .unwrap();
    w.access
        .login("bob", "correct horse battery", None)
        .await
        .unwrap();

    let acme_repo = {
        w.access.create_org(&alice, "acme").await.unwrap();
        w.repos
            .create(&alice, &user("acme"), "tool", None, None)
            .await
            .unwrap()
    };
    let acme = w.repos.open("acme", &acme_repo.name).await.unwrap();
    assert!(acme.service.upload_blob(b"ok".to_vec()).await.is_ok());

    w.access
        .unfreeze_owner(&root, &user("alice"))
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    assert!(open.service.upload_blob(b"ok".to_vec()).await.is_ok());
    w.repos
        .create(&alice, &user("alice"), "second", None, None)
        .await
        .unwrap();
    w.access
        .revoke_invite(&principal, &repo.id, &invite.id)
        .await
        .unwrap();
    open.service
        .checkout(&lock_path, &user("bob"), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_frozen_organization_refuses_every_write_under_it_and_stays_readable() {
    let w = world();
    signup(&w, &["alice", "bob", "carol"]).await;
    let root = root(&w).await;
    let alice = session("alice");
    w.access.create_org(&alice, "acme").await.unwrap();
    let acme = user("acme");
    w.access
        .add_org_member(&alice, &acme, &user("bob"), OrgRole::Member)
        .await
        .unwrap();
    w.access
        .create_team(&alice, &acme, "art", None, None)
        .await
        .unwrap();
    w.access
        .add_team_member(&alice, &acme, "art", &user("bob"))
        .await
        .unwrap();
    w.access
        .set_creation_rule(
            &alice,
            &acme,
            CreationRule {
                subject: CreationSubject::Role(OrgRole::Member),
                effect: CreationEffect::Allow,
                scope: CreationScope::Both,
            },
        )
        .await
        .unwrap();
    let repo = w
        .repos
        .create(&alice, &acme, "game", None, None)
        .await
        .unwrap();
    let principal = w.access.principal_in(&repo.id, &alice).await.unwrap();
    w.access
        .set_team_access(&principal, &repo.id, "art", Role::Writer)
        .await
        .unwrap();

    let content = w
        .repos
        .open("acme", "game")
        .await
        .unwrap()
        .service
        .upload_blob(b"before".to_vec())
        .await
        .unwrap();
    w.access
        .freeze_owner(&root, &acme, "plan lapsed")
        .await
        .unwrap();

    write_everywhere(&w, "alice", &repo, content).await;
    admin_writes(&w, "alice", &repo).await;
    frozen(
        w.access
            .add_org_member(&alice, &acme, &user("carol"), OrgRole::Member)
            .await,
        "acme",
    );
    frozen(
        w.access
            .set_org_member_role(&alice, &acme, &user("bob"), OrgRole::Owner)
            .await,
        "acme",
    );
    frozen(
        w.access
            .remove_org_member(&alice, &acme, &user("bob"))
            .await,
        "acme",
    );
    frozen(
        w.access
            .remove_org_member(&session("bob"), &acme, &user("bob"))
            .await,
        "acme",
    );
    frozen(
        w.access
            .create_team(&alice, &acme, "code", None, None)
            .await,
        "acme",
    );
    frozen(
        w.access
            .update_team(&alice, &acme, "art", Some("Art"), None)
            .await,
        "acme",
    );
    frozen(w.access.delete_team(&alice, &acme, "art").await, "acme");
    frozen(
        w.access
            .add_team_member(&alice, &acme, "art", &user("alice"))
            .await,
        "acme",
    );
    frozen(
        w.access
            .remove_team_member(&alice, &acme, "art", &user("bob"))
            .await,
        "acme",
    );
    frozen(
        w.access
            .set_team_access(&principal, &repo.id, "art", Role::Admin)
            .await,
        "acme",
    );
    frozen(
        w.access
            .remove_team_access(&principal, &repo.id, "art")
            .await,
        "acme",
    );
    frozen(
        w.access
            .set_member_creation(&alice, &acme, MemberCreation::Both)
            .await,
        "acme",
    );
    frozen(
        w.access
            .set_creation_rule(
                &alice,
                &acme,
                CreationRule {
                    subject: CreationSubject::Role(OrgRole::Member),
                    effect: CreationEffect::Deny,
                    scope: CreationScope::Both,
                },
            )
            .await,
        "acme",
    );
    frozen(
        w.access
            .remove_creation_rule(
                &alice,
                &acme,
                &CreationSubject::Role(OrgRole::Member),
                CreationEffect::Allow,
            )
            .await,
        "acme",
    );
    frozen(
        w.repos.create(&alice, &acme, "second", None, None).await,
        "acme",
    );
    frozen(
        w.repos
            .create(&session("bob"), &acme, "third", None, None)
            .await,
        "acme",
    );
    frozen(w.access.delete_org(&alice, &acme).await, "acme");

    assert_eq!(w.access.org_members(&alice, &acme).await.unwrap().len(), 2);
    assert_eq!(w.access.teams(&alice, &acme).await.unwrap().len(), 1);
    assert!(w.access.repo_policy(&alice, &acme).await.is_ok());
    assert!(
        w.access
            .org_audit(
                &alice,
                &acme,
                &AuditQuery {
                    limit: 50,
                    ..Default::default()
                }
            )
            .await
            .is_ok()
    );
    assert!(
        w.repos
            .open("acme", "game")
            .await
            .unwrap()
            .service
            .files(None, 10)
            .await
            .is_ok()
    );
    assert_eq!(
        w.repos
            .list(&session("bob"), Some(&acme))
            .await
            .unwrap()
            .len(),
        1
    );

    w.access.unfreeze_owner(&root, &acme).await.unwrap();
    w.access
        .add_org_member(&alice, &acme, &user("carol"), OrgRole::Member)
        .await
        .unwrap();
    w.access
        .create_team(&alice, &acme, "code", None, None)
        .await
        .unwrap();
    w.repos
        .create(&alice, &acme, "second", None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_administrator_can_still_delete_a_frozen_empty_organization() {
    let w = world();
    signup(&w, &["alice"]).await;
    let root = root(&w).await;
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    w.access
        .freeze_owner(&root, &user("acme"), "abuse")
        .await
        .unwrap();
    w.access
        .admin_delete_org(&root, &user("acme"))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_invitation_to_a_frozen_owners_repository_cannot_be_redeemed() {
    let w = world_in(RegistrationMode::InviteOnly);
    let root = root(&w).await;
    w.store
        .create_user(&user("alice"), Utc::now())
        .await
        .unwrap();
    w.store
        .set_password_hash(&user("alice"), "unused")
        .await
        .unwrap();
    let repo = w
        .repos
        .create(&session("alice"), &user("alice"), "game", None, None)
        .await
        .unwrap();
    let principal = w
        .access
        .principal_in(&repo.id, &session("alice"))
        .await
        .unwrap();
    let (_, code) = w
        .access
        .create_invite(&principal, &repo.id, Role::Reader, Duration::hours(2))
        .await
        .unwrap();

    w.access
        .freeze_owner(&root, &user("alice"), "abuse")
        .await
        .unwrap();
    frozen(w.access.register(attempt("newcomer", &code)).await, "alice");
    assert!(w.store.account(&user("newcomer")).await.unwrap().is_none());

    w.access
        .unfreeze_owner(&root, &user("alice"))
        .await
        .unwrap();
    w.access.register(attempt("newcomer", &code)).await.unwrap();
}

#[tokio::test]
async fn freezing_and_unfreezing_are_audited_and_unfreezing_an_open_owner_is_a_no_op() {
    let w = world();
    signup(&w, &["alice"]).await;
    let root = root(&w).await;
    let acme = user("acme");
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();

    w.access.unfreeze_owner(&root, &acme).await.unwrap();
    assert!(
        server_log(&w)
            .await
            .iter()
            .all(|(a, _)| a != "owner_unfrozen"),
        "nothing to lift"
    );

    w.access
        .freeze_owner(&root, &acme, "plan lapsed")
        .await
        .unwrap();
    w.access
        .freeze_owner(&root, &acme, "abuse report")
        .await
        .unwrap();
    assert_eq!(
        w.access
            .owner_freeze(&root, &acme)
            .await
            .unwrap()
            .reason
            .as_deref(),
        Some("abuse report")
    );
    w.access.unfreeze_owner(&root, &acme).await.unwrap();
    w.access
        .freeze_owner(&root, &user("alice"), "x")
        .await
        .unwrap();

    let log = server_log(&w).await;
    let rows: Vec<_> = log
        .iter()
        .filter(|(a, _)| a.starts_with("owner_"))
        .cloned()
        .collect();
    assert_eq!(
        rows,
        [
            (
                "owner_frozen".to_string(),
                "acme frozen: plan lapsed".to_string()
            ),
            (
                "owner_frozen".to_string(),
                "acme frozen: abuse report".to_string()
            ),
            ("owner_unfrozen".to_string(), "acme unfrozen".to_string()),
            ("owner_frozen".to_string(), "alice frozen: x".to_string()),
        ]
    );
    let org_rows: Vec<_> = w
        .access
        .org_audit(
            &session("alice"),
            &acme,
            &AuditQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .filter(|e| {
            matches!(
                e.action,
                AuditAction::OwnerFrozen | AuditAction::OwnerUnfrozen
            )
        })
        .collect();
    assert_eq!(org_rows.len(), 3);
    assert!(org_rows.iter().all(|e| e.actor == user("root")));
}

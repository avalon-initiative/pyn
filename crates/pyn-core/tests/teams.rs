use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditAction, AuditEvent, AuditQuery, AuditScope,
    AuditStore, Credential, Identity, ManualClock, OrgCreation, OrgRole, Permission, PynError,
    RegistrationMode, Repositories, Role, Rules, TokenRecord, UserId,
};

struct World {
    store: Arc<MemoryAccessStore>,
    repos: Repositories,
    access: Arc<AccessService>,
    audit: Arc<MemoryAuditStore>,
}

fn world_with(org_creation: OrgCreation) -> World {
    build(org_creation, RegistrationMode::Open)
}

fn build(org_creation: OrgCreation, registration: RegistrationMode) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    let access = Arc::new(
        AccessService::new(store.clone(), clock.clone())
            .with_config(AccessConfig {
                registration,
                require_email_verification: false,
                org_creation,
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
        Rules::empty(),
    );
    World {
        store,
        repos,
        access,
        audit,
    }
}

fn world() -> World {
    world_with(OrgCreation::Anyone)
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

fn token_for(name: &str, permissions: &[Permission]) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Token(TokenRecord {
            id: pyn_core::TokenId("aaaaaaaaaaaa".into()),
            user: user(name),
            name: "t".into(),
            secret_hash: String::new(),
            permissions: permissions.iter().copied().collect(),
            repos: Vec::new(),
            created_at: Utc::now(),
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        }),
    }
}

async fn events(w: &World, scope: AuditScope) -> Vec<AuditEvent> {
    w.audit
        .list(
            &scope,
            &AuditQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap()
}

async fn setup() -> World {
    let w = world();
    let alice = session("alice");
    w.access.create_org(&alice, "acme").await.unwrap();
    for name in ["bob", "carol"] {
        w.store.create_user(&user(name), Utc::now()).await.unwrap();
        w.access
            .add_org_member(&alice, &user("acme"), &user(name), OrgRole::Member)
            .await
            .unwrap();
    }
    w.repos
        .create(&alice, &user("acme"), "game", None, None)
        .await
        .unwrap();
    w
}

async fn game(w: &World) -> pyn_core::RepoId {
    let all = w
        .repos
        .list(&session("alice"), Some(&user("acme")))
        .await
        .unwrap();
    all[0].0.id.clone()
}

fn details(events: &[AuditEvent], action: AuditAction) -> Vec<String> {
    events
        .iter()
        .filter(|e| e.action == action)
        .map(|e| e.detail.clone())
        .collect()
}

#[tokio::test]
async fn team_grant_events_name_the_team_the_role_and_the_repository() {
    let w = setup().await;
    let (alice, acme) = (session("alice"), user("acme"));
    w.access
        .create_team(&alice, &acme, "art", None, None)
        .await
        .unwrap();
    let admin = w
        .access
        .principal(&game(&w).await, &user("alice"))
        .await
        .unwrap();
    w.access
        .set_team_access(&admin, &game(&w).await, "art", Role::Reader)
        .await
        .unwrap();
    w.access
        .set_team_access(&admin, &game(&w).await, "art", Role::Reader)
        .await
        .unwrap();
    w.access
        .set_team_access(&admin, &game(&w).await, "art", Role::Writer)
        .await
        .unwrap();
    w.access.delete_team(&alice, &acme, "art").await.unwrap();

    let org = events(&w, AuditScope::Org(acme)).await;
    assert_eq!(
        details(&org, AuditAction::TeamAccessSet),
        [
            "team art on acme/game: reader -> writer",
            "team art granted reader on acme/game"
        ]
    );
    assert_eq!(
        details(&org, AuditAction::TeamDeleted),
        ["team art deleted; access dropped in acme/game"]
    );
    let repo = events(&w, AuditScope::Repo(game(&w).await)).await;
    assert_eq!(
        details(&repo, AuditAction::TeamAccessRemoved),
        ["team art removed with the team (was writer)"]
    );
    assert_eq!(
        details(&repo, AuditAction::TeamAccessSet),
        ["team art: reader -> writer", "team art added as reader"],
        "an unchanged grant is not logged again"
    );
}

#[tokio::test]
async fn removing_an_organization_member_logs_the_teams_they_left() {
    let w = setup().await;
    let (alice, acme) = (session("alice"), user("acme"));
    for slug in ["art", "eng"] {
        w.access
            .create_team(&alice, &acme, slug, None, None)
            .await
            .unwrap();
        w.access
            .add_team_member(&alice, &acme, slug, &user("bob"))
            .await
            .unwrap();
    }
    w.access
        .remove_org_member(&alice, &acme, &user("bob"))
        .await
        .unwrap();

    let org = events(&w, AuditScope::Org(acme.clone())).await;
    assert_eq!(
        details(&org, AuditAction::OrgMemberRemoved),
        ["bob removed; removed from teams art, eng"]
    );
    let detail = w.access.team(&alice, &acme, "art").await.unwrap();
    assert!(detail.members.is_empty());
}

#[tokio::test]
async fn team_management_needs_an_owner_and_an_unscoped_token_with_manage_roles() {
    let w = setup().await;
    let acme = user("acme");
    let narrow = token_for("alice", &[Permission::Read]);
    assert!(matches!(
        w.access
            .create_team(&narrow, &acme, "art", None, None)
            .await,
        Err(PynError::Forbidden(Permission::ManageRoles))
    ));
    assert!(matches!(
        w.access
            .create_team(&session("bob"), &acme, "art", None, None)
            .await,
        Err(PynError::NotOrgOwner(_))
    ));
    let wide = token_for("alice", &[Permission::ManageRoles]);
    w.access
        .create_team(&wide, &acme, "art", None, None)
        .await
        .unwrap();
    assert!(matches!(
        w.access
            .add_team_member(&wide, &acme, "art", &user("nobody"))
            .await,
        Err(PynError::UserNotOrgMember { .. })
    ));
}

#[tokio::test]
async fn team_slugs_names_and_descriptions_are_validated() {
    let w = setup().await;
    let (alice, acme) = (session("alice"), user("acme"));
    for bad in ["", "Art", "-art", "a b", &"x".repeat(40)] {
        assert!(
            matches!(
                w.access.create_team(&alice, &acme, bad, None, None).await,
                Err(PynError::InvalidRequest(_))
            ),
            "{bad:?}"
        );
    }
    let long = "x".repeat(501);
    assert!(
        w.access
            .create_team(&alice, &acme, "art", None, Some(&long))
            .await
            .is_err()
    );
    let t = w
        .access
        .create_team(&alice, &acme, "a", Some(" Art "), None)
        .await
        .unwrap();
    assert_eq!(t.team.name, "Art");
}

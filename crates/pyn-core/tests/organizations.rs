use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore,
    Credential, Identity, ManualClock, OrgCreation, OrgRole, Permission, PynError, Registration,
    RegistrationMode, Repositories, Role, Rules, TokenRecord, UserId, Visibility,
};

const PASSWORD: &str = "correct horse battery";

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    audit: Arc<MemoryAuditStore>,
}

fn world_with(org_creation: OrgCreation) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_config(AccessConfig {
                registration: RegistrationMode::Open,
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

async fn sign_up(w: &World, name: &str) {
    w.access
        .register(Registration::new(name, PASSWORD))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_creator_becomes_the_first_owner_and_creation_is_logged() {
    let w = world();
    let org = w
        .access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    assert_eq!(org.user, user("acme"));
    assert_eq!(org.kind, pyn_core::AccountKind::Org);

    assert_eq!(
        w.access
            .org_role(&user("acme"), &user("alice"))
            .await
            .unwrap(),
        Some(OrgRole::Owner)
    );
    assert_eq!(
        w.access
            .org_role(&user("acme"), &user("bob"))
            .await
            .unwrap(),
        None
    );
    let mine = w.access.orgs_of(&user("alice")).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(
        (mine[0].0.user.clone(), mine[0].1),
        (user("acme"), OrgRole::Owner)
    );
    assert!(w.access.orgs_of(&user("bob")).await.unwrap().is_empty());

    let log = events(&w, AuditScope::Org(user("acme"))).await;
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].action, AuditAction::OrgCreated);
    assert_eq!(log[0].actor, user("alice"));
    assert!(events(&w, AuditScope::Server).await.is_empty());
}

#[tokio::test]
async fn names_share_one_namespace_and_reserved_names_are_refused() {
    let w = world();
    sign_up(&w, "alice").await;
    let alice = session("alice");

    let err = w.access.create_org(&alice, "alice").await.unwrap_err();
    assert!(matches!(err, PynError::UserExists(_)), "{err}");
    w.access.create_org(&alice, "acme").await.unwrap();
    let err = w.access.create_org(&alice, "acme").await.unwrap_err();
    assert!(matches!(err, PynError::UserExists(_)), "{err}");
    let err = w
        .access
        .register(Registration::new("acme", PASSWORD))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::UserExists(_)),
        "sign-up cannot take it: {err}"
    );

    for reserved in ["_", "orgs", "settings", "admin", "-"] {
        let err = w.access.create_org(&alice, reserved).await.unwrap_err();
        assert!(
            matches!(err, PynError::ReservedName(_)),
            "{reserved}: {err}"
        );
        let err = w
            .access
            .register(Registration::new(reserved, PASSWORD))
            .await
            .unwrap_err();
        assert!(
            matches!(err, PynError::ReservedName(_)),
            "{reserved}: {err}"
        );
        assert_eq!(err.code(), "reserved_name");
    }
    for bad in ["", "a", "Acme", "-acme", "ac me", &"x".repeat(40)] {
        let err = w.access.create_org(&alice, bad).await.unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad:?}: {err}");
    }
}

#[tokio::test]
async fn organizations_cannot_sign_in_or_act() {
    let w = world();
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();

    let err = w.access.login("acme", PASSWORD, None).await.unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    let err = w
        .access
        .start_session("acme", PASSWORD, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");

    w.access
        .set_password_for_operator(&user("acme"), PASSWORD)
        .await
        .unwrap();
    let err = w.access.login("acme", PASSWORD, None).await.unwrap_err();
    assert!(
        matches!(err, PynError::Unauthenticated(ref m) if m.contains("organizations")),
        "even with a password set: {err}"
    );
    assert!(w.access.reject_org(&user("acme")).await.is_err());
    w.access.reject_org(&user("alice")).await.unwrap();
}

#[tokio::test]
async fn the_server_can_limit_creation_to_administrators() {
    let w = world_with(OrgCreation::AdminsOnly);
    w.access.bootstrap_admin(&user("root")).await.unwrap();

    let err = w
        .access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
    assert!(!w.access.is_org(&user("acme")).await.unwrap());
    w.access.create_org(&session("root"), "acme").await.unwrap();
}

#[tokio::test]
async fn a_token_needs_manage_roles_and_no_repository_scope_to_create_an_organization() {
    let w = world();
    let err = w
        .access
        .create_org(&token_for("alice", &[Permission::Read]), "acme")
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::ManageRoles)),
        "{err}"
    );
    w.access
        .create_org(&token_for("alice", &[Permission::ManageRoles]), "acme")
        .await
        .unwrap();
}

#[tokio::test]
async fn only_owners_create_repositories_in_the_organization() {
    let w = world();
    let (alice, bob) = (session("alice"), session("bob"));
    w.access.create_org(&alice, "acme").await.unwrap();
    sign_up(&w, "bob").await;

    let rec = w
        .repos
        .create(&alice, &user("acme"), "game", None, None)
        .await
        .unwrap();
    assert_eq!(rec.address(), "acme/game");
    assert_eq!(rec.visibility, Visibility::Private);
    assert!(
        w.access
            .members(
                &w.access.principal(&rec.id, &user("alice")).await.unwrap(),
                &rec.id
            )
            .await
            .unwrap()
            .is_empty(),
        "owners need no grant"
    );
    let log = events(&w, AuditScope::Repo(rec.id.clone())).await;
    assert_eq!(
        log.iter().map(|e| e.action).collect::<Vec<_>>(),
        [AuditAction::RepoCreated]
    );

    let err = w
        .repos
        .create(&bob, &user("acme"), "other", None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::NotOrgOwner(ref o) if o == "acme"),
        "{err}"
    );
    assert_eq!(err.code(), "not_org_owner");
    let err = w
        .repos
        .create(&alice, &user("nobody"), "x", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotNamespaceOwner(_)), "{err}");
    let err = w
        .repos
        .create(&alice, &user("bob"), "x", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotNamespaceOwner(_)), "{err}");

    let err = w
        .repos
        .create(&alice, &user("acme"), "game", None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::RepoExists(_)),
        "names are unique per owner: {err}"
    );
    w.repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn organization_owners_resolve_to_admin_on_every_repository_it_owns() {
    let w = world();
    let (alice, bob, carol) = (session("alice"), session("bob"), session("carol"));
    w.access.create_org(&alice, "acme").await.unwrap();
    for name in ["bob", "carol"] {
        sign_up(&w, name).await;
    }
    let game = w
        .repos
        .create(&alice, &user("acme"), "game", None, None)
        .await
        .unwrap();
    let tools = w
        .repos
        .create(&alice, &user("acme"), "tools", None, None)
        .await
        .unwrap();
    let personal = w
        .repos
        .create(&bob, &user("bob"), "notes", None, None)
        .await
        .unwrap();

    assert_eq!(
        w.access.role_in(&game.id, &user("alice")).await.unwrap(),
        Some(Role::Admin)
    );
    assert_eq!(
        w.access.role_in(&game.id, &user("carol")).await.unwrap(),
        None
    );
    let who = w.access.principal_in(&tools.id, &alice).await.unwrap();
    assert_eq!(who.permissions.len(), Permission::ALL.len());
    assert!(
        w.access
            .principal_in(&tools.id, &carol)
            .await
            .unwrap()
            .permissions
            .is_empty()
    );
    assert_eq!(
        w.access.repos_of(&user("alice")).await.unwrap().len(),
        2,
        "both organization repositories and no personal one"
    );
    assert!(
        !w.access
            .repos_of(&user("alice"))
            .await
            .unwrap()
            .contains(&personal.id)
    );

    let listed = w.repos.list(&alice, None).await.unwrap();
    assert_eq!(
        listed
            .iter()
            .map(|(r, role)| (r.address(), *role))
            .collect::<Vec<_>>(),
        [
            ("acme/game".to_string(), Some(Role::Admin)),
            ("acme/tools".to_string(), Some(Role::Admin))
        ]
    );
    assert!(w.repos.list(&carol, None).await.unwrap().is_empty());

    let narrowed = token_for("alice", &[Permission::Read]);
    let who = w.access.principal_in(&game.id, &narrowed).await.unwrap();
    assert_eq!(
        who.permissions.into_iter().collect::<Vec<_>>(),
        [Permission::Read],
        "a token still narrows what ownership grants"
    );

    // A weaker direct grant never lowers an owner.
    let root = w.access.principal_in(&game.id, &alice).await.unwrap();
    w.access
        .set_user_role(&root, &game.id, &user("alice"), Role::Reader)
        .await
        .unwrap();
    assert_eq!(
        w.access.role_in(&game.id, &user("alice")).await.unwrap(),
        Some(Role::Admin)
    );
    w.access
        .set_user_role(&root, &game.id, &user("carol"), Role::Writer)
        .await
        .unwrap();
    assert_eq!(
        w.access.role_in(&game.id, &user("carol")).await.unwrap(),
        Some(Role::Writer),
        "a direct grant to someone else still counts"
    );
}

#[tokio::test]
async fn owners_rename_reconfigure_and_delete_organization_repositories_but_other_admins_do_not() {
    let w = world();
    let (alice, carol) = (session("alice"), session("carol"));
    w.access.create_org(&alice, "acme").await.unwrap();
    sign_up(&w, "carol").await;
    let game = w
        .repos
        .create(&alice, &user("acme"), "game", None, None)
        .await
        .unwrap();
    let root = w.access.principal_in(&game.id, &alice).await.unwrap();
    w.access
        .set_user_role(&root, &game.id, &user("carol"), Role::Admin)
        .await
        .unwrap();

    let renamed = w
        .repos
        .update(
            &alice,
            &game,
            pyn_core::RepoUpdate {
                name: Some("game2".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.address(), "acme/game2");
    let err = w
        .repos
        .update(&carol, &renamed, pyn_core::RepoUpdate::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");
    let err = w.repos.delete(&carol, &renamed).await.unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");

    w.repos.delete(&alice, &renamed).await.unwrap();
    assert!(w.repos.open("acme", "game2").await.is_err());
}

#[tokio::test]
async fn an_organization_is_deleted_only_when_it_owns_no_repositories() {
    let w = world();
    let (alice, bob) = (session("alice"), session("bob"));
    w.access.create_org(&alice, "acme").await.unwrap();
    sign_up(&w, "bob").await;
    let game = w
        .repos
        .create(&alice, &user("acme"), "game", None, None)
        .await
        .unwrap();

    let err = w.access.delete_org(&bob, &user("acme")).await.unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");
    let err = w
        .access
        .delete_org(&alice, &user("acme"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::OrgNotEmpty(_)), "{err}");
    assert_eq!(err.code(), "org_not_empty");
    assert!(w.access.is_org(&user("acme")).await.unwrap());

    w.repos.delete(&alice, &game).await.unwrap();
    for missing in ["nobody", "bob"] {
        let err = w
            .access
            .delete_org(&alice, &user(missing))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::OrgNotFound(_)), "{missing}: {err}");
    }
    w.access.delete_org(&alice, &user("acme")).await.unwrap();

    assert!(!w.access.is_org(&user("acme")).await.unwrap());
    assert!(w.access.orgs_of(&user("alice")).await.unwrap().is_empty());
    let log = events(&w, AuditScope::Org(user("acme"))).await;
    assert_eq!(
        log.iter().map(|e| e.action).collect::<Vec<_>>(),
        [AuditAction::OrgDeleted, AuditAction::OrgCreated],
        "the log stays after the organization is gone"
    );
    w.access.create_org(&bob, "acme").await.unwrap();
}

#[tokio::test]
async fn the_organization_log_is_for_its_owners() {
    let w = world();
    let (alice, bob) = (session("alice"), session("bob"));
    w.access.create_org(&alice, "acme").await.unwrap();
    let query = AuditQuery {
        limit: 10,
        ..Default::default()
    };

    let log = w
        .access
        .org_audit(&alice, &user("acme"), &query)
        .await
        .unwrap();
    assert_eq!(log.len(), 1);
    let err = w
        .access
        .org_audit(&bob, &user("acme"), &query)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");
    let err = w
        .access
        .org_audit(&alice, &user("nobody"), &query)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::OrgNotFound(_)), "{err}");
}

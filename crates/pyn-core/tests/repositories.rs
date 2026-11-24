use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditEvent, AuditQuery, AuditStore, Credential,
    Identity, ManualClock, Permission, PynError, Registration, RepoPath, RepoSettings, RepoUpdate,
    Repositories, Role, Rules, UserId, Visibility,
};

const PASSWORD: &str = "correct horse battery";

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    audit: Arc<MemoryAuditStore>,
}

fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: pyn_core::RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_audit(audit.clone()),
    );
    let repos = Repositories::new(
        Arc::new(MemoryMetadataStore::new()),
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

fn user(name: &str) -> UserId {
    UserId::new(name)
}

fn session(name: &str) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Session,
    }
}

async fn events(w: &World, repo: &pyn_core::RepoId) -> Vec<AuditEvent> {
    w.audit
        .list(
            &pyn_core::AuditScope::Repo(repo.clone()),
            &AuditQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn creating_a_repository_makes_the_creator_its_admin() {
    let w = world();
    let alice = session("alice");
    let rec = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    assert_eq!(rec.address(), "alice/game");
    assert_eq!(rec.visibility, Visibility::Private, "private unless asked");
    assert_eq!(rec.settings, RepoSettings::default());

    let who = w.access.principal_in(&rec.id, &alice).await.unwrap();
    assert_eq!(who.permissions.len(), Permission::ALL.len());
    let open = w.repos.open("alice", "game").await.unwrap();
    assert_eq!(open.record, rec);
    assert_eq!(open.service.repo(), &rec.id);
    let log = events(&w, &rec.id).await;
    let actions: Vec<_> = log.iter().map(|e| e.action).collect();
    assert_eq!(
        actions,
        [AuditAction::RepoCreated, AuditAction::MemberAdded]
    );
}

#[tokio::test]
async fn repository_names_are_url_forms_on_create_lookup_and_rename() {
    let w = world();
    let alice = session("alice");
    let rec = w
        .repos
        .create(&alice, &user("alice"), "Test Repo", None, None)
        .await
        .unwrap();
    assert_eq!(rec.address(), "alice/test-repo");

    for typed in ["test-repo", "TeSt-RePo", "test repo", " TEST  REPO "] {
        let open = w.repos.open("alice", typed).await.unwrap();
        assert_eq!(open.record, rec, "{typed:?}");
        let err = w
            .repos
            .create(&alice, &user("alice"), typed, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::RepoExists(_)), "{typed:?}: {err}");
    }

    let other = w
        .repos
        .create(&alice, &user("alice"), "Other", None, None)
        .await
        .unwrap();
    assert_eq!(other.name, "other");
    let clash = RepoUpdate {
        name: Some("Test   REPO".into()),
        ..Default::default()
    };
    let err = w.repos.update(&alice, &other, clash).await.unwrap_err();
    assert!(matches!(err, PynError::RepoExists(_)), "{err}");
    let renamed = RepoUpdate {
        name: Some("Next Name".into()),
        ..Default::default()
    };
    let moved = w.repos.update(&alice, &other, renamed).await.unwrap();
    assert_eq!(moved.name, "next-name");
}

#[tokio::test]
async fn only_valid_unique_names_in_your_own_namespace_are_accepted() {
    let w = world();
    let alice = session("alice");
    w.repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();

    let err = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::RepoExists(ref a) if a == "alice/game"),
        "{err}"
    );
    let err = w
        .repos
        .create(&alice, &user("alice"), "Bad!Name", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRepoName(_)), "{err}");
    let err = w
        .repos
        .create(&alice, &user("bob"), "game", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotNamespaceOwner(_)), "{err}");
    let err = w
        .repos
        .create(
            &alice,
            &user("alice"),
            "slow",
            None,
            Some(RepoSettings {
                lease_hours: 0,
                ..RepoSettings::default()
            }),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");

    w.repos
        .create(&session("bob"), &user("bob"), "game", None, None)
        .await
        .unwrap();
    let missing = w.repos.open("alice", "nope").await.err().unwrap();
    assert!(matches!(missing, PynError::RepoNotFound(_)), "{missing}");
}

#[tokio::test]
async fn a_limited_token_cannot_create_repositories() {
    let w = world();
    w.access
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let alice = session("alice");
    let first = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    let (_, ci) = w
        .access
        .create_token(
            &alice,
            pyn_core::fixtures::new_token_for(
                &alice,
                "ci",
                [Permission::Read, Permission::Checkin],
                vec![first.id.clone()],
                None,
            ),
        )
        .await
        .unwrap();
    let ci = w.access.identify(&ci).await.unwrap();
    let err = w
        .repos
        .create(&ci, &user("alice"), "other", None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::ManageRoles)),
        "{err}"
    );

    let (_, full) = w.access.login("alice", PASSWORD, None).await.unwrap();
    let full = w.access.identify(&full).await.unwrap();
    w.repos
        .create(&full, &user("alice"), "other", None, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn listing_shows_the_repositories_you_belong_to() {
    let w = world();
    let (alice, bob) = (session("alice"), session("bob"));
    let a1 = w
        .repos
        .create(&alice, &user("alice"), "one", None, None)
        .await
        .unwrap();
    w.repos
        .create(&alice, &user("alice"), "two", None, None)
        .await
        .unwrap();
    w.repos
        .create(&bob, &user("bob"), "three", None, None)
        .await
        .unwrap();
    let root = w.access.principal_in(&a1.id, &alice).await.unwrap();
    w.access
        .set_user_role(&root, &a1.id, &user("bob"), Role::Reader)
        .await
        .unwrap();

    let names = |list: Vec<(pyn_core::RepoRecord, Option<Role>)>| -> Vec<String> {
        list.into_iter()
            .map(|(r, role)| format!("{}:{}", r.address(), role.unwrap()))
            .collect()
    };
    assert_eq!(
        names(w.repos.list(&alice, None).await.unwrap()),
        ["alice/one:admin", "alice/two:admin"]
    );
    assert_eq!(
        names(w.repos.list(&bob, None).await.unwrap()),
        ["alice/one:reader", "bob/three:admin"]
    );
    assert_eq!(
        names(w.repos.list(&bob, Some(&user("bob"))).await.unwrap()),
        ["bob/three:admin"]
    );
    let dev = Identity {
        user: user("dev"),
        credential: Credential::Unrestricted,
    };
    assert_eq!(w.repos.list(&dev, None).await.unwrap().len(), 3);
}

#[tokio::test]
async fn only_the_owner_renames_or_deletes_and_data_goes_with_the_repository() {
    let w = world();
    let (alice, bob) = (session("alice"), session("bob"));
    let rec = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    let root = w.access.principal_in(&rec.id, &alice).await.unwrap();
    w.access.account_for_tests(&user("bob")).await.unwrap();
    w.access
        .set_user_role(&root, &rec.id, &user("bob"), Role::Admin)
        .await
        .unwrap();

    let rename = RepoUpdate {
        name: Some("engine".into()),
        visibility: Some(Visibility::Public),
        settings: Some(RepoSettings {
            lease_hours: 2,
            ..RepoSettings::default()
        }),
        ..Default::default()
    };
    let err = w
        .repos
        .update(&bob, &rec, rename.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::NotNamespaceOwner(_)),
        "even a repository admin: {err}"
    );
    let err = w.repos.delete(&bob, &rec).await.unwrap_err();
    assert!(matches!(err, PynError::NotNamespaceOwner(_)), "{err}");

    let renamed = w.repos.update(&alice, &rec, rename).await.unwrap();
    assert_eq!(renamed.address(), "alice/engine");
    assert_eq!(renamed.id, rec.id);
    assert!(w.repos.open("alice", "game").await.is_err());
    let open = w.repos.open("alice", "engine").await.unwrap();
    let map = RepoPath::new("Content/a.umap").unwrap();
    open.service
        .checkout(&map, &user("alice"), None)
        .await
        .unwrap();

    w.repos.delete(&alice, &renamed).await.unwrap();
    assert!(w.repos.open("alice", "engine").await.is_err());
    assert!(w.access.repos_of(&user("alice")).await.unwrap().is_empty());
    assert!(w.access.repos_of(&user("bob")).await.unwrap().is_empty());
    let log = events(&w, &rec.id).await;
    assert_eq!(
        log[0].action,
        AuditAction::RepoDeleted,
        "the log outlives the repository"
    );

    let again = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    assert_ne!(
        again.id, rec.id,
        "an address is never reused for the old data"
    );
}

#[tokio::test]
async fn the_lease_setting_applies_to_that_repository_only() {
    let w = world();
    let alice = session("alice");
    let short = w
        .repos
        .create(
            &alice,
            &user("alice"),
            "short",
            None,
            Some(RepoSettings {
                lease_hours: 1,
                ..RepoSettings::default()
            }),
        )
        .await
        .unwrap();
    w.repos
        .create(&alice, &user("alice"), "long", None, None)
        .await
        .unwrap();
    let map = RepoPath::new("Content/a.umap").unwrap();
    let lease = |lock: pyn_core::Lock| lock.expires_at - lock.acquired_at;
    for (name, hours) in [("short", 1), ("long", 8)] {
        let open = w.repos.open("alice", name).await.unwrap();
        let lock = open
            .service
            .checkout(&map, &user("alice"), None)
            .await
            .unwrap();
        assert_eq!(lease(lock), Duration::hours(hours), "{name}");
    }

    let changed = RepoUpdate {
        settings: Some(RepoSettings {
            lease_hours: 3,
            ..RepoSettings::default()
        }),
        ..Default::default()
    };
    w.repos.update(&alice, &short, changed).await.unwrap();
    let open = w.repos.open("alice", "short").await.unwrap();
    open.service.release(&map, &user("alice")).await.unwrap();
    let lock = open
        .service
        .checkout(&map, &user("alice"), None)
        .await
        .unwrap();
    assert_eq!(
        lease(lock),
        Duration::hours(3),
        "a changed setting applies at once"
    );
}

#[tokio::test]
async fn access_changes_and_tokens_are_audited_in_their_own_repository() {
    let w = world();
    let alice = session("alice");
    let one = w
        .repos
        .create(&alice, &user("alice"), "one", None, None)
        .await
        .unwrap();
    let two = w
        .repos
        .create(&alice, &user("alice"), "two", None, None)
        .await
        .unwrap();
    let three = w
        .repos
        .create(&alice, &user("alice"), "three", None, None)
        .await
        .unwrap();

    let (rec, _) = w
        .access
        .create_token(
            &alice,
            pyn_core::fixtures::new_token_for(
                &alice,
                "ci",
                [Permission::Read],
                vec![one.id.clone(), two.id.clone()],
                None,
            ),
        )
        .await
        .unwrap();
    w.access.revoke_token(&alice, &rec.id).await.unwrap();

    let count = |log: &[AuditEvent], a| log.iter().filter(|e| e.action == a).count();
    for repo in [&one, &two] {
        let log = events(&w, &repo.id).await;
        assert_eq!(count(&log, AuditAction::TokenCreated), 1, "{}", repo.name);
        assert_eq!(count(&log, AuditAction::TokenRevoked), 1, "{}", repo.name);
    }
    let log = events(&w, &three.id).await;
    assert_eq!(count(&log, AuditAction::TokenCreated), 0);
}

#[tokio::test]
async fn an_invitation_belongs_to_its_repository() {
    let w = world();
    let alice = session("alice");
    let one = w
        .repos
        .create(&alice, &user("alice"), "one", None, None)
        .await
        .unwrap();
    let two = w
        .repos
        .create(&alice, &user("alice"), "two", None, None)
        .await
        .unwrap();
    let admin_of = |repo: &pyn_core::RepoRecord| {
        let access = w.access.clone();
        let (alice, id) = (alice.clone(), repo.id.clone());
        async move { access.principal_in(&id, &alice).await.unwrap() }
    };

    let (record, _) = w
        .access
        .create_invite(&alice, &one.id, Role::Writer, Duration::days(1))
        .await
        .unwrap();
    let err = w
        .access
        .revoke_invite(&admin_of(&two).await, &two.id, &record.id)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::InvalidInvite(_)),
        "another repository's admin cannot revoke it: {err}"
    );
    w.access
        .revoke_invite(&admin_of(&one).await, &one.id, &record.id)
        .await
        .unwrap();

    let logged = events(&w, &one.id).await;
    let actions: Vec<_> = logged
        .iter()
        .map(|e| e.action)
        .filter(|a| matches!(a, AuditAction::InviteCreated | AuditAction::InviteRevoked))
        .collect();
    assert_eq!(
        actions,
        [AuditAction::InviteRevoked, AuditAction::InviteCreated],
        "newest first, and only in the repository concerned"
    );
    assert!(events(&w, &two.id).await.iter().all(|e| !matches!(
        e.action,
        AuditAction::InviteCreated | AuditAction::InviteRevoked
    )));
}

#[tokio::test]
async fn public_repositories_grant_read_to_anyone_and_private_ones_only_to_members() {
    let w = world();
    let (alice, bob) = (session("alice"), session("bob"));
    let private = w
        .repos
        .create(&alice, &user("alice"), "closed", None, None)
        .await
        .unwrap();
    let public = w
        .repos
        .create(
            &alice,
            &user("alice"),
            "open",
            Some(Visibility::Public),
            None,
        )
        .await
        .unwrap();

    let member = w.repos.principal_for(&private, Some(&alice)).await.unwrap();
    assert!(member.unwrap().has(Permission::Restore));
    for who in [Some(&bob), None] {
        assert_eq!(w.repos.principal_for(&private, who).await.unwrap(), None);
        let p = w.repos.principal_for(&public, who).await.unwrap().unwrap();
        assert_eq!(
            p.permissions,
            [Permission::Read].into(),
            "read and nothing more"
        );
    }
    let member = w.repos.principal_for(&public, Some(&alice)).await.unwrap();
    assert!(
        member.unwrap().has(Permission::Restore),
        "a role keeps its permissions"
    );
}

#[tokio::test]
async fn a_stricter_role_still_reads_a_public_repository() {
    let w = world();
    let alice = session("alice");
    let public = w
        .repos
        .create(
            &alice,
            &user("alice"),
            "open",
            Some(Visibility::Public),
            None,
        )
        .await
        .unwrap();
    let root = w.access.principal_in(&public.id, &alice).await.unwrap();
    w.access.account_for_tests(&user("bob")).await.unwrap();
    w.access
        .set_user_role(&root, &public.id, &user("bob"), Role::Reader)
        .await
        .unwrap();
    w.access
        .set_role_permissions(&root, &public.id, Role::Reader, [Permission::Lock].into())
        .await
        .unwrap();
    let p = w
        .repos
        .principal_for(&public, Some(&session("bob")))
        .await
        .unwrap()
        .unwrap();
    assert!(p.has(Permission::Read) && p.has(Permission::Lock));
}

#[tokio::test]
async fn my_locks_follow_visibility_for_someone_without_a_role() {
    let w = world();
    let (alice, bob) = (session("alice"), session("bob"));
    let rec = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let map = RepoPath::new("Content/a.umap").unwrap();
    open.service
        .checkout(&map, &user("bob"), None)
        .await
        .unwrap();
    assert!(w.repos.locks_of(&bob).await.unwrap().is_empty());

    let update = |visibility| RepoUpdate {
        name: None,
        visibility: Some(visibility),
        settings: None,
        storage_cap: None,
        ..Default::default()
    };
    w.repos
        .update(&alice, &rec, update(Visibility::Public))
        .await
        .unwrap();
    assert_eq!(w.repos.locks_of(&bob).await.unwrap().len(), 1);
}

#[tokio::test]
async fn changing_visibility_is_recorded_with_before_and_after() {
    let w = world();
    let alice = session("alice");
    let rec = w
        .repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    let made_public = w
        .repos
        .update(
            &alice,
            &rec,
            RepoUpdate {
                name: None,
                visibility: Some(Visibility::Public),
                settings: None,
                storage_cap: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let log = events(&w, &rec.id).await;
    assert_eq!(log[0].action, AuditAction::RepoUpdated);
    assert_eq!(log[0].actor, user("alice"));
    assert!(
        log[0].detail.contains("visibility private -> public"),
        "{}",
        log[0].detail
    );

    w.repos
        .update(
            &alice,
            &made_public,
            RepoUpdate {
                name: Some("renamed".into()),
                visibility: None,
                settings: None,
                storage_cap: None,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let log = events(&w, &rec.id).await;
    assert!(!log[0].detail.contains("visibility"), "{}", log[0].detail);
}

#[tokio::test]
async fn the_unreferenced_upload_cap_defaults_to_32_gib_and_a_configured_one_is_enforced() {
    assert_eq!(world().repos.unreferenced_upload_cap(), 32 << 30);

    let mut w = world();
    w.repos = w.repos.with_unreferenced_upload_cap(10);
    let alice = session("alice");
    w.repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    open.service.upload_blob(b"abcdef".to_vec()).await.unwrap();
    let err = open
        .service
        .upload_blob(b"ghijk".to_vec())
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::StorageLimitReached { limit: 10, .. }),
        "{err}"
    );
}

#[tokio::test]
async fn a_zero_unreferenced_upload_cap_refuses_every_upload() {
    let mut w = world();
    w.repos = w.repos.with_unreferenced_upload_cap(0);
    let alice = session("alice");
    w.repos
        .create(&alice, &user("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let err = open.service.upload_blob(b"a".to_vec()).await.unwrap_err();
    assert!(
        matches!(err, PynError::StorageLimitReached { limit: 0, .. }),
        "{err}"
    );
}

/// alice owns public `game` and `lobby` with a lock in each; her tokens come back through `identify`.
struct Scoped {
    w: World,
    game: pyn_core::RepoId,
    lobby: pyn_core::RepoId,
    full: Identity,
    scoped: Identity,
}

async fn scoped_world() -> Scoped {
    let w = world();
    w.access
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    let alice = session("alice");
    let mut ids = Vec::new();
    for name in ["game", "lobby"] {
        let rec = w
            .repos
            .create(&alice, &user("alice"), name, Some(Visibility::Public), None)
            .await
            .unwrap();
        let open = w.repos.open("alice", name).await.unwrap();
        let path = RepoPath::new("Content/a.umap").unwrap();
        open.service
            .checkout(&path, &user("alice"), None)
            .await
            .unwrap();
        ids.push(rec.id);
    }
    let (_, raw) = w.access.login("alice", PASSWORD, None).await.unwrap();
    let full = w.access.identify(&raw).await.unwrap();
    let (_, raw) = w
        .access
        .create_token(
            &alice,
            pyn_core::fixtures::new_token_for(
                &alice,
                "ci",
                [Permission::Read],
                vec![ids[0].clone()],
                None,
            ),
        )
        .await
        .unwrap();
    let scoped = w.access.identify(&raw).await.unwrap();
    let (game, lobby) = (ids[0].clone(), ids[1].clone());
    Scoped {
        w,
        game,
        lobby,
        full,
        scoped,
    }
}

#[tokio::test]
async fn a_role_is_reported_only_for_repositories_the_credential_covers() {
    let s = scoped_world().await;
    let access = &s.w.access;
    assert!(matches!(s.scoped.credential, Credential::Token(_)));
    let admin = Some(Role::Admin);
    for who in [&session("alice"), &s.full] {
        assert_eq!(access.role_for(&s.game, Some(who)).await.unwrap(), admin);
        assert_eq!(access.role_for(&s.lobby, Some(who)).await.unwrap(), admin);
    }
    assert_eq!(
        access.role_for(&s.game, Some(&s.scoped)).await.unwrap(),
        admin
    );
    assert_eq!(
        access.role_for(&s.lobby, Some(&s.scoped)).await.unwrap(),
        None
    );
    assert_eq!(access.role_for(&s.game, None).await.unwrap(), None);
}

#[tokio::test]
async fn public_listings_give_a_scoped_token_no_role_outside_its_grant() {
    let s = scoped_world().await;
    let roles = |found: Vec<(pyn_core::RepoRecord, Option<Role>)>| {
        found
            .into_iter()
            .map(|(r, role)| (r.name, role))
            .collect::<Vec<_>>()
    };
    let admin = Some(Role::Admin);
    let page = |who: &Identity| {
        let repos = &s.w.repos;
        let who = who.clone();
        async move { roles(repos.list_public(Some(&who), None, 10).await.unwrap().0) }
    };
    let visible = |who: &Identity| {
        let repos = &s.w.repos;
        let who = who.clone();
        async move { roles(repos.visible_to(&user("alice"), Some(&who)).await.unwrap()) }
    };
    let want_full = vec![("game".to_string(), admin), ("lobby".to_string(), admin)];
    let want_scoped = vec![("game".to_string(), admin), ("lobby".to_string(), None)];
    assert_eq!(page(&s.full).await, want_full);
    assert_eq!(page(&s.scoped).await, want_scoped);
    assert_eq!(visible(&s.full).await, want_full);
    assert_eq!(visible(&s.scoped).await, want_scoped);
}

#[tokio::test]
async fn my_locks_of_a_scoped_token_are_only_in_its_repositories() {
    let s = scoped_world().await;
    let names = |locks: Vec<(pyn_core::RepoRecord, pyn_core::Lock)>| {
        locks.into_iter().map(|(r, _)| r.name).collect::<Vec<_>>()
    };
    assert_eq!(
        names(s.w.repos.locks_of(&s.full).await.unwrap()),
        ["game", "lobby"]
    );
    assert_eq!(
        names(s.w.repos.locks_of(&s.scoped).await.unwrap()),
        ["game"]
    );
}

#[tokio::test]
async fn a_credential_covers_the_repositories_it_names() {
    let s = scoped_world().await;
    let alice = user("alice");
    assert!(s.scoped.covers_repo(&s.game, &alice) && !s.scoped.covers_repo(&s.lobby, &alice));
    assert!(s.full.covers_repo(&s.lobby, &alice) && session("alice").covers_repo(&s.lobby, &alice));
}

#[tokio::test]
async fn a_token_reads_public_repositories_it_does_not_cover_but_never_writes_to_them() {
    use pyn_core::{NewToken, RepoAccess, TokenExpiry};
    let s = scoped_world().await;
    let alice = session("alice");
    let mint = |access: RepoAccess, repos: Vec<pyn_core::RepoId>| {
        let (w, alice) = (&s.w, &alice);
        async move {
            let grants = match access {
                RepoAccess::Public => Default::default(),
                _ => pyn_core::fixtures::grants_for(&[
                    Permission::Read,
                    Permission::Lock,
                    Permission::Checkin,
                ]),
            };
            let req = NewToken {
                name: "t".into(),
                description: None,
                owner: user("alice"),
                expiry: TokenExpiry::Never,
                repo_access: access,
                repos,
                grants,
            };
            let (_, raw) = w.access.create_token(alice, req).await.unwrap();
            w.access.identify(&raw).await.unwrap()
        }
    };
    let selected = mint(RepoAccess::Selected, vec![s.game.clone()]).await;
    let public = mint(RepoAccess::Public, Vec::new()).await;
    let held = |who: &Identity, name: &'static str| {
        let (w, who) = (&s.w, who.clone());
        async move {
            let open = w.repos.open("alice", name).await.unwrap();
            let p = w
                .repos
                .principal_for(&open.record, Some(&who))
                .await
                .unwrap()
                .unwrap();
            p.permissions
        }
    };
    let writes = [
        Permission::Read,
        Permission::Lock,
        Permission::Checkin,
        Permission::Restore,
    ]
    .into();
    assert_eq!(held(&selected, "game").await, writes);
    assert_eq!(held(&selected, "lobby").await, [Permission::Read].into());
    assert_eq!(held(&public, "game").await, [Permission::Read].into());
    assert_eq!(held(&public, "lobby").await, [Permission::Read].into());
}

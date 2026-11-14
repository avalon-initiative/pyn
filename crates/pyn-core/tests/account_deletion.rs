//! Account deletion (decisions #975, #976): self-service with a recovery window, administrator deletion, the
//! server's timer, the purge rules and the name reservation.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AccountStatus, AuditAction, AuditQuery, AuditScope,
    AuditStore, Credential, Identity, ManualClock, PynError, Registration, RegistrationMode,
    Repositories, Rules, UserId,
};

const PASSWORD: &str = "correct horse battery";

struct World {
    access: Arc<AccessService>,
    repos: Repositories,
    store: Arc<MemoryAccessStore>,
    audit: Arc<MemoryAuditStore>,
    clock: Arc<ManualClock>,
}

fn world_with(config: impl FnOnce(&mut AccessConfig)) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    World::on(store, meta, audit, clock, config)
}

impl World {
    fn on(
        store: Arc<MemoryAccessStore>,
        meta: Arc<MemoryMetadataStore>,
        audit: Arc<MemoryAuditStore>,
        clock: Arc<ManualClock>,
        config: impl FnOnce(&mut AccessConfig),
    ) -> World {
        let mut cfg = AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        };
        config(&mut cfg);
        let access = Arc::new(
            AccessService::new(
                store.clone(),
                clock.clone(),
                Arc::new(pyn_core::MemoryLimitEvents::new()),
            )
            .with_config(cfg)
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
            store,
            audit,
            clock,
        }
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

async fn sign_up(w: &World, name: &str) {
    w.access
        .register(Registration::new(name, PASSWORD))
        .await
        .unwrap();
}

async fn admin(w: &World, name: &str) -> Identity {
    let token = w.access.admin_for_tests(&user(name)).await.unwrap();
    w.access.identify(&token).await.unwrap()
}

async fn disable(w: &World, who: &Identity, name: &str) {
    w.access
        .disable_account(who, &user(name), Some("quiet"))
        .await
        .unwrap();
}

async fn server_audit(w: &World) -> Vec<(String, String, String)> {
    w.audit
        .list(
            &AuditScope::Server,
            &AuditQuery {
                limit: 200,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .rev()
        .map(|e| (e.actor.to_string(), e.action.to_string(), e.detail))
        .collect()
}

fn code<T: std::fmt::Debug>(result: Result<T, PynError>) -> &'static str {
    result.unwrap_err().code()
}

#[tokio::test]
async fn self_service_deletion_needs_the_password_and_schedules_the_window() {
    let w = world();
    sign_up(&w, "alice").await;

    let wrong = w
        .access
        .schedule_own_deletion(&session("alice"), "nope")
        .await;
    assert_eq!(code(wrong), "unauthenticated");
    assert!(
        w.access
            .deletion_status(&session("alice"))
            .await
            .unwrap()
            .is_none()
    );

    let deletion = w
        .access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap();
    assert_eq!(deletion.purge_at, w.clock_now() + Duration::days(30));
    assert!(deletion.by_owner);

    let account = w.store.account(&user("alice")).await.unwrap().unwrap();
    assert_eq!(account.status(), AccountStatus::Disabled);
    assert_eq!(
        w.access
            .deletion_status(&session("alice"))
            .await
            .unwrap()
            .unwrap()
            .purge_at,
        deletion.purge_at
    );
    assert_eq!(
        code(
            w.access
                .schedule_own_deletion(&session("alice"), PASSWORD)
                .await
        ),
        "invalid_request",
        "already scheduled"
    );
    let log = server_audit(&w).await;
    assert_eq!(log.last().unwrap().0, "alice");
    assert_eq!(log.last().unwrap().1, "account_deletion_scheduled");
}

impl World {
    fn clock_now(&self) -> chrono::DateTime<Utc> {
        use pyn_core::Clock;
        self.clock.now()
    }
}

#[tokio::test]
async fn the_window_length_is_a_setting() {
    let w = world_with(|c| c.deletion_window_days = 7);
    sign_up(&w, "alice").await;
    let d = w
        .access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap();
    assert_eq!(d.purge_at - d.scheduled_at, Duration::days(7));
}

#[tokio::test]
async fn a_pending_account_signs_in_to_restore_and_nothing_else() {
    let w = world();
    sign_up(&w, "alice").await;
    let (_, token) = w.access.login("alice", PASSWORD, None).await.unwrap();
    let (_, cookie) = w
        .access
        .start_session("alice", PASSWORD, None)
        .await
        .unwrap();
    w.access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap();

    let (login, secret) = w.access.login("alice", PASSWORD, None).await.unwrap();
    assert_eq!(
        login.user,
        user("alice"),
        "sign-in still works in the window"
    );
    w.access
        .start_session("alice", PASSWORD, None)
        .await
        .unwrap();

    for result in [
        w.access.identify(&token).await.map(|_| ()),
        w.access.identify(&secret).await.map(|_| ()),
        w.access.authenticate_session(&cookie).await.map(|_| ()),
    ] {
        let err = result.unwrap_err();
        assert_eq!(err.code(), "account_pending_deletion");
        assert!(err.to_string().contains("restore"), "{err}");
    }
    let who = w.access.identify_allowing_pending(&secret).await.unwrap();
    assert_eq!(who.user, user("alice"));
    w.access
        .authenticate_session_allowing_pending(&cookie)
        .await
        .unwrap();

    w.access.restore_account(&who).await.unwrap();
    assert!(w.access.identify(&secret).await.is_ok());
    assert!(w.access.authenticate_session(&cookie).await.is_ok());
    assert!(w.access.deletion_status(&who).await.unwrap().is_none());
    assert_eq!(
        w.store
            .account(&user("alice"))
            .await
            .unwrap()
            .unwrap()
            .status(),
        AccountStatus::Active
    );
    let log = server_audit(&w).await;
    assert_eq!(log.last().unwrap().1, "account_deletion_restored");
    assert_eq!(
        code(w.access.restore_account(&who).await),
        "invalid_request",
        "nothing left to restore"
    );
}

#[tokio::test]
async fn an_account_an_administrator_disabled_still_cannot_sign_in() {
    let w = world();
    sign_up(&w, "alice").await;
    let root = admin(&w, "root").await;
    disable(&w, &root, "alice").await;
    assert_eq!(
        code(w.access.login("alice", PASSWORD, None).await),
        "account_disabled"
    );
    w.access
        .admin_delete_account(&root, &user("alice"), "spam", false)
        .await
        .unwrap();
    assert_eq!(
        code(w.access.login("alice", PASSWORD, None).await),
        "account_disabled",
        "only a self-service deletion can be restored by sign-in"
    );
}

#[tokio::test]
async fn restoring_after_the_window_is_refused() {
    let w = world();
    sign_up(&w, "alice").await;
    w.access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap();
    w.clock.advance(Duration::days(31));
    assert_eq!(
        code(w.access.restore_account(&session("alice")).await),
        "invalid_request"
    );
}

#[tokio::test]
async fn an_account_that_owns_repositories_or_organizations_is_not_deleted() {
    let w = world();
    for name in ["alice", "bob"] {
        sign_up(&w, name).await;
    }
    w.repos
        .create(&session("alice"), &user("alice"), "game", None, None)
        .await
        .unwrap();
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    w.access.create_org(&session("alice"), "duo").await.unwrap();
    w.access
        .add_org_member(
            &session("alice"),
            &user("duo"),
            &user("bob"),
            pyn_core::OrgRole::Owner,
        )
        .await
        .unwrap();
    w.access
        .accept_org_invitation(&session("bob"), &user("duo"))
        .await
        .unwrap();

    let err = w
        .access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap_err();
    let PynError::AccountOwnsResources { repos, orgs, .. } = &err else {
        panic!("{err}");
    };
    assert_eq!(repos, &["alice/game"]);
    assert_eq!(
        orgs,
        &["acme"],
        "an organization with another owner does not block"
    );
    assert_eq!(err.code(), "account_owns_resources");
    assert!(err.to_string().contains("alice/game") && err.to_string().contains("acme"));
    assert_eq!(
        w.store
            .account(&user("alice"))
            .await
            .unwrap()
            .unwrap()
            .status(),
        AccountStatus::Active,
        "a refusal leaves the account as it was"
    );

    let game = w.repos.open("alice", "game").await.unwrap().record;
    w.repos.delete(&session("alice"), &game).await.unwrap();
    w.access
        .delete_org(&session("alice"), &user("acme"))
        .await
        .unwrap();
    w.access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap();
}

#[tokio::test]
async fn the_last_active_administrator_and_a_frozen_owner_are_not_deleted() {
    let w = world();
    let root = admin(&w, "root").await;
    sign_up(&w, "alice").await;
    w.access
        .set_password_for_operator(&user("root"), PASSWORD)
        .await
        .unwrap();
    assert_eq!(
        code(
            w.access
                .schedule_own_deletion(&session("root"), PASSWORD)
                .await
        ),
        "last_server_admin"
    );

    w.access
        .freeze_owner(&root, &user("alice"), "plan lapsed")
        .await
        .unwrap();
    assert_eq!(
        code(
            w.access
                .schedule_own_deletion(&session("alice"), PASSWORD)
                .await
        ),
        "account_frozen"
    );
    disable(&w, &root, "alice").await;
    assert_eq!(
        code(
            w.access
                .admin_delete_account(&root, &user("alice"), "gone", true)
                .await
        ),
        "account_frozen"
    );
    w.access
        .unfreeze_owner(&root, &user("alice"))
        .await
        .unwrap();
    w.access
        .admin_delete_account(&root, &user("alice"), "gone", true)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_administrator_deletes_only_a_disabled_account_with_a_reason() {
    let w = world();
    let root = admin(&w, "root").await;
    for name in ["alice", "bob"] {
        sign_up(&w, name).await;
    }
    assert_eq!(
        code(
            w.access
                .admin_delete_account(&root, &user("alice"), "x", true)
                .await
        ),
        "account_not_disabled"
    );
    disable(&w, &root, "alice").await;
    assert_eq!(
        code(
            w.access
                .admin_delete_account(&root, &user("alice"), "  ", true)
                .await
        ),
        "invalid_request",
        "a reason is required"
    );
    assert_eq!(
        code(
            w.access
                .admin_delete_account(&root, &user("root"), "x", true)
                .await
        ),
        "invalid_request",
        "not one's own account"
    );
    assert_eq!(
        code(
            w.access
                .admin_delete_account(&session("bob"), &user("alice"), "x", true)
                .await
        ),
        "server_admin_required"
    );
    assert_eq!(
        code(
            w.access
                .admin_delete_account(&root, &user("nobody"), "x", true)
                .await
        ),
        "user_not_found"
    );

    let scheduled = w
        .access
        .admin_delete_account(&root, &user("alice"), "inactive for a year", false)
        .await
        .unwrap()
        .unwrap();
    assert!(!scheduled.by_owner);
    assert_eq!(
        scheduled.purge_at - scheduled.scheduled_at,
        Duration::days(30)
    );
    assert!(w.store.account(&user("alice")).await.unwrap().is_some());

    let gone = w
        .access
        .admin_delete_account(&root, &user("alice"), "inactive for a year", true)
        .await
        .unwrap();
    assert!(gone.is_none());
    assert!(w.store.account(&user("alice")).await.unwrap().is_none());
    let log = server_audit(&w).await;
    let last = log.last().unwrap();
    assert_eq!(last.0, "root");
    assert_eq!(last.1, "account_deleted");
    assert!(
        last.2.contains("alice") && last.2.contains("inactive for a year"),
        "{}",
        last.2
    );
    assert!(
        log.iter()
            .any(|e| e.1 == "account_deletion_scheduled" && e.0 == "root")
    );
}

#[tokio::test]
async fn a_purged_name_is_reserved_for_registration_creation_and_organizations() {
    let w = world();
    let root = admin(&w, "root").await;
    sign_up(&w, "alice").await;
    disable(&w, &root, "alice").await;
    w.access
        .admin_delete_account(&root, &user("alice"), "gone", true)
        .await
        .unwrap();

    let again = w
        .access
        .register(Registration::new("alice", PASSWORD))
        .await;
    assert_eq!(code(again), "name_reserved");
    sign_up(&w, "bob").await;
    assert_eq!(
        code(w.access.create_org(&session("bob"), "alice").await),
        "name_reserved"
    );
    assert_eq!(
        code(
            w.access
                .admin_create_org(&root, "alice", &user("bob"))
                .await
        ),
        "name_reserved"
    );
    w.repos
        .create(&session("bob"), &user("bob"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("bob", "game").await.unwrap();
    let added = w
        .access
        .add_user(
            &w.access
                .principal(&open.record.id, &user("bob"))
                .await
                .unwrap(),
            &open.record.id,
            "alice",
            PASSWORD,
            pyn_core::Role::Reader,
        )
        .await;
    assert_eq!(code(added), "name_reserved");
}

#[tokio::test]
async fn a_name_found_nowhere_is_released_after_90_days_and_one_that_acted_is_kept() {
    let w = world();
    let root = admin(&w, "root").await;
    for name in ["quiet", "loud"] {
        sign_up(&w, name).await;
    }
    disable(&w, &root, "quiet").await;
    w.access
        .admin_delete_account(&root, &user("quiet"), "gone", true)
        .await
        .unwrap();
    w.access
        .schedule_own_deletion(&session("loud"), PASSWORD)
        .await
        .unwrap();
    w.clock.advance(Duration::days(30));
    let sweep = w.access.run_deletion_sweep().await.unwrap();
    assert_eq!(sweep.purged, [user("loud")]);

    w.clock.advance(Duration::days(59));
    assert_eq!(
        code(
            w.access
                .register(Registration::new("quiet", PASSWORD))
                .await
        ),
        "name_reserved",
        "still inside the 90 days"
    );
    w.clock.advance(Duration::days(2));
    let sweep = w.access.run_deletion_sweep().await.unwrap();
    assert_eq!(sweep.names_released, 1, "only the name found nowhere");
    w.access
        .register(Registration::new("quiet", PASSWORD))
        .await
        .unwrap();
    assert_eq!(
        code(w.access.register(Registration::new("loud", PASSWORD)).await),
        "name_reserved",
        "a name in the audit log stays reserved"
    );
    w.clock.advance(Duration::days(3650));
    assert!(w.access.run_deletion_sweep().await.unwrap().is_empty());
    assert_eq!(
        code(w.access.register(Registration::new("loud", PASSWORD)).await),
        "name_reserved"
    );
}

#[tokio::test]
async fn the_timer_purges_when_a_window_ends_and_is_safe_to_repeat() {
    let w = world();
    for name in ["alice", "bob"] {
        sign_up(&w, name).await;
    }
    w.access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap();
    w.clock.advance(Duration::days(29));
    assert!(w.access.run_deletion_sweep().await.unwrap().is_empty());
    assert!(w.store.account(&user("alice")).await.unwrap().is_some());

    w.clock.advance(Duration::days(2));
    let sweep = w.access.run_deletion_sweep().await.unwrap();
    assert_eq!(sweep.purged, [user("alice")]);
    assert!(w.store.account(&user("alice")).await.unwrap().is_none());
    assert!(w.store.account(&user("bob")).await.unwrap().is_some());
    assert!(
        w.access.run_deletion_sweep().await.unwrap().is_empty(),
        "a second pass finds nothing"
    );
    let log = server_audit(&w).await;
    let last = log.last().unwrap();
    assert_eq!(last.0, pyn_core::ACCOUNT_DELETION_ACTOR);
    assert_eq!(last.1, "account_deleted");
}

#[tokio::test]
async fn the_timer_deletes_accounts_disabled_longer_than_the_retention() {
    let w = world();
    let root = admin(&w, "root").await;
    for name in ["old", "recent", "active"] {
        sign_up(&w, name).await;
    }
    disable(&w, &root, "old").await;
    w.clock.advance(Duration::days(60));
    disable(&w, &root, "recent").await;
    w.clock.advance(Duration::days(31));

    let sweep = w.access.run_deletion_sweep().await.unwrap();
    assert_eq!(sweep.purged, [user("old")], "90 days after its disable");
    assert!(w.store.account(&user("recent")).await.unwrap().is_some());
    assert!(w.store.account(&user("active")).await.unwrap().is_some());

    w.clock.advance(Duration::days(60));
    assert_eq!(
        w.access.run_deletion_sweep().await.unwrap().purged,
        [user("recent")]
    );
    let last = server_audit(&w).await.pop().unwrap();
    assert!(last.2.contains("90 days"), "{}", last.2);
}

#[tokio::test]
async fn a_retention_of_zero_turns_automatic_deletion_off() {
    let w = world_with(|c| c.disabled_retention_days = 0);
    let root = admin(&w, "root").await;
    sign_up(&w, "alice").await;
    disable(&w, &root, "alice").await;
    w.clock.advance(Duration::days(3650));
    assert!(
        w.access
            .run_deletion_sweep()
            .await
            .unwrap()
            .purged
            .is_empty()
    );
    assert!(w.store.account(&user("alice")).await.unwrap().is_some());
}

#[tokio::test]
async fn the_timer_holds_back_a_blocked_account_and_retries_once_it_is_clear() {
    let w = world();
    let root = admin(&w, "root").await;
    sign_up(&w, "alice").await;
    w.repos
        .create(&session("alice"), &user("alice"), "game", None, None)
        .await
        .unwrap();
    disable(&w, &root, "alice").await;
    w.clock.advance(Duration::days(91));

    let sweep = w.access.run_deletion_sweep().await.unwrap();
    assert!(sweep.purged.is_empty());
    assert_eq!(sweep.blocked.len(), 1);
    assert!(
        sweep.blocked[0].1.contains("alice/game"),
        "{:?}",
        sweep.blocked
    );
    assert!(w.store.account(&user("alice")).await.unwrap().is_some());

    let game = w.repos.open("alice", "game").await.unwrap().record;
    w.repos.delete(&session("alice"), &game).await.unwrap();
    assert_eq!(
        w.access.run_deletion_sweep().await.unwrap().purged,
        [user("alice")]
    );
}

#[tokio::test]
async fn a_restart_picks_up_the_stored_schedule_and_a_stale_one_is_dropped() {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
    ));
    let store = Arc::new(MemoryAccessStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let before = World::on(
        store.clone(),
        meta.clone(),
        audit.clone(),
        clock.clone(),
        |_| {},
    );
    for name in ["alice", "bob"] {
        sign_up(&before, name).await;
    }
    before
        .access
        .schedule_own_deletion(&session("alice"), PASSWORD)
        .await
        .unwrap();
    before
        .access
        .schedule_own_deletion(&session("bob"), PASSWORD)
        .await
        .unwrap();
    // Bob is enabled behind the service's back, as a crash between the two steps of an enable would leave it.
    store.set_disabled(&user("bob"), None).await.unwrap();
    drop(before);

    clock.advance(Duration::days(31));
    let after = World::on(store.clone(), meta, audit, clock, |_| {});
    let sweep = after.access.run_deletion_sweep().await.unwrap();
    assert_eq!(sweep.purged, [user("alice")]);
    assert_eq!(sweep.dropped, [user("bob")]);
    assert!(store.account(&user("bob")).await.unwrap().is_some());
    assert!(
        store
            .account_deletion(&user("bob"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn enabling_an_account_cancels_its_scheduled_deletion() {
    let w = world();
    let root = admin(&w, "root").await;
    sign_up(&w, "alice").await;
    disable(&w, &root, "alice").await;
    w.access
        .admin_delete_account(&root, &user("alice"), "later", false)
        .await
        .unwrap();
    w.access
        .enable_account(&root, &user("alice"))
        .await
        .unwrap();
    assert!(
        w.store
            .account_deletion(&user("alice"))
            .await
            .unwrap()
            .is_none()
    );
    w.clock.advance(Duration::days(120));
    assert!(
        w.access
            .run_deletion_sweep()
            .await
            .unwrap()
            .purged
            .is_empty()
    );
    assert_eq!(AuditAction::AccountDeleted.to_string(), "account_deleted");
}

#[tokio::test]
async fn a_rename_cannot_take_a_purged_accounts_reserved_name() {
    let w = world();
    let root = admin(&w, "root").await;
    sign_up(&w, "alice").await;
    sign_up(&w, "bob").await;
    disable(&w, &root, "alice").await;
    w.access
        .admin_delete_account(&root, &user("alice"), "gone", true)
        .await
        .unwrap();
    let refused = w
        .access
        .rename_account(&session("bob"), "alice", Some(PASSWORD))
        .await;
    assert_eq!(code(refused), "name_reserved");
    w.access
        .rename_account(&session("bob"), "robert", Some(PASSWORD))
        .await
        .unwrap();
}

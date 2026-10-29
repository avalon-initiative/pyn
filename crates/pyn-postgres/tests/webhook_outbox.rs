use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use pyn_core::contract::{Access, Store, Webhooks};
use pyn_core::{
    AccessStore, AccountId, AccountKind, DeliveryQuery, EventDraft, Limits, LimitsChange,
    MetadataStore, NewAccount, NewSubscription, PynError, RepoId, RepoRecord, RepoSettings,
    RepoUpdate, ServerSettings, SignupStage, UserId, Visibility, WebhookStore,
};
use pyn_postgres::PgMetadataStore;

async fn pg() -> Arc<PgMetadataStore> {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    )
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap()
}

fn good() -> EventDraft {
    EventDraft {
        event_type: "test.event".into(),
        payload: r#"{"ok":true}"#.into(),
        at: t0(),
    }
}

fn bad() -> EventDraft {
    EventDraft {
        event_type: "test.event".into(),
        payload: "not json".into(),
        at: t0(),
    }
}

fn account(name: &str) -> NewAccount {
    NewAccount {
        id: AccountId::generate(AccountKind::User).unwrap(),
        user: UserId::new(name),
        email: None,
        password_hash: "hash".into(),
        signup: SignupStage::Complete,
        created_at: t0(),
    }
}

fn repo(id: &str, name: &str) -> RepoRecord {
    RepoRecord {
        id: RepoId::new(id),
        owner: UserId::new("alice"),
        name: name.into(),
        visibility: Visibility::Private,
        settings: RepoSettings::default(),
        storage_cap: None,
        created_at: t0(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn changes_queue_their_events_in_order() {
    let store = pg().await;
    let (meta, access, hooks): (Store, Access, Webhooks) =
        (store.clone(), store.clone(), store.clone());
    pyn_core::contract::changes_queue_their_events_in_order(meta, access, hooks).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn lock_changes_queue_their_events() {
    let store = pg().await;
    let (meta, access, hooks): (Store, Access, Webhooks) =
        (store.clone(), store.clone(), store.clone());
    pyn_core::contract::lock_changes_queue_their_events(meta, access, hooks).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn refused_changes_queue_no_event() {
    let store = pg().await;
    let (meta, access, hooks): (Store, Access, Webhooks) =
        (store.clone(), store.clone(), store.clone());
    pyn_core::contract::refused_changes_queue_no_event(meta, access, hooks).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_failed_event_append_rolls_the_change_back() {
    let s = pg().await;
    let alice = UserId::new("alice");
    let sub = s
        .create_subscription(
            NewSubscription {
                name: "all".into(),
                url: "https://example.test/hook".into(),
                event_types: vec![],
                enabled: true,
                created_by: alice.clone(),
            },
            t0(),
        )
        .await
        .unwrap();
    let failed = |r: Result<_, PynError>| assert!(r.is_err());

    failed(
        s.create_account_with_events(account("alice"), &[good(), bad()])
            .await
            .map(|_| ()),
    );
    assert!(s.account(&alice).await.unwrap().is_none());
    failed(
        s.create_user_with_events(&alice, &AccountId::new("uaa"), t0(), &[bad()])
            .await
            .map(|_| ()),
    );
    assert!(!s.user_exists(&alice).await.unwrap());
    failed(
        s.complete_setup_with_events(
            account("root"),
            ServerSettings {
                initialised_at: t0(),
                server_name: None,
                public_url: None,
                registration: None,
            },
            &[bad()],
        )
        .await
        .map(|_| ()),
    );
    assert!(s.server_settings().await.unwrap().is_none());
    failed(
        s.create_org_with_events(
            &UserId::new("acme"),
            &AccountId::new("oaa"),
            &alice,
            t0(),
            &[bad()],
        )
        .await
        .map(|_| ()),
    );
    assert!(!s.user_exists(&UserId::new("acme")).await.unwrap());
    assert!(!s.user_exists(&alice).await.unwrap());

    assert!(s.create_user(&alice, t0()).await.unwrap());
    failed(
        s.set_disabled_with_events(&alice, Some((t0(), None)), &[bad()])
            .await
            .map(|_| ()),
    );
    assert!(
        s.account(&alice)
            .await
            .unwrap()
            .unwrap()
            .disabled_at
            .is_none()
    );
    failed(
        s.disable_account_with_events(&alice, t0(), None, &[bad()])
            .await
            .map(|_| ()),
    );
    assert!(
        s.account(&alice)
            .await
            .unwrap()
            .unwrap()
            .disabled_at
            .is_none()
    );
    failed(
        s.set_signup_with_events(&alice, SignupStage::PendingApproval, &[bad()])
            .await
            .map(|_| ()),
    );
    assert_eq!(
        s.account(&alice).await.unwrap().unwrap().signup,
        SignupStage::Complete
    );
    failed(
        s.update_owner_limits_with_events(
            &alice,
            LimitsChange {
                repositories: Some(Some(2)),
                ..Default::default()
            },
            &|_| Ok(bad()),
        )
        .await
        .map(|_| ()),
    );
    assert_eq!(s.owner_limits(&alice).await.unwrap(), Limits::default());

    failed(
        s.create_repo_capped_with_events(repo("r1", "game"), None, &[bad()])
            .await
            .map(|_| ()),
    );
    assert!(s.find_repo(&alice, "game").await.unwrap().is_none());
    s.create_repo(repo("r1", "game")).await.unwrap();
    failed(
        s.update_repo_with_events(
            &RepoId::new("r1"),
            RepoUpdate {
                name: Some("game2".into()),
                ..Default::default()
            },
            &[bad()],
        )
        .await
        .map(|_| ()),
    );
    assert!(s.find_repo(&alice, "game").await.unwrap().is_some());
    failed(
        s.delete_repo_with_events(&RepoId::new("r1"), &[bad()])
            .await
            .map(|_| ()),
    );
    assert!(s.find_repo(&alice, "game").await.unwrap().is_some());

    let org = UserId::new("acme");
    assert!(s.create_org(&org, &alice, t0()).await.unwrap());
    failed(s.delete_org_with_events(&org, &[bad()]).await.map(|_| ()));
    assert!(s.user_exists(&org).await.unwrap());

    let queued = s
        .list_deliveries(
            sub.id,
            &DeliveryQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(queued.is_empty());
}

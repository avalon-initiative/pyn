use chrono::{Duration, Utc};
use pyn_core::{
    MetadataStore, NewSubscription, RateLimitStore, RepoId, RepoPath, UserId, WebhookStore,
};
use pyn_postgres::{PgMetadataStore, TimeSource};

async fn store() -> PgMetadataStore {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    PgMetadataStore::connect_in_scratch_schema(&url)
        .await
        .unwrap()
        .with_time_source(TimeSource::Database)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn lease_expiry_ignores_a_skewed_caller_clock() {
    let store = store().await;
    let (repo, path) = (RepoId::new("r"), RepoPath::new("a").unwrap());
    let (amy, bob) = (UserId::new("amy"), UserId::new("bob"));
    let real = Utc::now();
    let ahead = real + Duration::days(1);
    let behind = real - Duration::days(1);

    // A copy a day behind still gives a one-hour lease; one a day ahead sees it as live.
    let lock = store
        .acquire_lock(&repo, &path, &amy, behind, behind + Duration::hours(1), 5)
        .await
        .unwrap();
    assert!(lock.expires_at > real);
    assert!(store.get_lock(&repo, &path, ahead).await.unwrap().is_some());
    assert!(
        store
            .acquire_lock(&repo, &path, &bob, ahead, ahead + Duration::hours(1), 5)
            .await
            .is_err()
    );
    assert_eq!(store.list_locks(&repo, ahead).await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn rate_windows_ignore_a_skewed_caller_clock() {
    let store = store().await;
    let window = Duration::minutes(10);
    let ahead = Utc::now() + Duration::days(1);

    assert_eq!(store.hit("k", window, ahead).await.unwrap().count, 1);
    assert_eq!(store.hit("k", window, ahead).await.unwrap().count, 2);
    let state = store.state("k", ahead).await.unwrap().unwrap();
    assert!(state.resets_at < ahead);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn webhook_claims_and_leases_ignore_a_skewed_caller_clock() {
    let store = store().await;
    let real = Utc::now();
    let ahead = real + Duration::days(1);
    let new = NewSubscription {
        name: "s".into(),
        url: "https://example.test/s".into(),
        event_types: vec![],
        enabled: true,
        created_by: UserId::new("amy"),
    };
    store.create_subscription(new, ahead).await.unwrap();
    let event = store.append_event("e", "{}", ahead).await.unwrap();
    assert!(event.created_at < ahead);

    let lease = Duration::minutes(5);
    let got = store.claim_due(1, lease, ahead).await.unwrap();
    assert_eq!(got.len(), 1);
    let until = got[0].delivery.claimed_until.unwrap();
    assert!(until < real + Duration::hours(1));
    assert!(store.claim_due(1, lease, ahead).await.unwrap().is_empty());
}

use std::sync::Arc;

use pyn_core::contract::Webhooks;
use pyn_postgres::PgMetadataStore;

async fn postgres() -> Webhooks {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    )
}

pyn_core::webhook_contract_tests!(postgres(), #[ignore = "needs PYN_DATABASE_URL"]);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn independent_services_racing_a_limit_record_exactly_one_event() {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let pg = Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    );
    let search_path = format!("SET search_path TO \"{}\"", pg.scratch_schema().unwrap());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(move |conn, _| {
            let statement = search_path.clone();
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(statement))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let other = Arc::new(
        PgMetadataStore::from_pool(pool)
            .await
            .unwrap()
            .with_time_source(pyn_postgres::TimeSource::Caller),
    );
    pyn_core::contract::racing_limit_services_record_one_event(pg.clone(), pg.clone(), [pg, other])
        .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_failed_outbox_append_does_not_consume_admission() {
    use chrono::{TimeZone, Utc};
    use pyn_core::{DeliveryQuery, NewSubscription, UserId};

    let store = postgres().await;
    let now = Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap();
    let owner = UserId::new("alice");
    let sub = store
        .create_subscription(
            NewSubscription {
                name: "all".into(),
                url: "https://example.test/hook".into(),
                event_types: vec![],
                enabled: true,
                created_by: owner.clone(),
            },
            now,
        )
        .await
        .unwrap();
    assert!(
        store
            .append_limit_reached(&owner, "locks", "invalid json", now)
            .await
            .is_err()
    );
    assert!(
        store
            .append_limit_reached(&owner, "locks", "{}", now)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .append_limit_reached(&owner, "locks", "{}", now)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .list_deliveries(
                sub.id,
                &DeliveryQuery {
                    limit: 10,
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn limit_admission_uses_the_injected_time_even_with_database_lease_time() {
    use chrono::{Duration, TimeZone, Utc};
    use pyn_core::{Clock, ManualClock, UserId, WebhookStore};
    use pyn_postgres::TimeSource;

    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let store = PgMetadataStore::connect_in_scratch_schema(&url)
        .await
        .unwrap()
        .with_time_source(TimeSource::Database);
    let clock = ManualClock::new(Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap());
    let owner = UserId::new("alice");
    assert!(
        store
            .append_limit_reached(&owner, "locks", "{}", clock.now())
            .await
            .unwrap()
            .is_some()
    );
    clock.advance(Duration::hours(1) - Duration::microseconds(1));
    assert!(
        store
            .append_limit_reached(&owner, "locks", "{}", clock.now())
            .await
            .unwrap()
            .is_none()
    );
    clock.advance(Duration::microseconds(1));
    let event = store
        .append_limit_reached(&owner, "locks", "{}", clock.now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.created_at, clock.now());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn an_unavailable_event_store_preserves_the_limit_error() {
    use chrono::{TimeZone, Utc};
    use pyn_core::memory::{MemoryAuditStore, MemoryObjectStore};
    use pyn_core::{
        AccessConfig, AccessService, Credential, Identity, Limits, ManualClock, PynError,
        Repositories, Rules, UserId, WebhookSettings,
    };

    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let pg = Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    );
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP TABLE \"{}\".webhook_limit_events",
        pg.scratch_schema().unwrap()
    )))
    .execute(&pool)
    .await
    .unwrap();
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
    ));
    let access = Arc::new(
        AccessService::new(pg.clone(), clock.clone())
            .with_config(AccessConfig {
                default_limits: Limits {
                    repositories: Some(0),
                    ..Default::default()
                },
                ..AccessConfig::default()
            })
            .with_registry(pg.clone())
            .with_webhooks(pg.clone(), WebhookSettings::default()),
    );
    let repos = Repositories::new(
        pg,
        Arc::new(MemoryObjectStore::new()),
        Arc::new(MemoryAuditStore::new()),
        access,
        clock,
        Rules::empty(),
    );
    let who = Identity {
        user: UserId::new("alice"),
        credential: Credential::Session,
    };
    let err = repos
        .create(&who, &who.user, "refused", None, None)
        .await
        .unwrap_err();
    assert!(matches!(&err, PynError::RepoLimitReached { owner, limit: 0 } if owner == "alice"));
    assert_eq!(err.code(), "repo_limit_reached");
    assert_eq!(
        err.to_string(),
        "repository limit reached: alice may own at most 0 repositories on this server"
    );
    pool.close().await;
}

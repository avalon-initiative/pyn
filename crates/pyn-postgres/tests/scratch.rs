use pyn_postgres::PgMetadataStore;
use sqlx::PgPool;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn scratch_schema_is_dropped_with_the_store() {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let store = PgMetadataStore::connect_in_scratch_schema(&url)
        .await
        .unwrap();
    let schema = store.scratch_schema().unwrap().to_string();
    drop(store);

    let pool = PgPool::connect(&url).await.unwrap();
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.schemata WHERE schema_name = $1",
    )
    .bind(&schema)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0);
}

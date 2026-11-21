//! The shared contract against a real S3-compatible service, behind the `real-endpoint` feature:
//! `PYN_S3_TEST_ENDPOINT=... PYN_S3_TEST_BUCKET=... PYN_S3_TEST_ACCESS_KEY_ID=... PYN_S3_TEST_SECRET_ACCESS_KEY=...
//! cargo test -p pyn-s3 --features real-endpoint --test real_endpoint -- --ignored` (optional: `PYN_S3_TEST_REGION`, `PYN_S3_TEST_PATH_STYLE`).
//! Each test writes under its own random key prefix and leaves the objects there.

#![cfg(feature = "real-endpoint")]

use std::sync::Arc;

use pyn_core::ObjectStore;
use pyn_s3::{S3ObjectStore, S3Settings};

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn required(name: &str) -> String {
    var(name).unwrap_or_else(|| panic!("{name} is not set"))
}

async fn real() -> (Arc<dyn ObjectStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = S3Settings::new(
        required("PYN_S3_TEST_ENDPOINT"),
        required("PYN_S3_TEST_BUCKET"),
        required("PYN_S3_TEST_ACCESS_KEY_ID"),
        required("PYN_S3_TEST_SECRET_ACCESS_KEY"),
    );
    if let Some(region) = var("PYN_S3_TEST_REGION") {
        settings.region = region;
    }
    settings.path_style = var("PYN_S3_TEST_PATH_STYLE").is_some_and(|v| v == "1" || v == "true");
    let mut id = [0u8; 8];
    getrandom::fill(&mut id).unwrap();
    settings.prefix = format!("pyn-s3-test/{}/", hex::encode(id));
    settings.spool_dir = dir.path().join("spool");
    settings.spool_threshold = 64 * 1024;
    let store = S3ObjectStore::open(settings).await.unwrap();
    (Arc::new(store), dir)
}

pyn_core::object_store_contract_tests!(real(), #[ignore = "needs PYN_S3_TEST_* variables"]);

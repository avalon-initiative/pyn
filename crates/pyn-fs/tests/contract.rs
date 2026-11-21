use std::sync::Arc;

use pyn_core::ObjectStore;
use pyn_fs::FsObjectStore;

async fn fs() -> (Arc<dyn ObjectStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = FsObjectStore::open(dir.path().join("objects"))
        .await
        .unwrap();
    (Arc::new(store), dir)
}

pyn_core::object_store_contract_tests!(fs());

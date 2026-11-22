#![allow(dead_code)]

use std::sync::Arc;

use pyn_core::ObjectStore;
use pyn_s3::S3ObjectStore;
#[allow(unused_imports)]
pub use pyn_s3::testing::{ACCESS_KEY, BUCKET, SECRET_KEY, Server};

pub async fn in_process() -> (Arc<dyn ObjectStore>, Server) {
    let server = Server::start();
    let store = S3ObjectStore::open(server.settings()).await.unwrap();
    (Arc::new(store), server)
}

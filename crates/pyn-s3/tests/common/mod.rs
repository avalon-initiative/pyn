#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use pyn_core::ObjectStore;
use pyn_s3::{S3ObjectStore, S3Settings};
use s3s::auth::SimpleAuth;
use s3s::service::S3ServiceBuilder;
use tokio::net::TcpListener;

pub const ACCESS_KEY: &str = "pyn-test-access";
pub const SECRET_KEY: &str = "pyn-test-secret-key-0123456789";
pub const BUCKET: &str = "pyn-test";

/// An in-process S3 server over a temporary directory; dropping it stops it.
pub struct Server {
    pub addr: std::net::SocketAddr,
    pub root: PathBuf,
    _dir: tempfile::TempDir,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        std::fs::create_dir_all(root.join(BUCKET)).unwrap();
        let fs = s3s_fs::FileSystem::new(&root).unwrap();
        let service = {
            let mut b = S3ServiceBuilder::new(fs);
            b.set_auth(SimpleAuth::from_single(ACCESS_KEY, SECRET_KEY));
            b.build()
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    continue;
                };
                let conn = Builder::new(TokioExecutor::new())
                    .serve_connection(TokioIo::new(socket), service.clone())
                    .into_owned();
                tokio::spawn(async move {
                    let _ = conn.await;
                });
            }
        });
        Self {
            addr,
            root,
            _dir: dir,
            task,
        }
    }

    pub fn settings(&self) -> S3Settings {
        let mut s = S3Settings::new(
            format!("http://{}", self.addr),
            BUCKET,
            ACCESS_KEY,
            SECRET_KEY,
        );
        s.path_style = true;
        s.spool_dir = self._dir.path().join("spool");
        s.spool_threshold = 64 * 1024;
        s.retry_base = Duration::from_millis(5);
        s
    }
}

pub async fn in_process() -> (Arc<dyn ObjectStore>, Server) {
    let server = Server::start().await;
    let store = S3ObjectStore::open(server.settings()).await.unwrap();
    (Arc::new(store), server)
}

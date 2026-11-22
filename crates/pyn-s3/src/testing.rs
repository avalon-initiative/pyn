//! An in-process S3 server over a temporary directory, for tests.

use std::path::PathBuf;
use std::time::Duration;

use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use s3s::auth::SimpleAuth;
use s3s::service::S3ServiceBuilder;
use tokio::net::TcpListener;

use crate::S3Settings;

pub const ACCESS_KEY: &str = "pyn-test-access";
pub const SECRET_KEY: &str = "pyn-test-secret-key-0123456789";
pub const BUCKET: &str = "pyn-test";

/// Runs on its own thread and runtime, so it works from sync and async tests alike; dropping it stops it.
pub struct Server {
    pub addr: std::net::SocketAddr,
    /// The directory holding one folder per bucket.
    pub root: PathBuf,
    dir: tempfile::TempDir,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Server {
    pub fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        std::fs::create_dir_all(root.join(BUCKET)).unwrap();
        let fs = s3s_fs::FileSystem::new(&root).unwrap();
        let service = {
            let mut b = S3ServiceBuilder::new(fs);
            b.set_auth(SimpleAuth::from_single(ACCESS_KEY, SECRET_KEY));
            b.build()
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let (ready, bound) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                ready.send(listener.local_addr().unwrap()).unwrap();
                let serve = async {
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
                };
                tokio::select! { () = serve => {}, _ = stopped => {} }
            });
        });
        Self {
            addr: bound.recv().unwrap(),
            root,
            dir,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// Settings that reach the bucket, with a short retry delay and a small spool threshold.
    pub fn settings(&self) -> S3Settings {
        let mut s = S3Settings::new(
            format!("http://{}", self.addr),
            BUCKET,
            ACCESS_KEY,
            SECRET_KEY,
        );
        s.path_style = true;
        s.spool_dir = self.dir.path().join("spool");
        s.spool_threshold = 64 * 1024;
        s.retry_base = Duration::from_millis(5);
        s
    }
}

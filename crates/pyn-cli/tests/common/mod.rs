//! Starts a real server in this process and runs the real `pyn` binary against it.
//! Each test binary uses a different part of this, so unused helpers are expected.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessService, AuthProvider, RepoId, RepoService, Rules, ServiceConfig, SystemClock,
};
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};

pub struct Env {
    pub url: String,
    _runtime: tokio::runtime::Runtime,
    config: tempfile::TempDir,
    work: tempfile::TempDir,
}

pub fn start() -> Env {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let repo = RepoId::new("t");
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let rules = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";
    let service = Arc::new(RepoService::new(
        repo.clone(),
        Rules::from_toml(rules).unwrap(),
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        Arc::new(MemoryAuditStore::new()),
        clock.clone(),
        ServiceConfig::default(),
    ));
    let access = Arc::new(AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        clock,
    ));
    let app = router(AppState {
        service,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access, repo }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
    });
    let url = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    });
    Env {
        url,
        _runtime: runtime,
        config: tempfile::tempdir().unwrap(),
        work: tempfile::tempdir().unwrap(),
    }
}

impl Env {
    pub fn dir(&self, name: &str) -> PathBuf {
        let p = self.work.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    pub fn run(&self, cwd: &Path, user: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("PYN_SERVER", &self.url)
            .env("PYN_USER", user)
            .env("PYN_CONFIG_DIR", self.config.path())
            .env_remove("PYN_TOKEN")
            .env_remove("PYN_DIR")
            .output()
            .unwrap()
    }

    pub fn ok(&self, cwd: &Path, user: &str, args: &[&str]) -> String {
        let out = self.run(cwd, user, args);
        assert!(
            out.status.success(),
            "pyn {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    pub fn fails(&self, cwd: &Path, user: &str, args: &[&str]) -> String {
        let out = self.run(cwd, user, args);
        assert!(!out.status.success(), "pyn {args:?} should have failed");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// Checks `text` in as `path` from outside any workspace.
    pub fn seed(&self, user: &str, path: &str, text: &str, base: Option<u64>) {
        let dir = self.dir("seed");
        let file = dir.join("upload");
        std::fs::write(&file, text).unwrap();
        let base = base.map(|b| b.to_string());
        let mut extra: Vec<&str> = Vec::new();
        if let Some(b) = &base {
            extra.extend(["--base", b]);
        }
        if path.starts_with("Content/") {
            let mut args = vec!["checkout", path];
            args.extend(&extra);
            self.ok(&dir, user, &args);
        }
        let mut args = vec!["checkin", path, file.to_str().unwrap(), "-m", "seed"];
        args.extend(&extra);
        self.ok(&dir, user, &args);
    }
}

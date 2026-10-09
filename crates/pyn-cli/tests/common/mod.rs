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
    AccessService, AuthProvider, MetadataStore, RepoId, RepoRecord, RepoSettings, Repositories,
    Rules, SystemClock, UserId, Visibility,
};
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};

/// The repository `start` registers and the commands default to.
pub const REPO: &str = "alice/game";

pub struct Env {
    pub url: String,
    default_repo: Option<&'static str>,
    _runtime: tokio::runtime::Runtime,
    config: tempfile::TempDir,
    work: tempfile::TempDir,
}

/// A server with `alice/game` registered, and `PYN_REPO` pointing at it.
pub fn start() -> Env {
    start_with(true)
}

/// A server with no repositories.
pub fn start_empty() -> Env {
    start_with(false)
}

fn start_with(with_repo: bool) -> Env {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let rules = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";
    let meta = Arc::new(MemoryMetadataStore::new());
    if with_repo {
        runtime
            .block_on(meta.create_repo(RepoRecord {
                id: RepoId::new("t"),
                owner: UserId::new("alice"),
                name: "game".into(),
                visibility: Visibility::Private,
                settings: RepoSettings::default(),
                created_at: chrono::Utc::now(),
            }))
            .unwrap();
    }
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_registry(meta.clone()),
    );
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::from_toml(rules).unwrap(),
    ));
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    });
    let url = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    });
    Env {
        url,
        default_repo: with_repo.then_some(REPO),
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

    /// Runs the CLI with the timezone pinned to UTC.
    pub fn run(&self, cwd: &Path, user: &str, args: &[&str]) -> Output {
        self.run_in_tz(cwd, user, args, "UTC")
    }

    pub fn run_in_tz(&self, cwd: &Path, user: &str, args: &[&str], tz: &str) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pyn"));
        cmd.args(args)
            .current_dir(cwd)
            .env("PYN_SERVER", &self.url)
            .env("PYN_USER", user)
            .env("PYN_CONFIG_DIR", self.config.path())
            .env("TZ", tz)
            .env_remove("PYN_TOKEN")
            .env_remove("PYN_DIR")
            .env_remove("PYN_REPO");
        if let Some(repo) = self.default_repo {
            cmd.env("PYN_REPO", repo);
        }
        cmd.output().unwrap()
    }

    /// The clone source for `owner/name` on this server.
    pub fn source(&self, repo: &str) -> String {
        format!("{}/{repo}", self.url)
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

/// Splits table output into rows of cells; columns are separated by two or more spaces.
pub fn cell_rows(out: &str) -> Vec<Vec<String>> {
    out.lines()
        .map(|l| {
            l.split("  ")
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect()
        })
        .collect()
}

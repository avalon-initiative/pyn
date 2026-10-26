//! The real `pyn` and `pyn-server` binaries (in-memory store) for the end-to-end suites.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant};

pub const SETUP_TOKEN: &str = "0123456789abcdef0123456789abcdef";
pub const PASSWORD: &str = "correct horse battery";
pub const POLICY: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n\
                      [shared]\npaths = [\".pyn/pyn.toml\"]\n";

fn server_binary() -> PathBuf {
    let pyn = Path::new(env!("CARGO_BIN_EXE_pyn"));
    let path = pyn.with_file_name("pyn-server");
    static BUILD: Once = Once::new();
    BUILD.call_once(|| {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(["build", "-p", "pyn-server"])
            .status()
            .unwrap();
        assert!(status.success(), "building pyn-server failed");
    });
    assert!(path.is_file(), "{} was not built", path.display());
    path
}

/// A spawned `pyn-server` with an in-memory store, set up with an open registration.
pub struct Server {
    child: Child,
    pub url: String,
    tmp: tempfile::TempDir,
}

impl Server {
    pub fn start() -> Self {
        Self::start_with_env(&[])
    }

    /// Like `start`, with extra environment variables for the server process.
    pub fn start_with_env(vars: &[(&str, &str)]) -> Self {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");
        let child = Command::new(server_binary())
            .env("PYN_ADDR", &addr)
            .env("PYN_SETUP_TOKEN", SETUP_TOKEN)
            .env("PYN_REGISTRATION", "open")
            .env("PYN_EMAIL_VERIFICATION", "false")
            .env_remove("PYN_DATABASE_URL")
            .env_remove("PYN_DATA_DIR")
            .env_remove("PYN_CONFIG")
            .envs(vars.iter().copied())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let server = Self {
            child,
            url: format!("http://{addr}"),
            tmp: tempfile::tempdir().unwrap(),
        };
        server.wait_until_healthy(&addr);
        server
    }

    fn wait_until_healthy(&self, addr: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if let Ok(mut s) = TcpStream::connect(addr) {
                let _ = s.write_all(b"GET /healthz HTTP/1.0\r\n\r\n");
                let mut reply = String::new();
                let _ = s.read_to_string(&mut reply);
                if reply.starts_with("HTTP/1.0 200") || reply.starts_with("HTTP/1.1 200") {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("pyn-server did not become healthy");
    }

    /// A fresh empty directory under the server's scratch space.
    pub fn dir(&self, name: &str) -> PathBuf {
        let p = self.tmp.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    pub fn run_with(&self, cwd: &Path, user: &str, args: &[&str], input: Option<&str>) -> Output {
        let home = self.dir("home");
        let mut child = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &home)
            .env("TZ", "UTC")
            .env("PYN_SERVER", &self.url)
            .env("PYN_CONFIG_DIR", self.dir(&format!("config-{user}")))
            .env("GIT_AUTHOR_NAME", "Author")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("PYN_USER")
            .env_remove("PYN_TOKEN")
            .env_remove("PYN_DIR")
            .env_remove("PYN_REPO")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.unwrap_or("").as_bytes()).unwrap();
        drop(stdin);
        child.wait_with_output().unwrap()
    }

    pub fn ok(&self, cwd: &Path, user: &str, args: &[&str]) -> String {
        let out = self.run_with(cwd, user, args, None);
        assert!(
            out.status.success(),
            "pyn {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The exit code and standard error of a command that must fail.
    pub fn fails(&self, cwd: &Path, user: &str, args: &[&str]) -> (i32, String) {
        let out = self.run_with(cwd, user, args, None);
        assert!(!out.status.success(), "pyn {args:?} should have failed");
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Sets the server up, registers and signs in `alice`, and creates `alice/game` with the test policy.
    pub fn with_game() -> Self {
        Self::with_game_env(&[])
    }

    pub fn with_game_env(vars: &[(&str, &str)]) -> Self {
        let server = Self::start_with_env(vars);
        let cwd = server.dir("admin");
        let input = format!("{PASSWORD}\n");
        for (user, args) in [
            (
                "root",
                vec![
                    "setup",
                    "root",
                    "--setup-token",
                    SETUP_TOKEN,
                    "--registration",
                    "open",
                    "--password-stdin",
                ],
            ),
            ("alice", vec!["register", "alice", "--password-stdin"]),
            ("alice", vec!["login", "alice", "--password-stdin"]),
            ("bob", vec!["register", "bob", "--password-stdin"]),
            ("bob", vec!["login", "bob", "--password-stdin"]),
        ] {
            let out = server.run_with(&cwd, user, &args, Some(&input));
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::fs::write(cwd.join("policy.toml"), POLICY).unwrap();
        server.ok(
            &cwd,
            "alice",
            &["repo", "create", "game", "--policy", "policy.toml"],
        );
        server.ok(
            &cwd,
            "alice",
            &["member", "set", "bob", "writer", "--repo", "alice/game"],
        );
        server
    }

    pub fn source(&self) -> String {
        format!("{}/alice/game", self.url)
    }

    /// Checks `text` in as `path` from a scratch folder, taking the lock first when the path needs one.
    pub fn seed(&self, path: &str, text: &str) {
        let cwd = self.dir("seed");
        let file = cwd.join("upload");
        std::fs::write(&file, text).unwrap();
        let repo = ["--repo", "alice/game"];
        if path.starts_with("Content/") {
            self.ok(&cwd, "alice", &[&["lock", path][..], &repo].concat());
        }
        let file = file.to_str().unwrap();
        let args = ["commit", path, "--file", file, "-m", "seed"];
        self.ok(&cwd, "alice", &[&args[..], &repo].concat());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

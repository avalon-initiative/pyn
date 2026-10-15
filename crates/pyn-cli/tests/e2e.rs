//! End to end: the real `pyn` and `pyn-server` binaries (in-memory store) against real temporary repositories.
//! The git binary appears only as an oracle for what git itself reports.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant};

mod common;

use common::gitfix::snapshot;

const SETUP_TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";
const POLICY: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n\
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
struct Server {
    child: Child,
    url: String,
    tmp: tempfile::TempDir,
}

impl Server {
    fn start() -> Self {
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
    fn dir(&self, name: &str) -> PathBuf {
        let p = self.tmp.path().join(name);
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    fn run_with(&self, cwd: &Path, user: &str, args: &[&str], input: Option<&str>) -> Output {
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

    fn ok(&self, cwd: &Path, user: &str, args: &[&str]) -> String {
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
    fn fails(&self, cwd: &Path, user: &str, args: &[&str]) -> (i32, String) {
        let out = self.run_with(cwd, user, args, None);
        assert!(!out.status.success(), "pyn {args:?} should have failed");
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Sets the server up, registers and signs in `alice`, and creates `alice/game` with the test policy.
    fn with_game() -> Self {
        let server = Self::start();
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

    fn source(&self) -> String {
        format!("{}/alice/game", self.url)
    }

    /// Checks `text` in as `path` from a scratch folder, taking the lock first when the path needs one.
    fn seed(&self, path: &str, text: &str) {
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

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A git repository made by git itself: two commits, one untracked file.
fn git_repo(server: &Server, name: &str) -> PathBuf {
    let dir = server.dir(name);
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "fn a() {}\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    std::fs::write(dir.join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "second"]);
    std::fs::write(dir.join("scratch.txt"), "untracked\n").unwrap();
    dir
}

fn changed(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let mut paths: Vec<String> = after
        .keys()
        .filter(|k| before.get(*k) != after.get(*k))
        .chain(before.keys().filter(|k| !after.contains_key(*k)))
        .cloned()
        .collect();
    paths.sort();
    paths
}

fn read_only(path: &Path) -> bool {
    std::fs::metadata(path).unwrap().permissions().readonly()
}

fn porcelain_paths(out: &str) -> Vec<String> {
    let mut rows = common::gitfix::porcelain(out);
    rows.sort();
    rows
}

#[test]
fn init_in_an_empty_folder_is_refused_with_the_git_command_message() {
    let server = Server::start();
    let empty = server.dir("empty");
    let (code, err) = server.fails(&empty, "-", &["init"]);
    assert_eq!(code, 1, "{err}");
    assert!(
        err.contains("'init' is a git command pyn does not support yet"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);
}

#[test]
fn init_in_a_git_repository_changes_only_the_exclude_file_and_pyn_folder() {
    let server = Server::start();
    let repo = git_repo(&server, "repo");
    let status = git(&repo, &["status", "--porcelain"]);
    let before = snapshot(&repo);

    let out = server.ok(&repo, "-", &["init"]);
    assert!(out.contains("added .pyn/"), "{out}");
    let after = snapshot(&repo);
    assert_eq!(
        changed(&before, &after),
        [".git/info/exclude", ".pyn", ".pyn/local_only"]
    );
    assert_eq!(git(&repo, &["status", "--porcelain"]), status);
    assert!(
        git(&repo, &["check-ignore", ".pyn/local_only"]).contains(".pyn"),
        "git ignores the pyn folder"
    );

    let again = server.ok(&repo, "-", &["init"]);
    assert!(again.contains("already excluded"), "{again}");
    assert_eq!(snapshot(&repo), after, "init is idempotent");
}

#[test]
fn clone_into_a_clean_folder() {
    let server = Server::with_game();
    server.seed("Source/a.cpp", "int a;");
    server.seed("Content/m.umap", "map one");

    let ws = server.dir("clean").join("game");
    let out = server.ok(
        &server.dir("elsewhere"),
        "alice",
        &["clone", &server.source(), ws.to_str().unwrap()],
    );
    assert!(out.contains("cloned"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "int a;"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Content/m.umap")).unwrap(),
        "map one"
    );
    assert!(!read_only(&ws.join("Source/a.cpp")));
    assert!(read_only(&ws.join("Content/m.umap")));
    assert!(ws.join(".pyn/pyn.toml").is_file());
    assert!(ws.join(".pyn/local_only/config.toml").is_file());
    assert!(
        server
            .ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn clone_inside_a_git_repository_is_its_own_pyn_workspace() {
    let server = Server::with_game();
    server.seed("Source/a.cpp", "int a;");
    let repo = git_repo(&server, "outer");
    let status = git(&repo, &["status", "--porcelain"]);
    let git_before = snapshot(&repo.join(".git"));

    let ws = repo.join("vendor/game");
    server.ok(
        &repo,
        "alice",
        &["clone", &server.source(), ws.to_str().unwrap()],
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "int a;"
    );
    assert!(
        server
            .ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    assert!(server.ok(&ws, "alice", &["whoami"]).starts_with("alice"));
    assert_eq!(
        snapshot(&repo.join(".git")),
        git_before,
        "nothing under .git changes"
    );

    let after = git(&repo, &["status", "--porcelain"]);
    assert_eq!(
        after.lines().collect::<Vec<_>>(),
        ["?? scratch.txt", "?? vendor/"],
        "git sees the clone as one untracked folder (was {status:?})"
    );

    let outer = server.ok(&repo, "-", &["status"]);
    assert!(
        outer.contains("README.md") || outer.contains("scratch.txt"),
        "{outer}"
    );
}

#[test]
fn lock_edit_and_commit_an_exclusive_file() {
    let server = Server::with_game();
    server.seed("Content/m.umap", "map one");
    let ws = server.dir("ws").join("game");
    server.ok(
        &server.dir("home"),
        "alice",
        &["clone", &server.source(), ws.to_str().unwrap()],
    );
    let file = ws.join("Content/m.umap");
    assert!(read_only(&file));

    server.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    assert!(!read_only(&file));
    let (_, err) = server.fails(&ws, "bob", &["lock", "Content/m.umap"]);
    assert!(err.contains("locked by alice"), "{err}");
    assert!(
        server
            .ok(&ws, "alice", &["locks"])
            .contains("Content/m.umap")
    );

    std::fs::write(&file, "map two").unwrap();
    let out = server.ok(&ws, "alice", &["commit", "Content/m.umap", "-m", "second"]);
    assert!(out.contains("revision 2"), "{out}");
    assert!(read_only(&file), "commit locks the file down again");
    assert!(
        !server
            .ok(&ws, "alice", &["locks"])
            .contains("Content/m.umap")
    );
    assert_eq!(
        server
            .ok(&ws, "alice", &["log", "Content/m.umap"])
            .lines()
            .count(),
        3
    );
}

#[test]
fn a_path_goes_exclusive_to_shared_and_back_through_policy_commits() {
    let server = Server::with_game();
    server.seed("Content/m.umap", "map one");
    let ws = server.dir("ws").join("game");
    server.ok(
        &server.dir("home"),
        "alice",
        &["clone", &server.source(), ws.to_str().unwrap()],
    );
    server.ok(&ws, "alice", &["lock", "Content/m.umap"]);

    let shared = POLICY.replace("[exclusive]\npaths = [\"Content/\"]\n", "");
    std::fs::write(ws.join("shared.toml"), &shared).unwrap();
    let preview = server.ok(&ws, "alice", &["policy", "preview", "shared.toml"]);
    assert!(
        preview.contains("exclusive -> shared") && preview.contains("Content/m.umap"),
        "{preview}"
    );
    assert!(preview.contains("would release alice's lock"), "{preview}");

    std::fs::write(ws.join(".pyn/pyn.toml"), &shared).unwrap();
    let out = server.ok(
        &ws,
        "alice",
        &["commit", ".pyn/pyn.toml", "-m", "Content is shared"],
    );
    assert!(out.contains("revision"), "{out}");
    assert!(
        !server
            .ok(&ws, "alice", &["locks"])
            .contains("Content/m.umap"),
        "lock released"
    );
    let log = server.ok(&ws, "alice", &["log", "Content/m.umap"]);
    assert_eq!(log.lines().count(), 2, "history intact: {log}");
    let audit = server.ok(&ws, "alice", &["audit", "--action", "mode_converted"]);
    assert!(
        audit.contains("Content/m.umap"),
        "conversion reported: {audit}"
    );

    std::fs::write(ws.join("Content/m.umap"), "map shared edit").unwrap();
    server.ok(
        &ws,
        "alice",
        &["commit", "Content/m.umap", "-m", "no lock needed"],
    );

    std::fs::write(ws.join(".pyn/pyn.toml"), POLICY).unwrap();
    server.ok(
        &ws,
        "alice",
        &[
            "commit",
            ".pyn/pyn.toml",
            "-m",
            "Content is exclusive again",
        ],
    );
    let log = server.ok(&ws, "alice", &["log", "Content/m.umap"]);
    assert_eq!(
        log.lines().count(),
        4,
        "shared edit and conversion revision kept: {log}"
    );
    let (_, err) = server.fails(
        &ws,
        "bob",
        &[
            "commit",
            "Content/m.umap",
            "--file",
            "shared.toml",
            "-m",
            "sneaky",
        ],
    );
    assert!(err.contains("lock"), "{err}");
    server.ok(&ws, "alice", &["update"]);
    server.ok(&ws, "alice", &["lock", "Content/m.umap"]);
}

#[test]
fn shared_files_go_through_add_and_commit() {
    let server = Server::with_game();
    server.seed("Source/a.cpp", "int a;");
    let ws = server.dir("ws").join("game");
    server.ok(
        &server.dir("home"),
        "alice",
        &["clone", &server.source(), ws.to_str().unwrap()],
    );

    std::fs::write(ws.join("Source/a.cpp"), "int a = 1;").unwrap();
    std::fs::write(ws.join("Source/b.cpp"), "int b;").unwrap();
    let status = server.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("Source/a.cpp") && status.contains("Source/b.cpp"),
        "{status}"
    );

    server.ok(&ws, "alice", &["add", "Source/a.cpp", "Source/b.cpp"]);
    let out = server.ok(&ws, "alice", &["commit", "-m", "edit a, add b"]);
    assert!(
        out.contains("revision 2") || out.contains("committed"),
        "{out}"
    );
    assert!(
        server
            .ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );

    let other = server.dir("other").join("game");
    server.ok(
        &server.dir("home2"),
        "alice",
        &["clone", &server.source(), other.to_str().unwrap()],
    );
    assert_eq!(
        std::fs::read_to_string(other.join("Source/a.cpp")).unwrap(),
        "int a = 1;"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("Source/b.cpp")).unwrap(),
        "int b;"
    );
}

#[test]
fn pyn_in_a_plain_git_repository_behaves_as_git_and_refuses_server_commands() {
    let server = Server::start();
    let repo = git_repo(&server, "plain");

    let status = server.ok(&repo, "-", &["status"]);
    assert_eq!(
        porcelain_paths(&status),
        git(&repo, &["status", "--porcelain"])
            .lines()
            .collect::<Vec<_>>()
    );
    let log = server.ok(&repo, "-", &["log"]);
    assert!(log.contains("first") && log.contains("second"), "{log}");

    let before = snapshot(&repo.join(".git"));
    server.ok(&repo, "-", &["diff"]);
    for args in [
        vec!["lock", "README.md"],
        vec!["unlock", "README.md"],
        vec!["locks"],
        vec!["policy", "preview", "x.toml"],
        vec!["audit"],
        vec!["verify"],
        vec!["summary"],
    ] {
        let (code, err) = server.fails(&repo, "-", &args);
        assert_eq!(code, 128, "{args:?}: {err}");
        assert!(
            err.contains(&format!(
                "`pyn {}` needs a pyn server, and this repository is a git repository; `pyn convert` moves it to a pyn server",
                args[0]
            )),
            "{args:?}: {err}"
        );
    }
    assert_eq!(
        snapshot(&repo.join(".git")),
        before,
        "reads and refusals leave .git alone"
    );

    std::fs::write(repo.join("README.md"), "hello\nmore\n").unwrap();
    server.ok(&repo, "-", &["add", "README.md"]);
    server.ok(&repo, "-", &["commit", "-m", "third"]);
    assert_eq!(git(&repo, &["log", "--format=%s", "-1"]).trim(), "third");
    assert_eq!(
        git(&repo, &["status", "--porcelain"]).trim(),
        "?? scratch.txt"
    );
    git(&repo, &["fsck", "--no-progress"]);
}

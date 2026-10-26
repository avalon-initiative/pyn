//! End to end, a first day with pyn: a git project is cloned, worked on and pushed to a git remote, moved onto a pyn
//! server, locked and checked in by two people, and moved back. Real git only builds the remote and is the oracle for
//! what the repositories hold afterwards.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use chrono::{Duration, Utc};
use common::real::{PASSWORD, Server};
use common::{Env, cell_rows, start_empty_with_clock};
use pyn_core::ManualClock;

const NEW: &str = "alice/game";
const MAP: &str = "Art/m.umap";

struct Day {
    env: Env,
    clock: Arc<ManualClock>,
    root: PathBuf,
}

impl Day {
    fn new() -> Self {
        let clock = Arc::new(ManualClock::new(Utc::now()));
        let env = start_empty_with_clock(clock.clone());
        let root = env.dir("day");
        Self { env, clock, root }
    }

    fn run(&self, cwd: &Path, user: &str, args: &[&str]) -> std::process::Output {
        let home = self.root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let vars = [
            ("HOME", home.to_str().unwrap()),
            ("GIT_AUTHOR_NAME", "Ann"),
            ("GIT_AUTHOR_EMAIL", "ann@example.com"),
            ("GIT_COMMITTER_NAME", "Ann"),
            ("GIT_COMMITTER_EMAIL", "ann@example.com"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ];
        self.env.run_with_vars(cwd, user, args, "UTC", &vars)
    }

    fn ok(&self, step: &str, cwd: &Path, user: &str, args: &[&str]) -> String {
        let out = self.run(cwd, user, args);
        assert!(
            out.status.success(),
            "step '{step}': pyn {args:?} failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn refused(&self, step: &str, cwd: &Path, user: &str, args: &[&str]) -> (i32, String) {
        let out = self.run(cwd, user, args);
        assert!(
            !out.status.success(),
            "step '{step}': pyn {args:?} should have been refused"
        );
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// A bare git remote on `main` with a readme and an `Art/` file, built by git.
    fn remote(&self) -> PathBuf {
        let seed = self.root.join("seed");
        std::fs::create_dir_all(seed.join("Art")).unwrap();
        gitsrv::init(&seed, "sha1", false);
        gitsrv::commit(&seed, "README.md", b"hello\n", "first");
        gitsrv::commit(&seed, MAP, b"map one\n", "add the map");
        let bare = self.root.join("origin.git");
        gitsrv::init(&bare, "sha1", true);
        let to = bare.to_str().unwrap();
        gitsrv::run(&seed, &["push", "-q", to, "HEAD:refs/heads/main"]);
        gitsrv::run(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        bare
    }

    fn clone_of(&self, bare: &Path) -> PathBuf {
        let ws = self.root.join("work");
        let url = format!("file://{}", bare.display());
        self.ok(
            "clone the git remote",
            &self.root,
            "alice",
            &["clone", &url, ws.to_str().unwrap()],
        );
        ws
    }

    /// Writes `text` to `path`, stages it and commits it.
    fn commit_file(&self, ws: &Path, path: &str, text: &str, message: &str) {
        write(ws, path, text);
        self.ok(&format!("add {path}"), ws, "alice", &["add", path]);
        self.ok(
            &format!("commit '{message}'"),
            ws,
            "alice",
            &["commit", "-m", message],
        );
    }
}

fn write(ws: &Path, path: &str, text: &str) {
    let file = ws.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    if file.exists() {
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
    }
    std::fs::write(file, text).unwrap();
}

fn read_only(ws: &Path, path: &str) -> bool {
    std::fs::metadata(ws.join(path))
        .unwrap()
        .permissions()
        .readonly()
}

fn subjects(repo: &Path, rev: &str) -> Vec<String> {
    gitsrv::run(repo, &["log", "--format=%s", rev])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_git_project_is_worked_on_and_pushed_to_its_git_remote() {
    let day = Day::new();
    let bare = day.remote();
    let ws = day.clone_of(&bare);
    let alice = "alice";

    day.commit_file(&ws, "README.md", "hello\nworld\n", "extend the readme");
    day.ok("branch feature", &ws, alice, &["branch", "feature"]);
    day.ok("switch to feature", &ws, alice, &["switch", "feature"]);
    day.commit_file(&ws, "src/feature.txt", "feature\n", "feature work");
    day.ok("switch back to main", &ws, alice, &["switch", "main"]);
    day.commit_file(&ws, "docs.txt", "docs\n", "write the docs");
    let merged = day.ok(
        "merge feature",
        &ws,
        alice,
        &["merge", "-m", "merge feature", "feature"],
    );
    assert!(merged.contains("Merge made"), "merge output: {merged}");

    day.ok("branch topic", &ws, alice, &["branch", "topic", "HEAD~2"]);
    day.ok("switch to topic", &ws, alice, &["switch", "topic"]);
    day.commit_file(&ws, "topic.txt", "topic\n", "topic work");
    day.ok("rebase topic", &ws, alice, &["rebase", "main"]);
    day.ok("switch to main", &ws, alice, &["switch", "main"]);
    day.ok(
        "push main and topic",
        &ws,
        alice,
        &["push", "origin", "main", "topic"],
    );

    // git is the oracle: the remote holds what the work tree holds, and is sound
    gitsrv::fsck(&bare);
    let remote = gitsrv::oracle_refs(&bare, &[]);
    for branch in ["main", "topic"] {
        let local = gitsrv::run(&ws, &["rev-parse", branch]);
        assert_eq!(
            remote[&format!("refs/heads/{branch}")],
            local.trim(),
            "the remote's {branch} is not the work tree's"
        );
    }
    assert!(
        !remote.contains_key("refs/heads/feature"),
        "feature was never pushed"
    );
    assert_eq!(
        gitsrv::run(&bare, &["rev-list", "--merges", "--count", "main"]).trim(),
        "1",
        "one merge commit on main"
    );
    assert_eq!(
        gitsrv::run(&bare, &["rev-parse", "topic~1"]),
        gitsrv::run(&bare, &["rev-parse", "main"]),
        "topic was rebased onto main"
    );
    assert_eq!(
        subjects(&bare, "main"),
        [
            "merge feature",
            "write the docs",
            "feature work",
            "extend the readme",
            "add the map",
            "first"
        ]
    );
    assert_eq!(
        gitsrv::run(&bare, &["show", "main:README.md"]),
        "hello\nworld\n"
    );
    assert_eq!(
        gitsrv::run(&ws, &["status", "--porcelain"]),
        "",
        "the work tree is clean after the push"
    );
}

#[test]
fn a_git_project_moves_to_a_pyn_server_is_locked_and_returns_to_git() {
    let day = Day::new();
    let bare = day.remote();
    let ws = day.clone_of(&bare);
    let (alice, bob) = ("alice", "bob");
    day.commit_file(&ws, "src/lib.txt", "lib\n", "add the library");
    day.ok("push to git", &ws, alice, &["push", "origin", "main"]);

    let moved = day.ok(
        "convert to a pyn server",
        &ws,
        alice,
        &[
            "--repo",
            NEW,
            "convert",
            "--home",
            "pyn",
            "--exclusive",
            "Art/",
            "--yes",
        ],
    );
    assert!(moved.contains("converted"), "convert output: {moved}");
    assert!(
        read_only(&ws, MAP),
        "an exclusive file is read-only until locked"
    );
    let status = day.ok("status on the server home", &ws, alice, &["status"]);
    assert!(status.contains("On branch main"), "status: {status}");

    // one person holds the lock, the other is refused
    day.ok("alice locks the map", &ws, alice, &["lock", MAP]);
    assert!(!read_only(&ws, MAP), "the lock makes the map writable");
    let (code, err) = day.refused("bob locks the held map", &ws, bob, &["lock", MAP]);
    assert_eq!(code, 128, "{err}");
    assert!(
        err.contains("lock_held (409)") && err.contains("locked by alice"),
        "bob's refusal: {err}"
    );
    let (_, err) = day.refused(
        "bob commits to the held map",
        &ws,
        bob,
        &["commit", MAP, "--file", "README.md", "-m", "sneaky"],
    );
    assert!(
        err.contains("locked by alice"),
        "bob's commit refusal: {err}"
    );

    // check in releases the lock and locks the file down again
    write(&ws, MAP, "map two\n");
    let out = day.ok(
        "alice checks in",
        &ws,
        alice,
        &["commit", MAP, "-m", "second map"],
    );
    assert!(out.contains("revision"), "commit output: {out}");
    assert!(read_only(&ws, MAP), "check in locks the file down again");
    assert!(
        !day.ok("locks after check in", &ws, alice, &["locks"])
            .contains(MAP),
        "the lock is released by check in"
    );

    // a lapsed lease frees the file for someone else
    day.ok("alice locks again", &ws, alice, &["lock", MAP]);
    day.clock.advance(Duration::hours(9));
    day.ok("bob takes the lapsed lease", &ws, bob, &["lock", MAP]);
    write(&ws, MAP, "alice after the lapse\n");
    let (code, err) = day.refused(
        "alice checks in on a lapsed lease",
        &ws,
        alice,
        &["commit", MAP, "-m", "late"],
    );
    assert_eq!(code, 128, "{err}");
    assert!(err.contains("locked by bob"), "alice's refusal: {err}");
    day.ok("bob lets go", &ws, bob, &["unlock", MAP]);
    write(&ws, MAP, "map two\n");

    // history and restore
    let log = day.ok("log of the map", &ws, alice, &["log", MAP]);
    let revisions = cell_rows(&log);
    assert_eq!(revisions[0][0], "REV", "{log}");
    assert!(
        log.contains("second map") && log.contains("Convert to exclusive"),
        "history of the map: {log}"
    );
    day.ok("alice locks to restore", &ws, alice, &["lock", MAP]);
    let restored = day.ok(
        "restore the first map",
        &ws,
        alice,
        &["restore", MAP, "1", "--yes"],
    );
    assert!(
        restored.contains("restored from r1"),
        "restore output: {restored}"
    );
    assert_eq!(
        day.ok("read the head", &ws, alice, &["get", MAP]),
        "map one\n",
        "the restore is the new head"
    );
    day.ok("update after the restore", &ws, alice, &["update"]);
    assert_eq!(
        std::fs::read_to_string(ws.join(MAP)).unwrap(),
        "map one\n",
        "update brings the restored bytes into the work tree"
    );
    let log = day.ok("log after the restore", &ws, alice, &["log", MAP]);
    assert!(
        log.contains("second map"),
        "the revision the restore replaced stays in history: {log}"
    );

    // policy change through the preview
    let shared = "[meta]\ndefault = \"shared\"\n[shared]\npaths = [\".pyn/pyn.toml\"]\n";
    write(&ws, "shared.toml", shared);
    let preview = day.ok(
        "preview the policy",
        &ws,
        alice,
        &["policy", "preview", "shared.toml"],
    );
    assert!(
        preview.contains("exclusive -> shared") && preview.contains(MAP),
        "preview: {preview}"
    );
    day.ok(
        "lock the policy file",
        &ws,
        alice,
        &["lock", ".pyn/pyn.toml"],
    );
    write(&ws, ".pyn/pyn.toml", shared);
    day.ok(
        "commit the policy",
        &ws,
        alice,
        &["commit", ".pyn/pyn.toml", "-m", "Art is shared"],
    );
    write(&ws, MAP, "edited without a lock\n");
    day.ok(
        "commit a now shared file without a lock",
        &ws,
        bob,
        &["commit", MAP, "-m", "no lock needed"],
    );
    std::fs::remove_file(ws.join("shared.toml")).unwrap();

    // and back to a git home, which real git must find sound
    std::fs::rename(ws.join(".git"), day.root.join("old.git")).unwrap();
    day.ok(
        "convert back to a git home",
        &ws,
        alice,
        &["convert", "--home", "git", "--yes"],
    );
    gitsrv::run(&ws, &["fsck", "--strict"]);
    let back = subjects(&ws, "main");
    for want in [
        "first",
        "add the map",
        "add the library",
        "Initial policy",
        "Convert to exclusive: policy r1",
        "second map",
        "Art is shared",
        "no lock needed",
    ] {
        assert!(
            back.iter().any(|s| s == want),
            "'{want}' is missing from the git history: {back:?}"
        );
    }
    assert_eq!(
        gitsrv::run(&ws, &["show", &format!("main:{MAP}")]),
        "edited without a lock\n"
    );
    assert_eq!(gitsrv::run(&ws, &["status", "--porcelain"]), "");
}

/// The delivery worker refuses loopback receivers by design; this checks the refusal is visible and nothing is sent.
#[test]
fn a_webhook_to_a_loopback_receiver_is_refused_and_shows_in_the_delivery_list() {
    let server = Server::with_game_env(&[("PYN_WEBHOOKS_ALLOW_HTTP", "true")]);
    let receiver = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    receiver.set_nonblocking(true).unwrap();
    let url = format!("http://{}/hook", receiver.local_addr().unwrap());
    let cwd = server.dir("admin");
    let password = format!("{PASSWORD}\n");
    let login = server.run_with(
        &cwd,
        "root",
        &["login", "root", "--password-stdin"],
        Some(&password),
    );
    assert!(login.status.success(), "step 'root logs in'");

    server.ok(
        &cwd,
        "root",
        &[
            "admin",
            "webhook",
            "create",
            "ci",
            "--url",
            &url,
            "--event",
            "repository.created",
        ],
    );
    server.ok(&cwd, "alice", &["repo", "create", "second"]);

    let deadline = Instant::now() + StdDuration::from_secs(30);
    let row = loop {
        let listed = cell_rows(&server.ok(&cwd, "root", &["admin", "webhook", "deliveries", "1"]));
        if let Some(row) = listed.get(1).filter(|r| r[2] != "0") {
            break row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "step 'wait for an attempt': the repository.created event was never attempted: {listed:?}"
        );
        std::thread::sleep(StdDuration::from_millis(500));
    };
    assert_eq!(
        row[1], "pending",
        "a refused delivery is retried later: {row:?}"
    );
    assert!(
        row.join(" ").contains("addresses webhooks may not use"),
        "the refusal reason is shown: {row:?}"
    );
    assert!(
        receiver.accept().is_err(),
        "the loopback receiver must never be contacted"
    );
}

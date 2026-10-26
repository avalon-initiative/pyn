//! `fetch`, `pull` and `push` against a real server with `local_store` on. Real git reads the store as an oracle for
//! semantics only.

mod common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{Env, REPO, start};

fn clone_with(env: &Env, name: &str, extra: &[&str]) -> PathBuf {
    let ws = env.dir(name);
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn store(ws: &Path) -> PathBuf {
    ws.join(".pyn/local_only/store")
}

fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(store(ws))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn seeded() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one\ntwo\nthree\n", None);
    env.seed("alice", "Source/b.cpp", "bee", None);
    env.seed("alice", "Content/m.umap", "map", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    (env, ws)
}

fn commit_local(env: &Env, ws: &Path, user: &str, path: &str, text: &str, message: &str) {
    if let Some(dir) = Path::new(path).parent() {
        std::fs::create_dir_all(ws.join(dir)).unwrap();
    }
    let target = ws.join(path);
    if target.exists() {
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
    std::fs::write(&target, text).unwrap();
    env.ok(ws, user, &["add", path]);
    env.ok(ws, user, &["commit", "-m", message]);
}

fn server_commits(env: &Env, ws: &Path) -> usize {
    let listed = env.ok(ws, "alice", &["log", "--commits"]).lines().count();
    let local = git(ws, &["rev-list", "--count", "origin/main..main"]);
    listed - local.trim().parse::<usize>().unwrap()
}

fn server_head(env: &Env, ws: &Path) -> String {
    let log = env.ok(ws, "alice", &["log", "--commits", "--limit", "1"]);
    log.lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn a_shared_path_commit_is_pushed_with_the_id_it_has_locally() {
    let (env, ws) = seeded();
    let before = server_commits(&env, &ws);
    commit_local(
        &env,
        &ws,
        "alice",
        "Source/a.cpp",
        "one\nTWO\nthree\n",
        "edit",
    );
    commit_local(&env, &ws, "alice", "Source/new.cpp", "fresh", "add");
    let local = git(&ws, &["rev-parse", "main"]);
    let out = env.ok(&ws, "alice", &["push"]);
    assert!(out.contains("main -> main"), "{out}");

    assert_eq!(server_commits(&env, &ws), before + 2);
    assert_eq!(server_head(&env, &ws), &local[..10]);
    assert_eq!(git(&ws, &["rev-parse", "origin/main"]), local);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(!status.contains("local commit"), "{status}");
    assert_eq!(env.ok(&ws, "alice", &["push"]), "Everything up-to-date\n");

    let other = clone_with(&env, "other", &[]);
    assert_eq!(
        std::fs::read_to_string(other.join("Source/a.cpp")).unwrap(),
        "one\nTWO\nthree\n"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("Source/new.cpp")).unwrap(),
        "fresh"
    );
    env.ok(&ws, "alice", &["update"]);
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn an_exclusive_path_needs_a_live_lock_when_pushed() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(&env, &ws, "alice", "Content/m.umap", "map2", "map");
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(
        err.contains("lock required") && err.contains("pyn lock"),
        "{err}"
    );
    assert!(err.contains("nothing was pushed"), "{err}");
    assert_eq!(
        git(&ws, &["rev-parse", "origin/main"]),
        git(&ws, &["rev-parse", "main~1"]),
        "the refused push moved nothing"
    );

    let other = clone_with(&env, "other", &[]);
    env.ok(&other, "bob", &["lock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("locked by someone else"), "{err}");
    env.ok(&other, "bob", &["unlock", "Content/m.umap"]);

    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(
        std::fs::read_to_string(clone_with(&env, "third", &[]).join("Content/m.umap")).unwrap(),
        "map2"
    );
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(!status.contains("local commit"), "{status}");
}

#[test]
fn a_lapsed_lease_is_refused_like_no_lock() {
    use pyn_core::{MetadataStore, RepoId, RepoPath, UserId};
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(&env, &ws, "alice", "Content/m.umap", "map2", "map");
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let now = chrono::Utc::now();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(env.meta.acquire_lock(
            &RepoId::new("t"),
            &RepoPath::new("Content/m.umap").unwrap(),
            &UserId::new("alice"),
            now - chrono::Duration::hours(3),
            now - chrono::Duration::hours(1),
            10,
        ))
        .unwrap();
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("lock required"), "{err}");
}

#[test]
fn a_non_fast_forward_is_replayed_by_pull_rebase_and_pushed() {
    let (env, ws) = seeded();
    let other = clone_with(&env, "other", &["--local-store"]);
    commit_local(
        &env,
        &other,
        "alice",
        "Source/a.cpp",
        "ONE\ntwo\nthree\n",
        "first line",
    );
    env.ok(&other, "alice", &["push"]);

    commit_local(
        &env,
        &ws,
        "alice",
        "Source/a.cpp",
        "one\ntwo\nTHREE\n",
        "last line",
    );
    commit_local(&env, &ws, "alice", "Source/b.cpp", "bee2", "b");
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(
        err.contains("non-fast-forward") && err.contains("pyn pull"),
        "{err}"
    );

    let out = env.ok(&ws, "alice", &["pull", "--rebase"]);
    assert!(out.contains("Rebased 2 local commit(s)"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "ONE\ntwo\nTHREE\n"
    );
    assert_eq!(
        git(&ws, &["rev-list", "--count", "origin/main..main"]),
        "2\n"
    );
    assert_eq!(
        git(&ws, &["cat-file", "blob", "main:Source/a.cpp"]),
        "ONE\ntwo\nTHREE\n"
    );
    git(&ws, &["fsck", "--strict"]);

    env.ok(&ws, "alice", &["push"]);
    assert_eq!(env.ok(&ws, "alice", &["verify"]), "");
    assert_eq!(
        git(&ws, &["rev-parse", "main"]),
        git(&ws, &["rev-parse", "origin/main"])
    );
    let third = clone_with(&env, "third", &[]);
    assert_eq!(
        std::fs::read_to_string(third.join("Source/a.cpp")).unwrap(),
        "ONE\ntwo\nTHREE\n"
    );
    assert_eq!(
        std::fs::read_to_string(third.join("Source/b.cpp")).unwrap(),
        "bee2"
    );
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(!status.contains("local commit"), "{status}");
}

#[test]
fn fetch_stores_another_clients_commits_and_pull_fast_forwards() {
    let (env, ws) = seeded();
    let tip = git(&ws, &["rev-parse", "main"]);
    let other = clone_with(&env, "other", &["--local-store"]);
    commit_local(&env, &other, "alice", "Source/c.cpp", "sea", "c");
    env.ok(&other, "alice", &["push"]);
    let theirs = git(&other, &["rev-parse", "main"]);

    let out = env.ok(&ws, "alice", &["fetch"]);
    assert!(out.contains("origin/main"), "{out}");
    assert_eq!(git(&ws, &["rev-parse", "origin/main"]), theirs);
    assert_eq!(
        git(&ws, &["rev-parse", "main"]),
        tip,
        "fetch leaves the local branch"
    );
    assert!(!ws.join("Source/c.cpp").exists(), "fetch leaves the files");
    assert_eq!(git(&ws, &["cat-file", "-t", theirs.trim()]), "commit\n");
    assert_eq!(
        env.ok(&ws, "alice", &["fetch"]),
        "",
        "nothing new prints nothing"
    );

    let out = env.ok(&ws, "alice", &["pull"]);
    assert!(out.contains("Fast-forward"), "{out}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), theirs);
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/c.cpp")).unwrap(),
        "sea"
    );
    assert!(
        env.ok(&ws, "alice", &["pull"])
            .contains("Already up to date.")
    );
}

#[test]
fn a_pull_will_not_overwrite_an_uncommitted_change() {
    let (env, ws) = seeded();
    let other = clone_with(&env, "other", &["--local-store"]);
    commit_local(&env, &other, "alice", "Source/b.cpp", "theirs", "t");
    env.ok(&other, "alice", &["push"]);
    std::fs::write(ws.join("Source/b.cpp"), "dirty").unwrap();
    let err = env.fails(&ws, "alice", &["pull"]);
    assert!(
        err.contains("would be overwritten") && err.contains("Source/b.cpp"),
        "{err}"
    );
}

fn delete_local(env: &Env, ws: &Path, user: &str, path: &str, message: &str) {
    let target = ws.join(path);
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
    std::fs::remove_file(&target).unwrap();
    env.ok(ws, user, &["add", path]);
    env.ok(ws, user, &["commit", "-m", message]);
}

#[test]
fn a_local_deletion_of_a_shared_path_is_pushed() {
    let (env, ws) = seeded();
    let before = server_commits(&env, &ws);
    delete_local(&env, &ws, "alice", "Source/b.cpp", "drop");
    let local = git(&ws, &["rev-parse", "main"]);
    env.ok(&ws, "alice", &["push"]);

    assert_eq!(server_commits(&env, &ws), before + 1);
    assert_eq!(server_head(&env, &ws), &local[..10]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(!status.contains("local commit"), "{status}");
    assert!(!ws.join("Source/b.cpp").exists());
    let other = clone_with(&env, "other", &[]);
    assert!(!other.join("Source/b.cpp").exists());
    assert!(other.join("Source/a.cpp").exists());
    assert_eq!(env.ok(&ws, "alice", &["push"]), "Everything up-to-date\n");
}

#[test]
fn a_local_deletion_of_an_exclusive_path_needs_a_live_lock() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    delete_local(&env, &ws, "alice", "Content/m.umap", "drop the map");
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let before = server_commits(&env, &ws);

    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("lock required"), "{err}");
    assert!(err.contains("nothing was pushed"), "{err}");
    assert_eq!(server_commits(&env, &ws), before);

    let other = clone_with(&env, "other", &[]);
    env.ok(&other, "bob", &["lock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("locked by someone else"), "{err}");
    env.ok(&other, "bob", &["unlock", "Content/m.umap"]);

    env.ok(&ws, "alice", &["lock", "Content/m.umap", "--base", "1"]);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(server_commits(&env, &ws), before + 1);
    assert!(
        !clone_with(&env, "third", &[])
            .join("Content/m.umap")
            .exists()
    );
}

const POLICY: &str =
    "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\", \"Source/b.cpp\"]\n";

#[test]
fn a_valid_policy_commit_is_pushed_and_then_enforced() {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "alice", ".pyn/pyn.toml", POLICY, "policy");
    let out = env.ok(&ws, "alice", &["push"]);
    assert!(out.contains("main -> main"), "{out}");
    assert!(!env.ok(&ws, "alice", &["status"]).contains("local commit"));

    // Source/b.cpp is exclusive now: an edit pushed without the lock is refused.
    env.ok(&ws, "alice", &["update"]);
    env.ok(&ws, "alice", &["lock", "Source/b.cpp"]);
    commit_local(&env, &ws, "alice", "Source/b.cpp", "bee2", "edit b");
    env.ok(&ws, "alice", &["unlock", "Source/b.cpp"]);
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("lock required"), "{err}");
}

#[test]
fn a_push_with_a_policy_commit_prints_the_versions_it_strands() {
    let (env, ws) = seeded();
    let other = clone_with(&env, "other", &["--local-store"]);
    env.server_branch(&other, "bob", "feature");
    commit_local(
        &env,
        &other,
        "bob",
        "Source/b.cpp",
        "feature bee",
        "feature",
    );
    env.ok(&other, "bob", &["push"]);

    commit_local(&env, &ws, "alice", ".pyn/pyn.toml", POLICY, "policy");
    let out = env.ok(&ws, "alice", &["push"]);
    assert!(
        out.contains("feature") && out.contains("Source/b.cpp"),
        "{out}"
    );
    assert!(out.contains("can no longer merge"), "{out}");
}

#[test]
fn a_push_that_makes_a_path_shared_reports_the_lock_it_released() {
    let (env, ws) = seeded();
    let other = clone_with(&env, "other", &[]);
    env.ok(&other, "bob", &["lock", "Content/m.umap"]);
    commit_local(
        &env,
        &ws,
        "alice",
        ".pyn/pyn.toml",
        "[meta]\ndefault = \"shared\"\n",
        "all shared",
    );
    let out = env.ok(&ws, "alice", &["push"]);
    assert!(
        out.contains("released bob's lock on Content/m.umap"),
        "{out}"
    );
    assert!(env.ok(&ws, "alice", &["locks"]).contains("no locks"));
}

#[test]
fn an_invalid_policy_is_refused_whole_with_nothing_pushed() {
    let (env, ws) = seeded();
    let before = server_commits(&env, &ws);
    commit_local(&env, &ws, "alice", "Source/new.cpp", "fresh", "add");
    commit_local(
        &env,
        &ws,
        "alice",
        ".pyn/pyn.toml",
        "this is not [valid toml",
        "bad policy",
    );
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(
        err.contains("[rejected]") && err.contains("invalid policy"),
        "{err}"
    );
    assert!(err.contains("nothing was pushed"), "{err}");
    assert!(err.contains("pyn reset"), "{err}");
    assert_eq!(server_commits(&env, &ws), before);
    assert_eq!(
        git(&ws, &["rev-parse", "origin/main"]),
        git(&ws, &["rev-parse", "main~2"]),
    );

    // The way out: drop the bad commit, keep the file to fix, commit and push.
    let out = env.ok(&ws, "alice", &["reset", "HEAD~1"]);
    assert!(out.contains("1 local commit(s)"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join(".pyn/pyn.toml")).unwrap(),
        "this is not [valid toml"
    );
    assert!(
        env.ok(&ws, "alice", &["status"]).contains("pyn.toml"),
        "the change is back in the work tree"
    );
    commit_local(&env, &ws, "alice", ".pyn/pyn.toml", POLICY, "good policy");
    env.ok(&ws, "alice", &["push"]);
    // The add and the good policy, plus the server's own commit converting the policy's paths.
    assert_eq!(server_commits(&env, &ws), before + 3);
}

#[test]
fn reset_soft_keeps_the_changes_staged_and_hard_restores_the_files() {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "alice", "Source/a.cpp", "edited\n", "edit");
    commit_local(&env, &ws, "alice", "Source/new.cpp", "fresh", "add");
    env.ok(&ws, "alice", &["reset", "--soft", "HEAD~2"]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("Source/a.cpp") && status.contains("Source/new.cpp"),
        "{status}"
    );
    assert!(!status.contains("local commit"), "{status}");
    env.ok(&ws, "alice", &["commit", "-m", "again"]);
    assert_eq!(
        git(&ws, &["rev-list", "--count", "origin/main..main"]),
        "1\n"
    );

    let err = env.fails(&ws, "alice", &["reset", "--hard", "HEAD~1"]);
    assert!(
        err.contains("no terminal") && err.contains("--yes") && err.contains("Nothing was changed"),
        "{err}"
    );
    assert!(ws.join("Source/new.cpp").exists());
    env.ok(&ws, "alice", &["reset", "--hard", "--yes", "HEAD~1"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert!(!ws.join("Source/new.cpp").exists());
    assert_eq!(
        git(&ws, &["rev-parse", "main"]),
        git(&ws, &["rev-parse", "origin/main"])
    );
    let err = env.fails(&ws, "alice", &["reset", "HEAD~1"]);
    assert!(err.contains("published") || err.contains("before"), "{err}");
}

#[test]
fn a_policy_commit_from_someone_without_the_policy_permission_is_refused() {
    let env = common::start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["repo", "create", "game"]);
    env.ok(
        &home,
        "alice",
        &["member", "set", "bob", "writer", "--repo", "alice/game"],
    );
    let token = env.ok(
        &home,
        "bob",
        &[
            "token",
            "create",
            "narrow",
            "--permissions",
            "read,lock,checkin",
            "--repos",
            "alice/game",
        ],
    );
    let token = token.trim();
    let run = |ws: &Path, args: &[&str]| env.run_with_token(ws, token, args);
    let source = env.source("alice/game");
    let ws = env.dir("bobws");
    let cloned = run(
        &home,
        &[
            "clone",
            "--local-store",
            source.as_str(),
            ws.to_str().unwrap(),
        ],
    );
    assert!(cloned.status.success(), "{cloned:?}");
    let ws = ws.canonicalize().unwrap();
    let out = run(&ws, &["lock", ".pyn/pyn.toml"]);
    assert!(out.status.success(), "{out:?}");
    let target = ws.join(".pyn/pyn.toml");
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
    std::fs::write(&target, POLICY).unwrap();
    for args in [vec!["add", ".pyn/pyn.toml"], vec!["commit", "-m", "policy"]] {
        let out = run(&ws, &args);
        assert!(out.status.success(), "{args:?}: {out:?}");
    }
    let count = |ws: &Path| {
        String::from_utf8_lossy(&run(ws, &["log", "--commits"]).stdout)
            .lines()
            .count()
    };
    let before = count(&ws);
    let pushed = run(&ws, &["push"]);
    assert!(!pushed.status.success());
    let err = String::from_utf8_lossy(&pushed.stderr);
    assert!(
        err.contains("[rejected]") && err.contains("forbidden"),
        "{err}"
    );
    assert!(err.contains("nothing was pushed"), "{err}");
    assert_eq!(count(&ws), before, "nothing reached the server");
}

#[test]
fn when_opted_out_fetch_pull_and_push_are_as_before() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &["--no-local-store"]);
    env.seed("alice", "Source/b.cpp", "bee", None);
    assert_eq!(env.ok(&ws, "alice", &["fetch"]), "updated 1 file(s)\n");
    env.seed("alice", "Source/c.cpp", "sea", None);
    assert_eq!(env.ok(&ws, "alice", &["pull"]), "updated 1 file(s)\n");
    let err = env.fails(&ws, "alice", &["pull", "--rebase"]);
    assert!(err.contains("takes no arguments"), "{err}");
    assert_eq!(
        env.ok(&ws, "alice", &["push"]),
        "Nothing to push: commits go straight to the server, so `pyn commit` has already published them.\n"
    );
    assert!(!store(&ws).exists());
}

/// A proxy to the real server that can alter the answer of the fetch route.
struct Proxy {
    url: String,
    tamper: Arc<AtomicBool>,
}

fn proxy(upstream: &str) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let tamper = Arc::new(AtomicBool::new(false));
    let (flag, upstream) = (tamper.clone(), upstream.to_string());
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (flag, upstream) = (flag.clone(), upstream.clone());
            std::thread::spawn(move || relay(conn, &upstream, &flag));
        }
    });
    Proxy { url, tamper }
}

fn relay(mut conn: TcpStream, upstream: &str, tamper: &AtomicBool) {
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    let (head_end, length) = loop {
        let n = conn.read(&mut buf).unwrap();
        raw.extend_from_slice(&buf[..n]);
        if let Some(at) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..at]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse::<usize>().unwrap());
            break (at + 4, length);
        }
    };
    while raw.len() < head_end + length {
        let n = conn.read(&mut buf).unwrap();
        raw.extend_from_slice(&buf[..n]);
    }
    let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap().split(' ');
    let (method, path) = (first.next().unwrap(), first.next().unwrap());
    let client = reqwest::blocking::Client::new();
    let mut req = client.request(
        reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
        format!("{upstream}{path}"),
    );
    for line in lines.filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once(": ").unwrap();
        if !["host", "content-length", "connection"].contains(&k.to_ascii_lowercase().as_str()) {
            req = req.header(k, v);
        }
    }
    let resp = req.body(raw[head_end..].to_vec()).send().unwrap();
    let status = resp.status().as_u16();
    let mut body = resp.bytes().unwrap().to_vec();
    if path.ends_with("/fetch") && tamper.load(Ordering::SeqCst) {
        let mut json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        if let Some(tree) = json["commits"][0]["trees"][0]["data"].as_str() {
            use base64::Engine;
            let engine = base64::engine::general_purpose::STANDARD;
            let mut bytes = engine.decode(tree).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            json["commits"][0]["trees"][0]["data"] = engine.encode(bytes).into();
        }
        body = serde_json::to_vec(&json).unwrap();
    }
    let _ = write!(
        conn,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(&body);
}

#[test]
fn a_tampered_object_is_refused_and_nothing_is_stored() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let proxy = proxy(&env.url);
    let ws = env.dir("ws");
    let config = env.dir("proxy-config");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&ws)
            .env("PYN_SERVER", &proxy.url)
            .env("PYN_USER", "alice")
            .env("PYN_CONFIG_DIR", &config)
            .env("TZ", "UTC")
            .env_remove("PYN_TOKEN")
            .env_remove("PYN_DIR")
            .env("PYN_REPO", REPO)
            .output()
            .unwrap()
    };
    let source = format!("{}/{REPO}", proxy.url);
    let cloned = run(&["clone", "--local-store", &source, "."]);
    assert!(
        cloned.status.success(),
        "{}",
        String::from_utf8_lossy(&cloned.stderr)
    );
    let tracking = git(&ws, &["rev-parse", "origin/main"]);

    env.seed("alice", "Source/b.cpp", "bee", None);
    proxy.tamper.store(true, Ordering::SeqCst);
    let out = run(&["fetch"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("does not match its content"), "{err}");
    assert_eq!(git(&ws, &["rev-parse", "origin/main"]), tracking);

    proxy.tamper.store(false, Ordering::SeqCst);
    let out = run(&["fetch"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_ne!(git(&ws, &["rev-parse", "origin/main"]), tracking);
}

fn parents(ws: &Path, rev: &str) -> Vec<String> {
    git(ws, &["rev-list", "--parents", "-n", "1", rev])
        .split_whitespace()
        .skip(1)
        .map(str::to_string)
        .collect()
}

fn diverge(env: &Env, ws: &Path) -> PathBuf {
    let other = clone_with(env, "other", &["--local-store"]);
    commit_local(
        env,
        &other,
        "alice",
        "Source/a.cpp",
        "ONE\ntwo\nthree\n",
        "first line",
    );
    env.ok(&other, "alice", &["push"]);
    commit_local(
        env,
        ws,
        "alice",
        "Source/a.cpp",
        "one\ntwo\nTHREE\n",
        "last line",
    );
    commit_local(env, ws, "alice", "Source/b.cpp", "bee2", "b");
    other
}

#[test]
fn a_non_fast_forward_pull_makes_a_merge_commit_and_push_sends_it() {
    let (env, ws) = seeded();
    diverge(&env, &ws);
    let local = git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("non-fast-forward"), "{err}");

    let out = env.ok(&ws, "alice", &["pull"]);
    assert!(out.contains("Merge made by the 'ort' strategy."), "{out}");
    let merge = git(&ws, &["rev-parse", "main"]);
    let theirs = git(&ws, &["rev-parse", "origin/main"]);
    assert_eq!(parents(&ws, "main"), [local.trim(), theirs.trim()]);
    assert!(git(&ws, &["log", "-1", "--format=%s", "main"]).starts_with("Merge branch 'main' of "),);
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "ONE\ntwo\nTHREE\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/b.cpp")).unwrap(),
        "bee2"
    );
    git(&ws, &["fsck", "--strict"]);
    // Git as the oracle for the result only.
    let oracle = git(
        &ws,
        &["merge-tree", "--write-tree", local.trim(), theirs.trim()],
    );
    assert_eq!(
        git(&ws, &["rev-parse", "main^{tree}"]),
        oracle.lines().next().unwrap().to_string() + "\n"
    );
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("local commit"), "{status}");

    let before = server_commits(&env, &ws);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(
        server_commits(&env, &ws),
        before + 3,
        "both local commits and the merge"
    );
    assert_eq!(server_head(&env, &ws), &merge.trim()[..10]);
    assert_eq!(git(&ws, &["rev-parse", "origin/main"]), merge);
    assert_eq!(env.ok(&ws, "alice", &["push"]), "Everything up-to-date\n");
    let third = clone_with(&env, "third", &[]);
    assert_eq!(
        std::fs::read_to_string(third.join("Source/a.cpp")).unwrap(),
        "ONE\ntwo\nTHREE\n"
    );
    assert_eq!(
        std::fs::read_to_string(third.join("Source/b.cpp")).unwrap(),
        "bee2"
    );
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(!status.contains("local commit"), "{status}");
}

#[test]
fn a_pulled_merge_adopts_an_exclusive_path_only_the_server_changed() {
    let (env, ws) = seeded();
    let other = clone_with(&env, "other", &["--local-store"]);
    env.ok(&other, "bob", &["lock", "Content/m.umap"]);
    commit_local(&env, &other, "bob", "Content/m.umap", "map by bob", "map");
    env.ok(&other, "bob", &["push"]);
    commit_local(&env, &ws, "alice", "Source/b.cpp", "bee2", "b");

    let out = env.ok(&ws, "alice", &["pull"]);
    assert!(out.contains("Merge made"), "{out}");
    assert_eq!(parents(&ws, "main").len(), 2);
    assert_eq!(
        std::fs::read_to_string(ws.join("Content/m.umap")).unwrap(),
        "map by bob"
    );
    env.ok(&ws, "alice", &["push"]);
    let third = clone_with(&env, "third", &[]);
    assert_eq!(
        std::fs::read_to_string(third.join("Content/m.umap")).unwrap(),
        "map by bob"
    );
    assert_eq!(
        std::fs::read_to_string(third.join("Source/b.cpp")).unwrap(),
        "bee2"
    );
    assert!(
        env.ok(&ws, "alice", &["locks"]).trim().is_empty()
            || !env.ok(&ws, "alice", &["locks"]).contains("m.umap")
    );
}

#[test]
fn a_diverged_exclusive_path_is_refused_until_the_person_holds_its_lock() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(
        &env,
        &ws,
        "alice",
        "Content/m.umap",
        "map by alice",
        "alice's map",
    );
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let other = clone_with(&env, "other", &["--local-store"]);
    env.ok(&other, "bob", &["lock", "Content/m.umap"]);
    commit_local(
        &env,
        &other,
        "bob",
        "Content/m.umap",
        "map by bob",
        "bob's map",
    );
    env.ok(&other, "bob", &["push"]);

    let local = git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["pull"]);
    assert!(
        err.contains("Content/m.umap") && err.contains("exclusive") && err.contains("pyn lock"),
        "{err}"
    );
    assert_eq!(git(&ws, &["rev-parse", "main"]), local);
    assert_eq!(
        std::fs::read_to_string(ws.join("Content/m.umap")).unwrap(),
        "map by alice"
    );
    env.ok(&ws, "alice", &["pull", "--abort"]);
    let err = env.fails(&ws, "alice", &["pull", "--rebase"]);
    assert!(err.contains("Content/m.umap"), "{err}");
    env.ok(&ws, "alice", &["pull", "--abort"]);

    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    let out = env.ok(&ws, "alice", &["pull"]);
    assert!(out.contains("Kept your version of Content/m.umap"), "{out}");
    assert_eq!(parents(&ws, "main").len(), 2);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(
        std::fs::read_to_string(clone_with(&env, "third", &[]).join("Content/m.umap")).unwrap(),
        "map by alice"
    );
}

#[test]
fn a_merged_push_still_needs_the_lock_for_the_local_exclusive_change() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(
        &env,
        &ws,
        "alice",
        "Content/m.umap",
        "map by alice",
        "alice's map",
    );
    let other = clone_with(&env, "other", &["--local-store"]);
    commit_local(&env, &other, "bob", "Source/b.cpp", "bee by bob", "b");
    env.ok(&other, "bob", &["push"]);
    env.ok(&ws, "alice", &["pull"]);
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("lock required"), "{err}");
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["push"]);
}

#[test]
fn a_held_lock_keeps_the_file_writable_through_a_local_commit_and_modes_stay_private() {
    use std::os::unix::fs::PermissionsExt;
    let (env, ws) = seeded();
    let mode = |p: &str| std::fs::metadata(ws.join(p)).unwrap().permissions().mode() & 0o777;
    assert_eq!(
        mode("Content/m.umap") & 0o222,
        0,
        "read-only before the lock"
    );
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    assert_eq!(
        mode("Content/m.umap") & 0o022,
        0,
        "not writable by group or others"
    );
    assert_ne!(mode("Content/m.umap") & 0o200, 0);
    commit_local(&env, &ws, "alice", "Content/m.umap", "map2", "map");
    assert_ne!(
        mode("Content/m.umap") & 0o200,
        0,
        "still writable under the lock"
    );
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    assert_eq!(mode("Content/m.umap") & 0o222, 0);
}

#[test]
fn committing_an_unchanged_exclusive_path_says_there_is_nothing_to_commit() {
    let (env, ws) = seeded();
    let err = env.fails(&ws, "alice", &["commit", "Content/m.umap", "-m", "x"]);
    assert!(err.contains("nothing to commit"), "{err}");
    assert!(!err.contains("lock_required"), "{err}");
}

#[test]
fn a_locally_added_file_is_not_shown_as_revision_zero() {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "alice", "Source/new.cpp", "fresh", "add");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("Source/new.cpp") && !status.contains("r0"),
        "{status}"
    );
}

#[test]
fn log_and_show_cover_unpublished_local_commits() {
    let (env, ws) = seeded();
    commit_local(
        &env,
        &ws,
        "alice",
        "Source/a.cpp",
        "one\nTWO\nthree\n",
        "local edit",
    );
    let local = git(&ws, &["rev-parse", "main"]);
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    let first = log.lines().nth(1).unwrap();
    assert!(
        first.starts_with(&local[..10]) && first.contains("local edit"),
        "{log}"
    );
    assert!(log.contains("seed"), "server history follows: {log}");

    let shown = env.ok(&ws, "alice", &["show"]);
    assert!(
        shown.contains(&local) && shown.contains("local edit"),
        "{shown}"
    );
    assert!(shown.contains("-two") && shown.contains("+TWO"), "{shown}");
    let by_hash = env.ok(&ws, "alice", &["show", &local[..10]]);
    assert_eq!(by_hash, shown);

    env.ok(&ws, "alice", &["push"]);
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    assert_eq!(log.matches("local edit").count(), 1, "{log}");
}

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

fn write(ws: &Path, path: &str, text: &str) {
    let target = ws.join(path);
    if target.exists() {
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
    std::fs::write(target, text).unwrap();
}

/// A local commit and a pushed commit from another clone that change the first line of `Source/a.cpp` differently,
/// and a clean change on each side.
fn clash(env: &Env, ws: &Path) {
    let other = clone_with(env, "other", &["--local-store"]);
    commit_local(
        env,
        &other,
        "bob",
        "Source/a.cpp",
        "theirs\ntwo\nthree\n",
        "t",
    );
    commit_local(env, &other, "bob", "Source/new.cpp", "fresh", "new");
    env.ok(&other, "bob", &["push"]);
    commit_local(env, ws, "alice", "Source/a.cpp", "mine\ntwo\nthree\n", "m");
    commit_local(env, ws, "alice", "Source/b.cpp", "bee2", "b");
}

#[test]
fn a_conflicted_pull_leaves_markers_and_is_concluded_with_add_and_commit() {
    let (env, ws) = seeded();
    clash(&env, &ws);
    let local = git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["pull"]);
    assert!(
        err.contains("CONFLICT (content): Merge conflict in Source/a.cpp")
            && err.contains("pyn add")
            && err.contains("pyn commit")
            && err.contains("pyn pull --abort"),
        "{err}"
    );
    assert_eq!(
        git(&ws, &["rev-parse", "main"]),
        local,
        "the branch has not moved"
    );
    assert_eq!(
        read(&ws, "Source/a.cpp"),
        "<<<<<<< HEAD\nmine\n||||||| base\none\n=======\ntheirs\n>>>>>>> origin/main\ntwo\nthree\n"
    );
    assert_eq!(
        read(&ws, "Source/new.cpp"),
        "fresh",
        "clean changes are in the tree"
    );
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");

    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("unmerged (both modified)") && status.contains("You have unmerged paths"),
        "{status}"
    );
    assert!(
        env.ok(&ws, "alice", &["status", "--short"])
            .contains("UU Source/a.cpp")
    );
    for command in [["pull"], ["fetch"], ["push"]] {
        let err = env.fails(&ws, "alice", &command);
        assert!(err.contains("not concluded"), "{err}");
    }
    let err = env.fails(&ws, "alice", &["commit"]);
    assert!(
        err.contains("unmerged") && err.contains("Source/a.cpp"),
        "{err}"
    );

    write(&ws, "Source/a.cpp", "mine and theirs\ntwo\nthree\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["commit"]);
    assert!(out.contains("Merge made"), "{out}");
    let (theirs, merge) = (
        git(&ws, &["rev-parse", "origin/main"]),
        git(&ws, &["rev-parse", "main"]),
    );
    assert_eq!(parents(&ws, "main"), [local.trim(), theirs.trim()]);
    assert!(git(&ws, &["log", "-1", "--format=%s", "main"]).starts_with("Merge branch 'main' of "));
    assert_eq!(read(&ws, "Source/a.cpp"), "mine and theirs\ntwo\nthree\n");
    git(&ws, &["fsck", "--strict"]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        !status.contains("unmerged") && !status.contains("You have"),
        "{status}"
    );

    env.ok(&ws, "alice", &["push"]);
    assert_eq!(git(&ws, &["rev-parse", "origin/main"]), merge);
    let third = clone_with(&env, "third", &[]);
    assert_eq!(
        read(&third, "Source/a.cpp"),
        "mine and theirs\ntwo\nthree\n"
    );
    assert_eq!(read(&third, "Source/new.cpp"), "fresh");
}

#[test]
fn aborting_a_conflicted_pull_puts_everything_back_and_loses_nothing() {
    let (env, ws) = seeded();
    clash(&env, &ws);
    write(&ws, "Source/untouched.txt", "kept");
    let local = git(&ws, &["rev-parse", "main"]);
    env.fails(&ws, "alice", &["pull"]);
    write(&ws, "Source/a.cpp", "half way\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);

    let out = env.ok(&ws, "alice", &["pull", "--abort"]);
    assert!(out.contains("given up"), "{out}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), local);
    assert_eq!(read(&ws, "Source/a.cpp"), "mine\ntwo\nthree\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");
    assert!(!ws.join("Source/new.cpp").exists());
    assert_eq!(read(&ws, "Source/untouched.txt"), "kept");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        !status.contains("unmerged") && !status.contains("You have"),
        "{status}"
    );
    assert!(status.contains("2 local commit(s)"), "{status}");
    assert!(
        env.fails(&ws, "alice", &["pull", "--abort"])
            .contains("no pull in progress")
    );

    let again = env.fails(&ws, "alice", &["pull"]);
    assert!(again.contains("CONFLICT"), "{again}");
    env.ok(&ws, "alice", &["merge", "--abort"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "mine\ntwo\nthree\n");
}

#[test]
fn a_side_can_be_taken_and_a_deleted_file_is_a_resolution() {
    let (env, ws) = seeded();
    clash(&env, &ws);
    env.fails(&ws, "alice", &["pull"]);
    env.ok(
        &ws,
        "alice",
        &["checkout", "--theirs", "--", "Source/a.cpp"],
    );
    assert_eq!(read(&ws, "Source/a.cpp"), "theirs\ntwo\nthree\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "take theirs"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "theirs\ntwo\nthree\n");
    assert!(git(&ws, &["log", "-1", "--format=%s", "main"]).starts_with("take theirs"));
    env.ok(&ws, "alice", &["push"]);

    let env2 = start();
    env2.seed("alice", "Source/a.cpp", "one\n", None);
    env2.seed("alice", "Source/b.cpp", "bee", None);
    let ws2 = clone_with(&env2, "ws", &["--local-store"]);
    let other = clone_with(&env2, "other", &["--local-store"]);
    delete_local(&env2, &other, "bob", "Source/b.cpp", "drop b");
    env2.ok(&other, "bob", &["push"]);
    commit_local(&env2, &ws2, "alice", "Source/b.cpp", "bee edited", "edit b");
    let err = env2.fails(&ws2, "alice", &["pull"]);
    assert!(
        err.contains("CONFLICT (modify/delete): Source/b.cpp"),
        "{err}"
    );
    assert_eq!(read(&ws2, "Source/b.cpp"), "bee edited");
    std::fs::remove_file(ws2.join("Source/b.cpp")).unwrap();
    env2.ok(&ws2, "alice", &["add", "Source/b.cpp"]);
    env2.ok(&ws2, "alice", &["commit"]);
    assert!(!ws2.join("Source/b.cpp").exists());
    env2.ok(&ws2, "alice", &["push"]);
    assert!(
        !clone_with(&env2, "third", &[])
            .join("Source/b.cpp")
            .exists()
    );
}

#[test]
fn an_exclusive_conflict_is_resolved_by_choosing_a_side_under_the_lock() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(
        &env,
        &ws,
        "alice",
        "Content/m.umap",
        "map by alice",
        "alice's map",
    );
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let other = clone_with(&env, "other", &["--local-store"]);
    env.ok(&other, "bob", &["lock", "Content/m.umap"]);
    commit_local(
        &env,
        &other,
        "bob",
        "Content/m.umap",
        "map by bob",
        "bob's map",
    );
    env.ok(&other, "bob", &["push"]);
    commit_local(&env, &ws, "alice", "Source/b.cpp", "bee2", "b");

    let err = env.fails(&ws, "alice", &["pull"]);
    assert!(
        err.contains("CONFLICT (exclusive): Content/m.umap") && err.contains("pyn lock"),
        "{err}"
    );
    assert_eq!(read(&ws, "Content/m.umap"), "map by alice");
    assert!(env.ok(&ws, "alice", &["status"]).contains("unmerged"));

    env.ok(
        &ws,
        "alice",
        &["checkout", "--theirs", "--", "Content/m.umap"],
    );
    assert_eq!(read(&ws, "Content/m.umap"), "map by bob");
    env.ok(&ws, "alice", &["add", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["commit"]);
    assert!(err.contains("pyn lock Content/m.umap"), "{err}");
    assert_eq!(
        parents(&ws, "main").len(),
        1,
        "nothing was committed without the lock"
    );

    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["commit"]);
    assert_eq!(parents(&ws, "main").len(), 2);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(
        read(&clone_with(&env, "third", &[]), "Content/m.umap"),
        "map by bob"
    );
}

fn rebase_clash(env: &Env, ws: &Path) {
    let other = clone_with(env, "other", &["--local-store"]);
    commit_local(
        env,
        &other,
        "bob",
        "Source/a.cpp",
        "theirs\ntwo\nthree\n",
        "t",
    );
    env.ok(&other, "bob", &["push"]);
    commit_local(env, ws, "alice", "Source/b.cpp", "bee2", "b first");
    commit_local(
        env,
        ws,
        "alice",
        "Source/a.cpp",
        "mine\ntwo\nthree\n",
        "a second",
    );
    commit_local(env, ws, "alice", "Source/c.cpp", "cee", "c third");
}

#[test]
fn a_conflicted_rebase_stops_and_is_continued_with_the_resolution() {
    let (env, ws) = seeded();
    rebase_clash(&env, &ws);
    let err = env.fails(&ws, "alice", &["pull", "--rebase"]);
    assert!(
        err.contains("could not apply")
            && err.contains("a second")
            && err.contains("commit 2 of 3"),
        "{err}"
    );
    assert!(
        err.contains("pyn pull --continue") && err.contains("--skip") && err.contains("--abort"),
        "{err}"
    );
    assert!(read(&ws, "Source/a.cpp").contains("<<<<<<< HEAD\ntheirs\n"));
    assert!(read(&ws, "Source/a.cpp").contains(">>>>>>> "));
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");
    assert!(
        !ws.join("Source/c.cpp").exists(),
        "the commit after the stop is not applied yet"
    );
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("rebasing") && status.contains("unmerged"),
        "{status}"
    );

    let err = env.fails(&ws, "alice", &["pull", "--continue"]);
    assert!(err.contains("unmerged"), "{err}");
    write(&ws, "Source/a.cpp", "both\ntwo\nthree\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["pull", "--continue"]);
    assert!(out.contains("Rebased 3 local commit(s)"), "{out}");
    assert_eq!(read(&ws, "Source/c.cpp"), "cee");
    assert_eq!(
        git(&ws, &["rev-list", "--count", "origin/main..main"]),
        "3\n"
    );
    assert_eq!(
        git(&ws, &["log", "--format=%s", "origin/main..main"]),
        "c third\na second\nb first\n"
    );
    assert_eq!(
        git(&ws, &["cat-file", "blob", "main:Source/a.cpp"]),
        "both\ntwo\nthree\n"
    );
    git(&ws, &["fsck", "--strict"]);
    assert!(!env.ok(&ws, "alice", &["status"]).contains("rebasing"));
    env.ok(&ws, "alice", &["push"]);
    let third = clone_with(&env, "third", &[]);
    assert_eq!(read(&third, "Source/a.cpp"), "both\ntwo\nthree\n");
    assert_eq!(read(&third, "Source/c.cpp"), "cee");
}

#[test]
fn a_rebase_can_skip_the_conflicting_commit_or_be_aborted() {
    let (env, ws) = seeded();
    rebase_clash(&env, &ws);
    let local = git(&ws, &["rev-parse", "main"]);
    env.fails(&ws, "alice", &["pull", "--rebase"]);
    let out = env.ok(&ws, "alice", &["pull", "--skip"]);
    assert!(
        out.contains("Rebased 2 local commit(s)") && out.contains("dropped"),
        "{out}"
    );
    assert_eq!(read(&ws, "Source/a.cpp"), "theirs\ntwo\nthree\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");
    assert_eq!(read(&ws, "Source/c.cpp"), "cee");
    assert_eq!(
        git(&ws, &["log", "--format=%s", "origin/main..main"]),
        "c third\nb first\n"
    );

    let (env, ws) = seeded();
    rebase_clash(&env, &ws);
    let local2 = git(&ws, &["rev-parse", "main"]);
    env.fails(&ws, "alice", &["pull", "--rebase"]);
    write(&ws, "Source/a.cpp", "scribble\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["pull", "--abort"]);
    assert_eq!(git(&ws, &["rev-parse", "main"]), local2);
    assert_eq!(read(&ws, "Source/a.cpp"), "mine\ntwo\nthree\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");
    assert_eq!(read(&ws, "Source/c.cpp"), "cee");
    assert!(!env.ok(&ws, "alice", &["status"]).contains("unmerged"));
    let _ = local;
}

/// Two clones commit the same change at the start of a wall-clock second, so both get the same commit id.
fn identical_pair(env: &Env, names: [&str; 2]) -> Option<[PathBuf; 2]> {
    let ws = names.map(|n| clone_with(env, n, &["--local-store"]));
    for (w, branch) in ws.iter().zip(["one", "two"]) {
        env.server_branch(w, "alice", branch);
    }
    let into_second = u64::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_millis(),
    );
    std::thread::sleep(std::time::Duration::from_millis(1000 - into_second));
    for w in &ws {
        commit_local(env, w, "alice", "Source/a.cpp", "same\n", "same change");
    }
    let ids = ws.clone().map(|w| git(&w, &["rev-parse", "HEAD"]));
    (ids[0] == ids[1]).then_some(ws)
}

#[test]
fn a_commit_identical_to_one_pushed_on_another_branch_is_pushed() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one\ntwo\nthree\n", None);
    let mut pair = None;
    for attempt in 0..10 {
        pair = identical_pair(&env, [&format!("a{attempt}"), &format!("b{attempt}")]);
        if pair.is_some() {
            break;
        }
    }
    let [one, two] = pair.expect("two identical commits in one second");
    env.ok(&one, "alice", &["push"]);
    let out = env.ok(&two, "alice", &["push"]);
    assert!(!out.contains("rejected"), "{out}");
    let log = |ws: &Path| env.ok(ws, "alice", &["log", "--commits", "--limit", "1"]);
    assert_eq!(log(&one), log(&two));
}

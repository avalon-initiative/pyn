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
    env.ok(ws, "alice", &["log", "--commits"]).lines().count()
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
fn overlapping_edits_are_refused_by_pull_and_change_nothing() {
    let (env, ws) = seeded();
    let other = clone_with(&env, "other", &["--local-store"]);
    commit_local(
        &env,
        &other,
        "alice",
        "Source/a.cpp",
        "theirs\ntwo\nthree\n",
        "t",
    );
    env.ok(&other, "alice", &["push"]);
    commit_local(
        &env,
        &ws,
        "alice",
        "Source/a.cpp",
        "mine\ntwo\nthree\n",
        "m",
    );
    let local = git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["pull"]);
    assert!(
        err.contains("Source/a.cpp") && err.contains("nothing was changed"),
        "{err}"
    );
    assert_eq!(git(&ws, &["rev-parse", "main"]), local);
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "mine\ntwo\nthree\n"
    );
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
    env.ok(&other, "bob", &["branch", "feature"]);
    env.ok(&other, "bob", &["switch", "feature"]);
    env.ok(&other, "bob", &["update"]);
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
    assert_eq!(server_commits(&env, &ws), before);
    assert_eq!(
        git(&ws, &["rev-parse", "origin/main"]),
        git(&ws, &["rev-parse", "main~2"]),
    );
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
fn without_the_setting_fetch_pull_and_push_are_as_before() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &[]);
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
    assert!(err.contains("nothing was changed"), "{err}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), local);
    assert_eq!(
        std::fs::read_to_string(ws.join("Content/m.umap")).unwrap(),
        "map by alice"
    );
    let err = env.fails(&ws, "alice", &["pull", "--rebase"]);
    assert!(err.contains("Content/m.umap"), "{err}");

    env.ok(&ws, "alice", &["lock", "Content/m.umap", "--base", "2"]);
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

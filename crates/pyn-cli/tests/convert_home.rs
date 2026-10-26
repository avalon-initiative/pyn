//! `pyn convert --home git`: a pyn server workspace becomes a git home with the server's history, compared with the
//! server's own commits. Real git is the oracle in one test, for semantics only.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Env, REPO, start};
use pyn_gitwork::Repo;
use pyn_object::{FileMode, Kind, ObjectId};
use pyn_proto as api;

fn clone(env: &Env, name: &str) -> PathBuf {
    let ws = env.dir(name);
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--local-store",
            &env.source(REPO),
            ws.to_str().unwrap(),
        ],
    );
    ws.canonicalize().unwrap()
}

fn commit(env: &Env, ws: &Path, path: &str, bytes: &[u8], message: &str) {
    let file = ws.join(path);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    if file.exists() {
        let mut perms = std::fs::metadata(&file).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&file, perms).unwrap();
    }
    std::fs::write(&file, bytes).unwrap();
    env.ok(ws, "alice", &["commit", path, "-m", message]);
}

/// Two branches, a merge commit, a file with several revisions and a binary file.
fn fixture() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one\n", None);
    env.seed("alice", "Source/a.cpp", "one\ntwo\n", Some(1));
    env.seed("alice", "Source/b.cpp", "bee\n", None);
    let ws = clone(&env, "ws");
    env.server_branch(&ws, "alice", "feature");
    let binary: Vec<u8> = (0..=255u8).chain([0, 0, 255, 10, 13]).collect();
    commit(&env, &ws, "assets/blob.bin", &binary, "binary on feature");
    commit(
        &env,
        &ws,
        "Source/a.cpp",
        b"one\ntwo\nthree\n",
        "a on feature",
    );
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/b.cpp", b"bee from main\n", "b on main");
    env.ok(
        &ws,
        "alice",
        &["merge", "feature", "-m", "bring in feature"],
    );
    commit(
        &env,
        &ws,
        "Source/c.cpp",
        b"after merge\n",
        "after the merge",
    );
    (env, ws)
}

struct Server {
    heads: BTreeMap<String, String>,
    bodies: HashMap<String, Vec<u8>>,
}

fn server_state(env: &Env) -> Server {
    let client = reqwest::blocking::Client::new();
    let mut state = Server {
        heads: BTreeMap::new(),
        bodies: HashMap::new(),
    };
    let mut cursor: Option<String> = None;
    loop {
        let page: api::FetchResponse = client
            .post(format!("{}/v1/repos/{REPO}/fetch", env.url))
            .header("x-pyn-user", "alice")
            .json(&api::FetchRequest {
                cursor: cursor.take(),
                ..Default::default()
            })
            .send()
            .unwrap()
            .json()
            .unwrap();
        for r in page.refs {
            if let Some(h) = r.head {
                state.heads.insert(r.branch, h);
            }
        }
        for c in page.commits {
            state.bodies.insert(c.id, STANDARD.decode(c.data).unwrap());
        }
        match page.next {
            Some(n) => cursor = Some(n),
            None => return state,
        }
    }
}

fn open(ws: &Path) -> Repo {
    Repo::open(ws, None, None).unwrap()
}

fn id(hex: &str) -> ObjectId {
    ObjectId::parse(hex).unwrap()
}

/// Every file under `dir` with its bytes, to prove a failed command changed nothing.
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p.clone());
                out.insert(p, Vec::new());
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap_or_default());
            }
        }
    }
    out
}

fn convert(env: &Env, ws: &Path) {
    env.ok(ws, "alice", &["convert", "--home", "git", "--yes"]);
}

#[test]
fn history_survives_with_the_servers_ids() {
    let (env, ws) = fixture();
    let server = server_state(&env);
    assert!(server.heads.len() >= 2 && server.bodies.len() >= 8);
    convert(&env, &ws);

    let repo = open(&ws);
    assert_eq!(repo.algorithm(), pyn_object::Algorithm::Sha256);
    let branches: BTreeMap<String, String> = repo
        .refs()
        .list("refs/heads/")
        .unwrap()
        .into_iter()
        .map(|(n, i)| (n.trim_start_matches("refs/heads/").to_string(), i.to_hex()))
        .collect();
    assert_eq!(branches, server.heads);
    assert_eq!(repo.head().unwrap().id(), Some(id(&server.heads["main"])));

    let mut seen = HashSet::new();
    let mut queue: Vec<ObjectId> = server.heads.values().map(|h| id(h)).collect();
    let mut merges = 0;
    while let Some(c) = queue.pop() {
        if !seen.insert(c) {
            continue;
        }
        let object = repo.objects().read(&c).unwrap();
        assert_eq!(object.kind, Kind::Commit);
        assert_eq!(object.data, server.bodies[&c.to_hex()], "commit {c}");
        let parsed = repo.commit(&c).unwrap();
        merges += usize::from(parsed.parents.len() > 1);
        queue.extend(parsed.parents);
        let mut trees = vec![parsed.tree];
        while let Some(t) = trees.pop() {
            for e in repo.tree(&t).unwrap() {
                if e.mode == FileMode::Tree {
                    trees.push(e.id);
                } else {
                    let blob = repo.objects().read(&e.id).unwrap();
                    assert_eq!(blob.kind, Kind::Blob);
                }
            }
        }
    }
    assert_eq!(seen.len(), server.bodies.len());
    assert!(merges >= 1);

    let on_main = repo
        .flatten(
            Some(&repo.commit(&id(&server.heads["main"])).unwrap().tree),
            &Default::default(),
        )
        .unwrap();
    let binary = repo
        .blob(&on_main[b"assets/blob.bin".as_slice()].id)
        .unwrap();
    assert_eq!(
        binary,
        (0..=255u8).chain([0, 0, 255, 10, 13]).collect::<Vec<u8>>()
    );
}

#[test]
fn the_workspace_is_a_clean_git_home_afterwards() {
    let (env, ws) = fixture();
    std::fs::write(ws.join("Source/b.cpp"), "edited after\n").unwrap();
    convert(&env, &ws);
    assert!(ws.join(".git").is_dir() && !ws.join(".git.pyn-convert").exists());
    assert_eq!(
        std::fs::read_to_string(ws.join(".pyn/local_only/repo.toml")).unwrap(),
        "repo_tool = \"git\"\n"
    );
    assert!(!ws.join(".pyn/local_only/state").exists());
    let status = env.ok(&ws, "alice", &["status", "--short"]);
    assert_eq!(status, " M Source/b.cpp\n");
    std::fs::write(ws.join("Source/b.cpp"), "bee from main\n").unwrap();
    assert_eq!(env.ok(&ws, "alice", &["status", "--short"]), "");
    let log = env.ok(&ws, "alice", &["log", "--oneline"]);
    assert!(log.contains("after the merge"), "{log}");
    assert!(env.ok(&ws, "alice", &["branch"]).contains("feature"));
    env.ok(&ws, "alice", &["switch", "feature"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "one\ntwo\nthree\n"
    );
}

#[test]
fn unpublished_local_commits_are_kept() {
    let (env, ws) = fixture();
    env.ok(&ws, "alice", &["pull"]);
    std::fs::write(ws.join("Source/local.cpp"), "local\n").unwrap();
    env.ok(&ws, "alice", &["add", "Source/local.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "only local"]);
    let out = env.ok(&ws, "alice", &["convert", "--home", "git", "--yes"]);
    assert!(out.contains("unpublished local"), "{out}");
    assert!(
        env.ok(&ws, "alice", &["log", "--oneline"])
            .contains("only local")
    );
    assert_eq!(env.ok(&ws, "alice", &["status", "--short"]), "");
}

#[test]
fn without_yes_and_without_a_terminal_it_refuses() {
    let (env, ws) = fixture();
    let before = snapshot(&ws);
    let out = env.run(&ws, "alice", &["convert", "--home", "git"]);
    assert_eq!(out.status.code(), Some(128));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no terminal") && err.contains("--yes"),
        "{err}"
    );
    assert!(
        err.contains("locks") && err.contains("revision numbers"),
        "{err}"
    );
    assert_eq!(snapshot(&ws), before);
}

#[test]
fn outside_a_server_workspace_it_fails() {
    let env = start();
    let dir = env.dir("empty");
    let out = env.run(&dir, "alice", &["convert", "--home", "git", "--yes"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(!dir.join(".git").exists());
}

struct Proxy {
    url: String,
    fail: Arc<AtomicBool>,
}

fn proxy(upstream: &str) -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let fail = Arc::new(AtomicBool::new(false));
    let (flag, upstream) = (fail.clone(), upstream.to_string());
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let (flag, upstream) = (flag.clone(), upstream.clone());
            std::thread::spawn(move || relay(conn, &upstream, &flag));
        }
    });
    Proxy { url, fail }
}

fn relay(mut conn: TcpStream, upstream: &str, fail: &AtomicBool) {
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
    if fail.load(Ordering::SeqCst) && path.contains("/objects/") {
        let _ = write!(
            conn,
            "HTTP/1.1 500 X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        return;
    }
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
    let ctype = resp
        .headers()
        .get("content-type")
        .map_or("application/octet-stream".to_string(), |v| {
            v.to_str().unwrap().to_string()
        });
    let body = resp.bytes().unwrap();
    let _ = write!(
        conn,
        "HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = conn.write_all(&body);
}

#[test]
fn a_server_error_midway_leaves_the_workspace_as_it_was() {
    let (env, _) = fixture();
    let proxy = proxy(&env.url);
    let ws = env.dir("via-proxy");
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
    let cloned = run(&[
        "clone",
        "--local-store",
        &format!("{}/{REPO}", proxy.url),
        ".",
    ]);
    assert!(
        cloned.status.success(),
        "{}",
        String::from_utf8_lossy(&cloned.stderr)
    );
    let before = snapshot(&ws);
    proxy.fail.store(true, Ordering::SeqCst);
    let out = run(&["convert", "--home", "git", "--yes"]);
    assert!(!out.status.success());
    assert!(!ws.join(".git").exists() && !ws.join(".git.pyn-convert").exists());
    assert_eq!(snapshot(&ws), before);

    proxy.fail.store(false, Ordering::SeqCst);
    let out = run(&["convert", "--home", "git", "--yes"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn real_git_reads_the_result() {
    let (env, ws) = fixture();
    let server = server_state(&env);
    convert(&env, &ws);
    git(&ws, &["fsck", "--strict"]);
    assert_eq!(
        git(&ws, &["rev-parse", "--show-object-format"]).trim(),
        "sha256"
    );
    let listed: BTreeSet<String> = git(&ws, &["log", "--all", "--format=%H"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        listed,
        server.bodies.keys().cloned().collect::<BTreeSet<_>>()
    );
    assert_eq!(git(&ws, &["status", "--porcelain"]), "");
}

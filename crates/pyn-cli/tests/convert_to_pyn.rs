//! `pyn convert --home pyn`: a git home repository moves to a repository on a pyn server with its history, against an
//! in-process server. Real git builds the fixtures and checks the round trip, for semantics only.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use common::{Env, start_empty};
use pyn_proto as api;

const NEW: &str = "alice/moved";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Ann")
        .env("GIT_AUTHOR_EMAIL", "ann@example.com")
        .env("GIT_COMMITTER_NAME", "Cy")
        .env("GIT_COMMITTER_EMAIL", "cy@example.com")
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

fn write(dir: &Path, path: &str, bytes: &[u8]) {
    let file = dir.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, bytes).unwrap();
}

fn commit(dir: &Path, path: &str, bytes: &[u8], message: &str) {
    write(dir, path, bytes);
    git(dir, &["add", path]);
    git(dir, &["commit", "-q", "-m", message]);
}

/// A repository on `master` with a feature branch, a merge, a branch merged and deleted, a binary file and a tag.
fn fixture(env: &Env, format: &str) -> PathBuf {
    let dir = env.dir(&format!("git-{format}"));
    git(
        &dir,
        &[
            "init",
            "-q",
            "-b",
            "master",
            &format!("--object-format={format}"),
        ],
    );
    commit(&dir, "src/a.txt", b"one\n", "first");
    commit(
        &dir,
        "assets/m.bin",
        &(0..=255u8).collect::<Vec<_>>(),
        "binary",
    );
    git(&dir, &["checkout", "-q", "-b", "feature"]);
    commit(&dir, "src/a.txt", b"one\ntwo\n", "a on feature");
    git(&dir, &["checkout", "-q", "master"]);
    git(&dir, &["checkout", "-q", "-b", "side"]);
    commit(&dir, "src/side.txt", b"side\n", "side work");
    git(&dir, &["checkout", "-q", "master"]);
    commit(&dir, "src/b.txt", b"bee\n", "b on master");
    git(
        &dir,
        &["merge", "-q", "--no-ff", "-m", "bring in side", "side"],
    );
    git(&dir, &["branch", "-q", "-d", "side"]);
    commit(&dir, "src/c.txt", b"after\n", "after the merge");
    git(&dir, &["tag", "v1"]);
    dir
}

fn convert(env: &Env, dir: &Path, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["--repo", NEW, "convert", "--home", "pyn", "--yes"];
    args.extend(extra);
    env.run(dir, "alice", &args)
}

fn ok(out: &std::process::Output) -> String {
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn get<T: serde::de::DeserializeOwned>(env: &Env, route: &str) -> T {
    reqwest::blocking::Client::new()
        .get(format!("{}{route}", env.url))
        .header("x-pyn-user", "alice")
        .send()
        .unwrap()
        .json()
        .unwrap()
}

fn server_heads(env: &Env, repo: &str) -> BTreeMap<String, String> {
    let branches: Vec<api::Branch> = get(env, &format!("/v1/repos/{repo}/branches"));
    branches
        .into_iter()
        .filter_map(|b| b.commit.map(|c| (b.name, c)))
        .collect()
}

/// Every commit id the server holds, from a full fetch.
fn server_commits(env: &Env, repo: &str) -> BTreeMap<String, Vec<u8>> {
    let client = reqwest::blocking::Client::new();
    let (mut out, mut cursor) = (BTreeMap::new(), None);
    loop {
        let page: api::FetchResponse = client
            .post(format!("{}/v1/repos/{repo}/fetch", env.url))
            .header("x-pyn-user", "alice")
            .json(&api::FetchRequest {
                cursor: cursor.take(),
                ..Default::default()
            })
            .send()
            .unwrap()
            .json()
            .unwrap();
        for c in page.commits {
            out.insert(c.id, STANDARD.decode(c.data).unwrap());
        }
        match page.next {
            Some(n) => cursor = Some(n),
            None => return out,
        }
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_string).collect()
}

#[allow(dead_code)]
fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p);
            } else {
                out.insert(p.clone(), std::fs::read(&p).unwrap_or_default());
            }
        }
    }
    out
}

#[test]
fn a_sha256_repository_keeps_every_commit_id() {
    let env = start_empty();
    let dir = fixture(&env, "sha256");
    let ids = lines(&git(&dir, &["log", "--all", "--format=%H"]));
    let out = ok(&convert(&env, &dir, &["--exclusive", "assets/"]));
    assert!(out.contains("converted"), "{out}");

    let heads = server_heads(&env, NEW);
    assert_eq!(
        heads.keys().cloned().collect::<Vec<_>>(),
        ["feature", "main"]
    );
    assert_eq!(
        heads["feature"],
        git(&dir, &["rev-parse", "feature"]).trim()
    );
    let held = server_commits(&env, NEW);
    for id in &ids {
        assert!(held.contains_key(id), "commit {id} is not on the server");
    }
    // the policy commit and the conversion of the files it makes exclusive sit on top of the old master
    let tip = git(&dir, &["rev-parse", "master"]);
    assert_ne!(heads["main"], tip.trim());
    assert_eq!(held.len(), ids.len() + 2);
    let policy = get_file(&env, ".pyn/pyn.toml");
    assert!(policy.contains("assets/"), "{policy}");
    assert!(
        dir.join("assets/m.bin")
            .metadata()
            .unwrap()
            .permissions()
            .readonly()
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(".pyn/pyn.toml")).unwrap(),
        policy
    );
}

fn get_file(env: &Env, path: &str) -> String {
    let client = reqwest::blocking::Client::new();
    client
        .get(format!("{}/v1/repos/{NEW}/content", env.url))
        .header("x-pyn-user", "alice")
        .query(&[("path", path)])
        .send()
        .unwrap()
        .text()
        .unwrap()
}

#[test]
fn a_sha1_repository_is_rehashed_and_survives_the_round_trip() {
    let env = start_empty();
    let dir = fixture(&env, "sha1");
    let before = git(
        &dir,
        &["log", "--all", "--format=%an|%ae|%at|%cn|%ce|%ct|%s"],
    );
    let old_tip_tree: String = git(&dir, &["ls-tree", "-r", "master"]);
    let status = git(&dir, &["status", "--porcelain"]);
    let unchanged = |dir: &Path| {
        git(dir, &["status", "--porcelain"])
            .lines()
            .filter(|l| !l.ends_with(".pyn/"))
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let out = ok(&convert(&env, &dir, &[]));
    assert!(out.contains("workspace on 'main'"), "{out}");

    let heads = server_heads(&env, NEW);
    assert!(!heads.values().any(|h| h.len() == 40));
    assert_eq!(held_len(&env), lines(&before).len() + 1);
    assert_eq!(unchanged(&dir), lines(&status));
    assert_eq!(
        git(&dir, &["rev-parse", "--show-object-format"]).trim(),
        "sha1"
    );
    git(&dir, &["fsck", "--strict"]);
    assert_eq!(
        std::fs::read_to_string(dir.join(".pyn/local_only/repo.toml")).unwrap(),
        "repo_tool = \"pyn\"\n"
    );

    // back to a git home; the old .git is moved aside first
    std::fs::rename(dir.join(".git"), dir.join(".git.orig")).unwrap();
    ok(&env.run(&dir, "alice", &["convert", "--home", "git", "--yes"]));
    assert_eq!(
        git(&dir, &["rev-parse", "--show-object-format"]).trim(),
        "sha256"
    );
    git(&dir, &["fsck", "--strict"]);
    let after = git(
        &dir,
        &["log", "--all", "--format=%an|%ae|%at|%cn|%ce|%ct|%s"],
    );
    let got: BTreeSet<String> = lines(&after).into_iter().collect();
    let want: BTreeSet<String> = lines(&before).into_iter().collect();
    let extra: Vec<&String> = got.difference(&want).collect();
    assert_eq!(extra.len(), 1, "{extra:?}");
    assert!(extra[0].ends_with("|Initial policy"));
    assert!(want.is_subset(&got));
    // the tree of the old master is the new main without the policy file
    let new_tree = git(&dir, &["ls-tree", "-r", "--name-only", "main"]);
    let old_names: Vec<String> = old_tip_tree
        .lines()
        .map(|l| l.split('\t').nth(1).unwrap().to_string())
        .collect();
    let mut new_names = lines(&new_tree);
    new_names.retain(|n| n != ".pyn/pyn.toml");
    assert_eq!(new_names, old_names);
    for name in &old_names {
        assert_eq!(
            git(&dir, &["show", &format!("main:{name}")]),
            String::from_utf8_lossy(&std::fs::read(dir.join(name)).unwrap()),
        );
    }
}

fn held_len(env: &Env) -> usize {
    server_commits(env, NEW).len()
}

fn refused(env: &Env, dir: &Path, code: i32, needle: &str) {
    let before = snapshot(dir);
    let out = convert(env, dir, &[]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(code), "{err}");
    assert!(err.contains(needle), "{err}");
    assert_eq!(snapshot(dir), before);
    let probe = reqwest::blocking::Client::new()
        .get(format!("{}/v1/repos/{NEW}/branches", env.url))
        .header("x-pyn-user", "alice")
        .send()
        .unwrap();
    assert_eq!(probe.status().as_u16(), 404, "the repository was created");
}

fn plain(env: &Env, name: &str) -> PathBuf {
    let dir = env.dir(name);
    git(&dir, &["init", "-q", "-b", "main"]);
    commit(&dir, "a.txt", b"a\n", "first");
    dir
}

#[test]
fn what_the_server_does_not_store_is_refused_before_anything_changes() {
    let env = start_empty();
    let sub = plain(&env, "submodule");
    let id = "1".repeat(40);
    git(
        &sub,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{id},vendor"),
        ],
    );
    git(&sub, &["commit", "-q", "-m", "submodule"]);
    refused(&env, &sub, 128, "submodule entry 'vendor'");

    let empty = plain(&env, "empty");
    git(&empty, &["commit", "-q", "--allow-empty", "-m", "nothing"]);
    refused(&env, &empty, 128, "changes no file");

    let octopus = plain(&env, "octopus");
    for b in ["one", "two", "three"] {
        git(&octopus, &["checkout", "-q", "-b", b, "main"]);
        commit(&octopus, &format!("{b}.txt"), b.as_bytes(), b);
    }
    git(&octopus, &["checkout", "-q", "main"]);
    git(
        &octopus,
        &["merge", "-q", "-m", "octopus", "one", "two", "three"],
    );
    refused(&env, &octopus, 128, "merge of 3 parents");

    let roots = plain(&env, "roots");
    git(&roots, &["checkout", "-q", "--orphan", "other"]);
    commit(&roots, "b.txt", b"b\n", "second root");
    git(&roots, &["checkout", "-q", "main"]);
    refused(&env, &roots, 128, "2 root commits");

    let detached = plain(&env, "detached");
    git(&detached, &["checkout", "-q", "--detach"]);
    refused(&env, &detached, 128, "HEAD is detached");

    let busy = plain(&env, "busy");
    std::fs::write(busy.join(".git/MERGE_HEAD"), "0".repeat(40)).unwrap();
    refused(&env, &busy, 128, "MERGE_HEAD");
}

#[test]
fn it_asks_first_and_needs_a_repository_and_a_git_home() {
    let env = start_empty();
    let dir = plain(&env, "ask");
    let before = snapshot(&dir);
    let out = env.run(&dir, "alice", &["--repo", NEW, "convert", "--home", "pyn"]);
    assert_eq!(out.status.code(), Some(128));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no terminal") && err.contains("--yes") && err.contains("tags"),
        "{err}"
    );
    assert_eq!(snapshot(&dir), before);

    let out = env.run(&dir, "alice", &["convert", "--home", "pyn", "--yes"]);
    assert_eq!(out.status.code(), Some(129));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--repo owner/name"));

    let out = env.run(
        &env.dir("nogit"),
        "alice",
        &["--repo", NEW, "convert", "--home", "pyn", "--yes"],
    );
    assert_eq!(out.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&out.stderr).contains("this is not one"));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_repository_with_history_is_never_merged_into_and_an_empty_one_is_filled() {
    let env = start_empty();
    let first = plain(&env, "first");
    ok(&convert(&env, &first, &[]));
    let second = plain(&env, "second");
    let before = snapshot(&second);
    let out = convert(&env, &second, &[]);
    assert_eq!(out.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&out.stderr).contains("already has history"));
    assert_eq!(snapshot(&second), before);

    let env = common::start();
    let dir = plain(&env, "into-existing");
    let out = env.run(
        &dir,
        "alice",
        &["--repo", common::REPO, "convert", "--home", "pyn", "--yes"],
    );
    ok(&out);
    assert_eq!(server_heads(&env, common::REPO).len(), 1);
}

#[test]
fn the_folder_works_as_a_pyn_home_afterwards() {
    let env = start_empty();
    let dir = fixture(&env, "sha256");
    ok(&convert(&env, &dir, &["--exclusive", "assets/"]));
    let status = env.ok(&dir, "alice", &["status"]);
    assert!(
        status.contains("On branch master") || status.contains("On branch main"),
        "{status}"
    );
    assert!(
        !status.contains(".git/") && !status.contains("untracked"),
        "{status}"
    );
    let branch = env.ok(&dir, "alice", &["branch"]);
    assert!(branch.contains("feature"), "{branch}");
    let log = env.ok(&dir, "alice", &["log"]);
    assert!(log.contains("after the merge"), "{log}");

    write(&dir, "src/c.txt", b"changed\n");
    env.ok(
        &dir,
        "alice",
        &["commit", "src/c.txt", "-m", "edit on the server"],
    );
    assert!(
        env.ok(&dir, "alice", &["log"])
            .contains("edit on the server")
    );

    let binary = dir.join("assets/m.bin");
    assert!(binary.metadata().unwrap().permissions().readonly());
    env.ok(&dir, "alice", &["lock", "assets/m.bin"]);
    let mut perms = binary.metadata().unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&binary, perms).unwrap();
    std::fs::write(&binary, b"new bytes").unwrap();
    env.ok(
        &dir,
        "alice",
        &["commit", "assets/m.bin", "-m", "edit the binary"],
    );
}

#[test]
fn a_long_history_goes_in_several_pushes() {
    let env = start_empty();
    let dir = env.dir("long");
    git(&dir, &["init", "-q", "-b", "main"]);
    let mut stream = String::new();
    for n in 1..=2100 {
        let body = format!("{n}\n");
        stream.push_str(&format!(
            "commit refs/heads/main\nmark :{n}\ncommitter C <c@x> {n} +0000\ndata 2\nc\nM 100644 inline f.txt\ndata {}\n{body}\n",
            body.len()
        ));
    }
    stream.push_str("commit refs/heads/side\ncommitter C <c@x> 5000 +0000\ndata 5\nside\nfrom :100\nM 100644 inline g.txt\ndata 2\ng\n\n");
    stream.push_str("commit refs/heads/main\ncommitter C <c@x> 6000 +0000\ndata 6\nmerge\nmerge refs/heads/side\nM 100644 inline h.txt\ndata 2\nh\n\n");
    let mut child = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["fast-import", "--quiet"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    git(&dir, &["checkout", "-q", "-f", "main"]);
    let count = lines(&git(&dir, &["rev-list", "--all"])).len();
    assert_eq!(count, 2102);
    ok(&convert(&env, &dir, &[]));
    assert_eq!(held_len(&env), count + 1);
    let heads = server_heads(&env, NEW);
    assert_eq!(heads.len(), 2);
}

#[test]
fn a_policy_the_server_refuses_leaves_the_folder_a_git_home() {
    let env = start_empty();
    let dir = plain(&env, "bad-policy");
    write(&dir, ".pyn/pyn.toml", b"this is [not toml");
    let out = convert(&env, &dir, &[]);
    assert_eq!(out.status.code(), Some(128));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("policy was not accepted") && err.contains("folder was not changed"),
        "{err}"
    );
    assert!(!dir.join(".pyn/local_only/state/files.toml").exists());
    assert!(!dir.join(".pyn/local_only/config.toml").exists());
    assert!(
        env.ok(&dir, "alice", &["log", "--oneline"])
            .contains("first")
    );
}

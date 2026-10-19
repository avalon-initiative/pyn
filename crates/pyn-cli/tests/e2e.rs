//! End to end: the real `pyn` and `pyn-server` binaries (in-memory store) against real temporary repositories.
//! The git binary appears only as an oracle for what git itself reports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

use common::gitfix::snapshot;
use common::real::{POLICY, Server};

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
        [
            ".git/info/exclude",
            ".pyn",
            ".pyn/local_only",
            ".pyn/local_only/repo.toml"
        ]
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
        &[
            "clone",
            "--no-local-store",
            &server.source(),
            ws.to_str().unwrap(),
        ],
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
        &[
            "clone",
            "--no-local-store",
            &server.source(),
            other.to_str().unwrap(),
        ],
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

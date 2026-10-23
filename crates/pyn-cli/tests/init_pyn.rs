//! `pyn init`: starts a pyn repository in a folder that is not tracked (#570). No git binary is used.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        Rig { _tmp: tmp, root }
    }

    /// Runs `pyn` with no server configured anywhere.
    fn pyn(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home"))
            .output()
            .unwrap()
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.pyn(cwd, args);
        assert!(out.status.success(), "{args:?}: {}", text(&out.stderr));
        text(&out.stdout)
    }

    fn work(&self) -> PathBuf {
        let w = self.root.join("work");
        fs::create_dir_all(&w).unwrap();
        w
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn assert_refused(out: &Output, names_convert: bool) {
    assert_eq!(out.status.code(), Some(128), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.starts_with("fatal: "), "{err}");
    assert_eq!(err.contains("pyn convert"), names_convert, "{err}");
}

#[test]
fn init_creates_the_folder_and_the_workspace_files() {
    let rig = Rig::new();
    let target = rig.root.join("new/repo");
    let out = rig.ok(&rig.root, &["init", "new/repo"]);
    assert!(
        out.starts_with("Initialized empty pyn repository in "),
        "{out}"
    );
    let listed = files(&target);
    for want in [".pyn/pyn.toml", ".pyn/local_only/repo.toml"] {
        assert!(listed.contains(&want.to_string()), "{listed:?}");
    }
    assert!(!listed.iter().any(|f| f.starts_with(".git")), "{listed:?}");
    assert_eq!(
        fs::read_to_string(target.join(".pyn/local_only/repo.toml")).unwrap(),
        "repo_tool = \"pyn\"\n"
    );
    assert!(!target.join(".pyn/local_only/config.toml").exists());
}

#[test]
fn the_policy_file_is_valid_and_shares_every_path() {
    let rig = Rig::new();
    rig.ok(&rig.work(), &["init"]);
    let policy = fs::read_to_string(rig.work().join(".pyn/pyn.toml")).unwrap();
    assert!(policy.contains("default = \"shared\""), "{policy}");
    assert!(policy.contains("# [exclusive]"), "{policy}");
    let rules = pyn_core::Rules::from_toml(&policy).unwrap();
    assert_eq!(
        rules.mode_for(&pyn_core::RepoPath::new("anything/at/all.bin").unwrap()),
        pyn_core::Mode::Shared
    );
}

#[test]
fn init_works_in_an_existing_empty_directory_and_defaults_to_the_current_one() {
    let rig = Rig::new();
    rig.ok(&rig.work(), &["init"]);
    assert!(rig.work().join(".pyn/pyn.toml").is_file());

    let empty = rig.root.join("empty");
    fs::create_dir(&empty).unwrap();
    rig.ok(&rig.root, &["init", "empty"]);
    assert!(empty.join(".pyn/local_only/repo.toml").is_file());
}

#[test]
fn init_leaves_other_files_in_the_folder_alone() {
    let rig = Rig::new();
    fs::write(rig.work().join("notes.txt"), "mine").unwrap();
    rig.ok(&rig.work(), &["init"]);
    assert_eq!(
        fs::read_to_string(rig.work().join("notes.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn status_and_config_work_without_a_server() {
    let rig = Rig::new();
    let w = rig.work();
    rig.ok(&w, &["init"]);

    let status = rig.pyn(&w, &["status"]);
    assert_eq!(status.status.code(), Some(128));
    let err = text(&status.stderr);
    assert!(err.contains("no server is set"), "{err}");

    assert_eq!(rig.ok(&w, &["config", "list", "--local"]), "no settings\n");
    rig.ok(&w, &["config", "set", "server", "http://localhost:1"]);
    assert_eq!(
        rig.ok(&w, &["config", "get", "server"]).trim(),
        "http://localhost:1"
    );
}

#[test]
fn status_in_the_workspace_asks_the_configured_server() {
    let env = common::start();
    let w = env.dir("w");
    env.ok(&w, "alice", &["init"]);
    let out = env.ok(&w, "alice", &["status"]);
    assert!(!out.contains("fatal"), "{out}");
}

#[test]
fn init_in_a_git_repository_is_fatal_and_names_convert() {
    let rig = Rig::new();
    let w = rig.work();
    rig.ok(&w, &["init-git"]);
    let before = files(&w);
    assert_refused(&rig.pyn(&w, &["init"]), true);
    assert_eq!(files(&w), before);
}

#[test]
fn init_inside_a_work_tree_is_fatal() {
    let rig = Rig::new();
    let w = rig.work();
    rig.ok(&w, &["init-git"]);
    assert_refused(&rig.pyn(&w, &["init", "sub/dir"]), true);
    assert!(!w.join("sub").exists());
}

#[test]
fn init_in_or_inside_a_pyn_workspace_is_fatal_and_changes_nothing() {
    let rig = Rig::new();
    let w = rig.work();
    rig.ok(&w, &["init"]);
    let before = files(&w);
    assert_refused(&rig.pyn(&w, &["init"]), false);
    assert_refused(&rig.pyn(&w, &["init", "sub"]), false);
    assert_eq!(files(&w), before);
}

#[test]
fn init_refuses_a_folder_that_only_has_a_pyn_folder() {
    let rig = Rig::new();
    fs::create_dir_all(rig.work().join(".pyn")).unwrap();
    assert_refused(&rig.pyn(&rig.work(), &["init"]), false);
}

#[test]
fn create_on_server_without_a_repository_or_a_server_is_a_usage_error() {
    let rig = Rig::new();
    let w = rig.work();
    for args in [
        vec!["init", "--create-on-server"],
        vec!["--repo", "alice/x", "init", "--create-on-server"],
        vec![
            "--server",
            "http://localhost:1",
            "init",
            "--create-on-server",
        ],
    ] {
        let out = rig.pyn(&w, &args);
        assert_eq!(
            out.status.code(),
            Some(129),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert!(!w.join(".pyn").exists(), "{args:?}");
    }
}

#[test]
fn the_two_server_flags_conflict() {
    let rig = Rig::new();
    let out = rig.pyn(
        &rig.work(),
        &["init", "--create-on-server", "--no-create-on-server"],
    );
    assert_eq!(out.status.code(), Some(129));
    assert!(!rig.work().join(".pyn").exists());
}

#[test]
fn create_on_server_creates_the_repository_and_records_it() {
    let env = common::start_empty();
    let w = env.dir("w");
    let out = env.ok(
        &w,
        "alice",
        &["--repo", "alice/fresh", "init", "--create-on-server"],
    );
    assert!(out.contains("created alice/fresh"), "{out}");
    assert!(
        env.ok(&w, "alice", &["repo", "list"])
            .contains("alice/fresh")
    );

    let config = fs::read_to_string(w.join(".pyn/local_only/config.toml")).unwrap();
    assert!(
        config.contains(&format!("server = \"{}\"", env.url)),
        "{config}"
    );
    assert!(config.contains("repo = \"alice/fresh\""), "{config}");

    let clone = env.dir("clone");
    env.ok(&clone, "alice", &["clone", &env.source("alice/fresh"), "c"]);
    let policy = fs::read_to_string(clone.join("c/.pyn/pyn.toml")).unwrap();
    assert!(policy.contains("default = \"shared\""), "{policy}");
}

#[test]
fn without_a_terminal_the_default_is_not_to_create() {
    let env = common::start_empty();
    let w = env.dir("w");
    let out = env.run(&w, "alice", &["--repo", "alice/fresh", "init"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!text(&out.stderr).contains("Create "), "no prompt");
    assert!(
        !env.ok(&w, "alice", &["repo", "list"])
            .contains("alice/fresh")
    );
    assert!(!w.join(".pyn/local_only/config.toml").exists());

    let other = env.dir("other");
    env.ok(
        &other,
        "alice",
        &["--repo", "alice/never", "init", "--no-create-on-server"],
    );
    assert!(
        !env.ok(&other, "alice", &["repo", "list"])
            .contains("alice/never")
    );
}

#[test]
fn a_failed_creation_still_leaves_the_initialized_folder_and_says_so() {
    let env = common::start_empty();
    let w = env.dir("w");
    env.ok(&w, "alice", &["repo", "create", "alice/taken"]);
    let again = env.dir("again");
    let err = env.fails(
        &again,
        "alice",
        &["--repo", "alice/taken", "init", "--create-on-server"],
    );
    assert!(
        err.contains("the repository was not created on the server"),
        "{err}"
    );
    assert!(again.join(".pyn/pyn.toml").is_file());
    assert!(!again.join(".pyn/local_only/config.toml").exists());
}

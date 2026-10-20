//! `pyn remote` on a git repository, compared with the real `git remote` on a copy of the same repository: the
//! output, the exit status and the config, refs and reflogs left behind. Needs `git` on the PATH (tests only).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::copy_tree;

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

const HEAD_ID_NOTE: &str = "refs and reflogs are copied from the git side, so ids agree";

struct Pair {
    _tmp: tempfile::TempDir,
    git: PathBuf,
    pyn: PathBuf,
    home: PathBuf,
}

fn git_cmd(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(IDENTITY)
        .output()
        .expect("git is installed")
}

impl Pair {
    /// A repository with one commit, then `setup` (git commands) run in it, then copied for pyn.
    fn new(config: &str, setup: &[&[&str]]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (git, pyn, home) = (root.join("git"), root.join("pyn"), root.join("home"));
        fs::create_dir_all(&git).unwrap();
        fs::create_dir_all(&home).unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["commit", "-q", "--allow-empty", "-m", "x"],
        ] {
            let out = git_cmd(&home, &git, args);
            assert!(out.status.success(), "git {args:?}");
        }
        for args in setup {
            let out = git_cmd(&home, &git, args);
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        if !config.is_empty() {
            let path = git.join(".git/config");
            let mut text = fs::read_to_string(&path).unwrap();
            text.push_str(config);
            fs::write(path, text).unwrap();
        }
        copy_tree(&git, &pyn);
        Self {
            _tmp: tmp,
            git,
            pyn,
            home,
        }
    }

    fn pyn_cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    /// Runs `remote <args>` in both; the output, exit status and what is left on disk must agree.
    /// `err` also compares standard error, which usage errors word differently.
    fn same(&self, args: &[&str], err: bool) {
        let mut git_args = vec!["remote"];
        git_args.extend(args);
        let mut pyn_args = vec!["remote"];
        pyn_args.extend(args);
        let (g, p) = (
            git_cmd(&self.home, &self.git, &git_args),
            self.pyn_cmd(&pyn_args),
        );
        let label = format!("remote {args:?}");
        assert_eq!(p.status.code(), g.status.code(), "{label}: exit status");
        assert_eq!(
            String::from_utf8_lossy(&p.stdout),
            String::from_utf8_lossy(&g.stdout),
            "{label}: stdout"
        );
        if err {
            assert_eq!(
                String::from_utf8_lossy(&p.stderr),
                String::from_utf8_lossy(&g.stderr),
                "{label}: stderr"
            );
        }
        self.same_disk(&label);
    }

    fn same_disk(&self, label: &str) {
        let read = |root: &Path| fs::read_to_string(root.join(".git/config")).unwrap();
        assert_eq!(read(&self.pyn), read(&self.git), "{label}: .git/config");
        assert_eq!(
            files_under(&self.pyn.join(".git/refs")),
            files_under(&self.git.join(".git/refs")),
            "{label}: refs ({HEAD_ID_NOTE})"
        );
        let logs = |root: &Path| symref_logs_by_name(files_under(&root.join(".git/logs")));
        assert_eq!(logs(&self.pyn), logs(&self.git), "{label}: logs");
    }
}

/// Keeps only the names of remote `HEAD` logs: git's text for a renamed symbolic ref differs between versions.
fn symref_logs_by_name(entries: Vec<String>) -> Vec<String> {
    let is_remote_head = |e: &str| e.starts_with("refs/remotes/") && e.contains("/HEAD: ");
    entries
        .into_iter()
        .map(|e| match e.split_once(": ") {
            Some((name, _)) if is_remote_head(&e) => name.to_string(),
            _ => e,
        })
        .collect()
}

/// Every file under `dir` with its text, and every folder, sorted.
fn files_under(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for entry in rd {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            if path.is_dir() {
                out.push(format!("{rel}/"));
                walk(root, &path, out);
            } else {
                out.push(format!("{rel}: {}", fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn listing_and_adding_match_git() {
    let pair = Pair::new("", &[]);
    for (args, err) in [
        (vec![], true),
        (vec!["add", "origin", "https://h/x.git"], true),
        (vec!["add", "origin", "u2"], true),
        (vec!["add", "bad name", "u"], true),
        (vec!["add", "a..b", "u"], true),
        (vec!["add", "", "u"], true),
        (vec!["add", "--", "-x", "u"], true),
        (vec!["add", "a/b", "ssh://git@h/x"], true),
        (vec!["add", "up", "/tmp/up"], true),
        (vec![], true),
        (vec!["-v"], true),
        (vec!["--verbose"], true),
        (vec!["add", "c"], false),
        (vec!["add", "c", "u", "extra"], false),
        (vec!["bogus"], false),
    ] {
        pair.same(&args, err);
    }
}

#[test]
fn add_keeps_the_rest_of_the_config() {
    let pair = Pair::new(
        "# my notes\n[user]\n\tname = Me ; trailing\n\n[core]\n\teditor = \"vi # x\"\n[branch \"main\"]\n\tmerge = refs/heads/main",
        &[],
    );
    pair.same(&["add", "origin", "https://h/x.git"], true);
    pair.same(&["add", "two", "u"], true);
    let text = fs::read_to_string(pair.pyn.join(".git/config")).unwrap();
    assert!(
        text.contains("# my notes\n[user]\n\tname = Me ; trailing\n\n[core]"),
        "{text}"
    );
}

#[test]
fn urls_match_git() {
    let pair = Pair::new("", &[&["remote", "add", "origin", "https://h/x.git"]]);
    for (args, err) in [
        (vec!["get-url", "origin"], true),
        (vec!["get-url", "--all", "origin"], true),
        (vec!["get-url", "--push", "origin"], true),
        (vec!["get-url", "nope"], true),
        (vec!["set-url", "nope", "x"], true),
        (vec!["set-url", "--add", "origin", "https://h/y.git"], true),
        (vec!["-v"], true),
        (vec!["get-url", "origin"], true),
        (vec!["get-url", "--all", "origin"], true),
        (vec!["set-url", "origin", "n"], true),
        (vec!["set-url", "--push", "origin", "ssh://p/x"], true),
        (vec!["-v"], true),
        (vec!["get-url", "--push", "origin"], true),
        (vec!["get-url", "--push", "--all", "origin"], true),
        (vec!["set-url", "origin", "new", "nomatch"], true),
        (vec!["set-url", "--add", "--delete", "origin", "x"], true),
        (
            vec!["set-url", "--delete", "origin", "https://h/y.git"],
            true,
        ),
        (
            vec!["set-url", "--delete", "origin", "https://h/x.git"],
            true,
        ),
        (vec!["set-url", "origin", "new", "https://h/x.git"], true),
        (vec!["set-url", "origin", "new2", "new"], true),
        (vec!["set-url", "--delete", "origin", "ssh://p/x"], true),
        (
            vec!["set-url", "--push", "--delete", "origin", "nomatch"],
            true,
        ),
        (
            vec!["set-url", "--push", "--delete", "origin", "ssh://p/x"],
            true,
        ),
        (vec!["set-url", "--add", "origin", "x", "y"], false),
        (vec!["-v"], true),
    ] {
        pair.same(&args, err);
    }
}

#[test]
fn replacing_a_url_with_several_values_fails_like_git() {
    let pair = Pair::new("", &[&["remote", "add", "a", "u"]]);
    pair.same(&["set-url", "--add", "a", "u2"], true);
    pair.same(&["set-url", "a", "n"], true);
    pair.same(&["set-url", "--delete", "a", "u"], true);
    pair.same(&["set-url", "a", "new", "u2"], true);
    pair.same(&["-v"], true);
}

#[test]
fn a_remote_without_a_url_prints_as_git_does() {
    let pair = Pair::new("[remote \"nourl\"]\n\ttagopt = --tags\n", &[]);
    pair.same(&["-v"], true);
    pair.same(&["get-url", "nourl"], true);
    pair.same(&["get-url", "--push", "nourl"], true);
}

fn tracked_setup() -> Vec<&'static [&'static str]> {
    vec![
        &["remote", "add", "origin", "u"],
        &["remote", "add", "other", "o"],
        &[
            "update-ref",
            "--create-reflog",
            "refs/remotes/origin/main",
            "HEAD",
        ],
        &[
            "update-ref",
            "--create-reflog",
            "refs/remotes/origin/feat/a",
            "HEAD",
        ],
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
        &[
            "update-ref",
            "--create-reflog",
            "refs/remotes/other/main",
            "HEAD",
        ],
        &["config", "branch.main.remote", "origin"],
        &["config", "branch.main.merge", "refs/heads/main"],
        &["config", "branch.dev.remote", "other"],
        &["config", "branch.dev.merge", "refs/heads/dev"],
        &["config", "branch.rb.remote", "."],
        &["config", "branch.rb.merge", "refs/heads/main"],
        &["config", "branch.main.pushRemote", "origin"],
        &["config", "remote.pushDefault", "origin"],
    ]
}

#[test]
fn removing_a_remote_deletes_its_refs_reflogs_and_tracking() {
    let pair = Pair::new("", &tracked_setup());
    pair.same(&["remove", "nope"], true);
    pair.same(&["rm", "origin"], true);
    pair.same(&["-v"], true);
    pair.same(&["remove", "other"], true);
    pair.same(&["remove", "origin"], true);
}

#[test]
fn renaming_a_remote_moves_config_refs_and_reflogs() {
    let pair = Pair::new("", &tracked_setup());
    for (args, err) in [
        (vec!["rename", "nope", "x"], true),
        (vec!["rename", "nope", "bad name"], true),
        (vec!["rename", "origin", "other"], true),
        (vec!["rename", "origin", "origin"], true),
        (vec!["rename", "origin", "bad name"], true),
        (vec!["rename", "origin", "neworigin"], true),
        (vec!["-v"], true),
        (vec!["rename", "neworigin", "a/b"], true),
        (vec!["rename", "other"], false),
    ] {
        pair.same(&args, err);
    }
}

#[test]
fn renaming_keeps_odd_config_and_warns_about_refspecs_it_cannot_move() {
    let pair = Pair::new(
        "# top comment\n[remote \"a\"]\n\turl = u1 # c\n\tpushurl = p\n\tfetch = +refs/heads/*:refs/remotes/a/*\n\tfetch = +refs/heads/x:refs/remotes/a/y\n\tfetch = +refs/heads/q:refs/other/q\n\tfetch = ^refs/heads/n\n\ttagopt = --no-tags\n\tpush = refs/heads/m\n[remote \"a\"]\n\turl = u2\n[branch \"b\"]\n\tremote = a\n\tmerge = refs/heads/z\n[remote \"a\"]\n\tprune = true\n",
        &[&["update-ref", "--create-reflog", "refs/remotes/a/x", "HEAD"]],
    );
    pair.same(&["rename", "a", "b"], true);
    pair.same(&["-v"], true);
    pair.same(&["remove", "b"], true);
}

#[test]
fn removing_cleans_sections_the_way_git_does() {
    let pair = Pair::new(
        "# keep\n[remote \"o\"]\n\turl = u\n\n[branch \"m\"]\n\tremote = o\n\n# note\n[branch \"n\"]\n\tremote = o\n\trebase = true\n[core]\n\tx = 1\n",
        &[],
    );
    pair.same(&["remove", "o"], true);
}

#[test]
fn a_packed_remote_branch_is_removed_too() {
    let pair = Pair::new(
        "",
        &[
            &["remote", "add", "o", "u"],
            &["update-ref", "refs/remotes/o/main", "HEAD"],
            &["pack-refs", "--all"],
        ],
    );
    pair.same(&["rm", "o"], true);
}

#[test]
fn tracking_survives_a_branch_rename_through_the_shared_writer() {
    let pair = Pair::new(
        "",
        &[
            &["remote", "add", "o", "u"],
            &["config", "branch.dev.remote", "o"],
            &["config", "branch.dev.merge", "refs/heads/dev"],
            &["branch", "dev"],
        ],
    );
    for args in [
        vec!["branch", "-m", "dev", "next"],
        vec!["branch", "-D", "next"],
    ] {
        let g = git_cmd(&pair.home, &pair.git, &args);
        let p = pair.pyn_cmd(&args);
        assert_eq!(p.status.code(), g.status.code(), "{args:?}");
        pair.same_disk(&format!("{args:?}"));
    }
}

#[test]
fn a_pyn_server_home_has_one_remote_named_origin() {
    let env = common::start();
    let home = env.dir("home");
    assert_eq!(env.ok(&home, "alice", &["remote"]), "origin\n");
    let url = env.source(common::REPO);
    assert_eq!(
        env.ok(&home, "alice", &["remote", "-v"]),
        format!("origin\t{url} (fetch)\norigin\t{url} (push)\n")
    );
    for args in [
        &["remote", "add", "x", "u"][..],
        &["remote", "rename", "origin", "o"],
    ] {
        let out = env.run(&home, "alice", args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("exactly one home") && err.contains("pyn config set"),
            "{err}"
        );
    }
}

#[test]
fn a_pyn_server_home_without_a_repository_says_how_to_set_one() {
    let env = common::start_empty();
    let home = env.dir("home");
    let out = env.run(&home, "alice", &["remote", "-v"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pyn config set repo"));
}

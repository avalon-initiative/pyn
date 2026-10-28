//! `pyn rebase`, `cherry-pick` and `revert` with `-s`/`--strategy` and `-X`, compared with real git on a copy of the same
//! repository: exit status class and the resulting state. Real git is the oracle for meaning only. Needs `git` on the PATH.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::{copy_tree, snapshot};

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

/// Two copies of one repository: one for git, one for pyn.
struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl Pair {
    fn new(format: &str, script: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let git = root.join("git");
        fs::create_dir_all(&git).unwrap();
        let pair = Self {
            _tmp: tmp,
            pyn: root.join("pyn"),
            git,
            home,
        };
        let init = format!(
            "git init -q -b main --object-format={format} .\n\
             git config gc.auto 0\ngit config maintenance.auto false\ngit config merge.conflictStyle diff3\n{script}"
        );
        let out = Command::new("sh")
            .args(["-e", "-c", &init])
            .current_dir(&pair.git)
            .env("HOME", &pair.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        copy_tree(&pair.git, &pair.pyn);
        pair
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_SEQUENCE_EDITOR",
                "sed -i 's/^pick/edit/;s/^edit \\(.*\\) c3$/pick \\1 c3/'",
            )
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn pyn_cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .env(
                "GIT_SEQUENCE_EDITOR",
                "sed -i 's/^pick/edit/;s/^edit \\(.*\\) c3$/pick \\1 c3/'",
            )
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    fn both_sh(&self, script: &str) {
        for dir in [&self.git, &self.pyn] {
            let out = Command::new("sh")
                .args(["-e", "-c", script])
                .current_dir(dir)
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .envs(IDENTITY)
                .output()
                .unwrap();
            assert!(out.status.success(), "{script}: {}", text(&out.stderr));
        }
    }

    /// The result a person can see: the commit contents, the index, the files and what is left in progress.
    fn state(&self, dir: &Path) -> Vec<String> {
        let g = |args: &[&str]| text(&self.git_in(dir, args).stdout);
        let mut out = vec![
            format!("tree {}", g(&["rev-parse", "HEAD^{tree}"]).trim()),
            format!("commits {}", g(&["rev-list", "--count", "HEAD"]).trim()),
            format!("subjects {}", g(&["log", "--format=%s"])),
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!("status\n{}", g(&["status", "--porcelain"])),
        ];
        for name in [
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "sequencer",
            "rebase-merge",
        ] {
            out.push(format!("{name} {}", dir.join(".git").join(name).exists()));
        }
        for (path, content) in snapshot(dir) {
            if path == ".git" || path.starts_with(".git/") || path.starts_with(".pyn") {
                continue;
            }
            out.push(format!("{path} {content}"));
        }
        out
    }

    /// Runs the same `git <args>` and `pyn <args>`; status class and the whole resulting state must agree.
    fn same(&self, args: &[&str]) {
        let g = self.git_in(&self.git, args);
        let p = self.pyn_cmd(args);
        let what = format!("{args:?}");
        assert_eq!(
            g.status.code(),
            p.status.code(),
            "{what}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(self.state(&self.git), self.state(&self.pyn), "{what}");
    }

    /// pyn refuses `args` with `code` and changes nothing.
    fn refused(&self, args: &[&str], code: i32) {
        let before = self.state(&self.pyn);
        let p = self.pyn_cmd(args);
        assert_eq!(p.status.code(), Some(code), "{args:?}: {}", text(&p.stderr));
        assert_eq!(before, self.state(&self.pyn), "{args:?}");
    }
}

fn each_format(script: &str, run: impl Fn(&Pair)) {
    for format in common::gitfix::FORMATS {
        run(&Pair::new(format, script));
    }
}

/// `side` (on `main`'s base) changes lines 2 and 5 of `f` in `c1`, then `g` in `c2`, then `f` line 4 in `c3`; `main`
/// changed line 2 and 4 of `f` itself. `c1` and `c3` conflict, `c2` does not. Left on `main`.
const HISTORY: &str = "printf 'a\\nb\\nc\\nd\\ne\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'a\\nB2\\nc\\nd\\nE\\n' >f; git commit -qam c1\n\
    printf 'x\\nside\\n' >g; git commit -qam c2\nprintf 'a\\nB2\\nc\\nD3\\nE\\n' >f; git commit -qam c3\n\
    git checkout -q main\nprintf 'a\\nB1\\nc\\nD1\\ne\\n' >f; echo m >h; git add .; git commit -qm main\n";

/// `c1` changes line 2 of `f` and `c2` changes the same line again, so reverting `c1` conflicts.
const REVERTABLE: &str = "printf 'a\\nb\\nc\\n' >f; git add .; git commit -qm base\n\
    printf 'a\\nB1\\nc\\n' >f; git commit -qam c1\nprintf 'a\\nB2\\nc\\n' >f; git commit -qam c2\n";

#[test]
fn rebase_options_match_git() {
    for opts in [
        &["-X", "theirs"][..],
        &["-X", "ours"],
        &["-Xtheirs"],
        &["--strategy-option=ours"],
        &["-s", "ort", "-X", "theirs"],
        &["--strategy=recursive", "-X", "ours"],
        &["-X", "ignore-all-space"],
        &["-X", "ignore-space-change", "-X", "theirs"],
        &["-X", "ours", "-X", "theirs"],
        &["-X", "no-renames", "-X", "theirs"],
        &["-s", "recursive"],
        &["-s", "ort", "-s", "recursive", "-X", "theirs"],
    ] {
        each_format(HISTORY, |p| {
            p.both_sh("git checkout -q side");
            let mut args = vec!["rebase"];
            args.extend(opts);
            args.push("main");
            p.same(&args);
        });
    }
}

#[test]
fn a_rebase_stopped_by_a_conflict_keeps_its_strategy_for_the_rest() {
    each_format(HISTORY, |p| {
        p.both_sh("git checkout -q side");
        p.same(&["rebase", "-i", "-X", "theirs", "main"]);
        p.both_sh("git status --short >/dev/null");
        p.same(&["rebase", "--continue"]);
        p.same(&["rebase", "--continue"]);
    });
}

#[test]
fn rebase_strategies_pyn_does_not_build_are_usage_errors() {
    let p = Pair::new("sha1", HISTORY);
    p.both_sh("git checkout -q side");
    for opts in [
        &["-s", "ours"][..],
        &["-s", "resolve"],
        &["-s", "octopus"],
        &["-s", "subtree"],
        &["-X", "subtree"],
        &["-X", "subtree=sub"],
        &["-X", "patience"],
        &["-X", "histogram"],
        &["-X", "diff-algorithm=patience"],
        &["-X", "find-renames"],
        &["-X", "find-renames=80"],
        &["-X", "rename-threshold=80"],
    ] {
        let mut args = vec!["rebase"];
        args.extend(opts);
        args.push("main");
        p.refused(&args, 129);
    }
}

#[test]
fn an_unknown_strategy_or_option_stops_a_rebase_before_anything_changes() {
    let p = Pair::new("sha1", HISTORY);
    p.both_sh("git checkout -q side");
    p.refused(&["rebase", "-s", "bogus", "main"], 1);
    p.refused(&["rebase", "-X", "bogus", "main"], 128);
}

#[test]
fn cherry_pick_options_match_git() {
    for opts in [
        &["-X", "theirs"][..],
        &["-X", "ours"],
        &["-Xtheirs"],
        &["--strategy-option=ours"],
        &["--strategy=ort", "-X", "theirs"],
        &["--strategy=recursive", "-X", "ours"],
        &["-X", "ignore-all-space"],
        &["-X", "no-renames"],
        &["--strategy=recursive"],
    ] {
        for picked in [&["side~2"][..], &["side~1"], &["side"], &["main..side"]] {
            each_format(HISTORY, |p| {
                let mut args = vec!["cherry-pick"];
                args.extend(opts);
                args.extend(picked);
                p.same(&args);
            });
        }
    }
}

#[test]
fn cherry_pick_keeps_its_options_across_a_stop() {
    each_format(HISTORY, |p| {
        p.same(&["cherry-pick", "-X", "ours", "side~2", "side"]);
        p.same(&["cherry-pick", "--skip"]);
        p.same(&["cherry-pick", "--continue"]);
    });
    each_format(HISTORY, |p| {
        p.same(&["cherry-pick", "-n", "-X", "theirs", "side~2"]);
    });
}

#[test]
fn cherry_pick_takes_signoff_with_a_strategy() {
    each_format(HISTORY, |p| {
        p.same(&["cherry-pick", "-s", "-X", "theirs", "side~2"]);
    });
}

#[test]
fn cherry_pick_strategies_pyn_does_not_build_are_usage_errors() {
    let p = Pair::new("sha1", HISTORY);
    for opts in [
        &["--strategy=ours"][..],
        &["--strategy=resolve"],
        &["-X", "subtree"],
        &["-X", "patience"],
        &["-X", "find-renames=50"],
    ] {
        let mut args = vec!["cherry-pick"];
        args.extend(opts);
        args.push("side");
        p.refused(&args, 129);
    }
    p.refused(&["cherry-pick", "-X", "bogus", "side"], 128);
}

#[test]
fn revert_options_match_git() {
    for opts in [
        &["-X", "theirs"][..],
        &["-X", "ours"],
        &["--strategy-option=theirs"],
        &["--strategy=recursive", "-X", "theirs"],
        &["-X", "ignore-space-change"],
    ] {
        each_format(REVERTABLE, |p| {
            let mut args = vec!["revert"];
            args.extend(opts);
            args.push("HEAD~1");
            p.same(&args);
        });
    }
}

#[test]
fn revert_strategies_pyn_does_not_build_are_usage_errors() {
    let p = Pair::new("sha1", REVERTABLE);
    p.refused(&["revert", "--strategy=ours", "HEAD~1"], 129);
    p.refused(&["revert", "-X", "patience", "HEAD~1"], 129);
}

#[test]
fn a_workspace_without_git_refuses_the_options() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
        .args(["cherry-pick", "-X", "theirs", "HEAD"])
        .current_dir(tmp.path())
        .env("HOME", tmp.path())
        .env("PYN_CONFIG_DIR", tmp.path().join("pyn-config"))
        .env_remove("PYN_SERVER")
        .env_remove("PYN_REPO")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

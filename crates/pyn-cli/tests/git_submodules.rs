//! Gitlink status and diff compared with real git; lifecycle operations only build the fixtures.

#[path = "common/gitfix.rs"]
mod gitfix;
#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _tmp: TempDir,
    repo: PathBuf,
    sub: PathBuf,
    second: String,
    config: String,
}

impl Fixture {
    fn new(format: &str, gitfile: bool) -> Self {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        let sub = repo.join("sub");
        init(&repo, format, false);
        init(&sub, format, false);
        let first = commit(&sub, "file", b"one\n", "one");
        let second = commit(&sub, "file", b"two\n", "two");
        run(&sub, &["checkout", "-q", &first]);
        fs::write(repo.join(".gitmodules"), Self::modules(None)).unwrap();
        run(&repo, &["add", ".gitmodules"]);
        run(
            &repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{first},sub"),
            ],
        );
        run(&repo, &["commit", "-qm", "base"]);
        if gitfile {
            fs::create_dir_all(repo.join(".git/modules")).unwrap();
            fs::rename(sub.join(".git"), repo.join(".git/modules/library")).unwrap();
            fs::write(sub.join(".git"), "gitdir: ../.git/modules/library\n").unwrap();
        }
        let config = fs::read_to_string(repo.join(".git/config")).unwrap();
        Self {
            _tmp: tmp,
            repo,
            sub,
            second,
            config,
        }
    }

    fn modules(ignore: Option<&str>) -> String {
        format!(
            "[submodule \"library\"]\n path = sub\n url = ../source\n{}",
            ignore.map_or(String::new(), |v| format!(" ignore = {v}\n"))
        )
    }

    fn configure(&self, default: Option<&str>, local: Option<&str>, modules: Option<&str>) {
        fs::write(
            self.repo.join(".git/config"),
            format!(
                "{}{}{}",
                self.config,
                default.map_or(String::new(), |v| format!(
                    "\n[diff]\n ignoreSubmodules = {v}\n"
                )),
                local.map_or(String::new(), |v| format!(
                    "\n[submodule \"library\"]\n ignore = {v}\n"
                ))
            ),
        )
        .unwrap();
        fs::write(self.repo.join(".gitmodules"), Self::modules(modules)).unwrap();
    }

    fn nested(&self, format: &str) -> PathBuf {
        let nested = self.sub.join("nested");
        init(&nested, format, false);
        let first = commit(&nested, "file", b"one\n", "one");
        commit(&nested, "file", b"two\n", "two");
        run(&nested, &["checkout", "-q", &first]);
        fs::write(
            self.sub.join(".gitmodules"),
            "[submodule \"inner\"]\n path = nested\n url = ../inner\n",
        )
        .unwrap();
        run(&self.sub, &["add", ".gitmodules"]);
        run(
            &self.sub,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{first},nested"),
            ],
        );
        run(&self.sub, &["commit", "-qm", "record nested"]);
        let head = run(&self.sub, &["rev-parse", "HEAD"]);
        run(
            &self.repo,
            &[
                "update-index",
                "--cacheinfo",
                &format!("160000,{},sub", head.trim()),
            ],
        );
        run(&self.repo, &["commit", "-qm", "record nested checkout"]);
        nested
    }

    fn state(&self, state: &str) {
        match state {
            "clean" => {}
            "forward" => {
                run(&self.sub, &["checkout", "-q", "main"]);
            }
            "backward" => {
                run(
                    &self.repo,
                    &[
                        "update-index",
                        "--cacheinfo",
                        &format!("160000,{},sub", self.second),
                    ],
                );
                run(&self.repo, &["commit", "-qm", "record second"]);
            }
            "detached" => {
                run(&self.sub, &["checkout", "-q", "--detach", &self.second]);
            }
            "tracked" => {
                fs::write(self.sub.join("file"), "dirty\n").unwrap();
            }
            "index" => {
                fs::write(self.sub.join("file"), "staged\n").unwrap();
                run(&self.sub, &["add", "file"]);
            }
            "untracked" => {
                fs::write(self.sub.join("new"), "new\n").unwrap();
            }
            "staged" => {
                run(&self.sub, &["checkout", "-q", "main"]);
                run(
                    &self.repo,
                    &[
                        "update-index",
                        "--cacheinfo",
                        &format!("160000,{},sub", self.second),
                    ],
                );
            }
            "missing" => {
                fs::rename(&self.sub, self.repo.parent().unwrap().join("saved")).unwrap();
            }
            "uninitialized" => {
                fs::rename(
                    self.sub.join(".git"),
                    self.repo.parent().unwrap().join("saved-git"),
                )
                .unwrap();
                fs::remove_file(self.sub.join("file")).unwrap();
            }
            other => panic!("unknown state {other}"),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pyn"));
        command
            .current_dir(&self.repo)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.repo.parent().unwrap().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TZ", "UTC");
        command
    }

    fn pyn(&self, args: &[&str]) -> std::process::Output {
        self.command(args).output().unwrap()
    }

    fn same(&self, args: &[&str]) {
        self.compare(args, 0);
    }

    fn compare(&self, args: &[&str], code: i32) {
        // Neither inspecting the submodule nor the oracle may refresh its index.
        let before = gitfix::snapshot(&self.sub_git_dir());
        let worktree = self.sub.is_dir().then(|| gitfix::snapshot(&self.sub));
        let want = git(&self.repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("TZ", "UTC")
            .args(args)
            .output()
            .unwrap();
        let got = self.pyn(args);
        assert_eq!(
            want.status.code(),
            Some(code),
            "oracle failed: {}",
            String::from_utf8_lossy(&want.stderr)
        );
        assert_eq!(
            got.status.code(),
            want.status.code(),
            "{args:?}: {}",
            String::from_utf8_lossy(&got.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&got.stdout),
            String::from_utf8_lossy(&want.stdout),
            "{args:?}"
        );
        assert_eq!(
            gitfix::snapshot(&self.sub_git_dir()),
            before,
            "inspection wrote to submodule git directory"
        );
        assert_eq!(
            self.sub.is_dir().then(|| gitfix::snapshot(&self.sub)),
            worktree,
            "inspection wrote to submodule worktree"
        );
    }

    fn sub_git_dir(&self) -> PathBuf {
        let dir = self.repo.join(".git/modules/library");
        if dir.is_dir() {
            dir
        } else if self.sub.join(".git").is_dir() {
            self.sub.join(".git")
        } else if self.repo.parent().unwrap().join("saved/.git").is_dir() {
            self.repo.parent().unwrap().join("saved/.git")
        } else {
            self.repo.parent().unwrap().join("saved-git")
        }
    }

    fn table(&self) {
        let before = gitfix::snapshot(&self.sub_git_dir());
        let worktree = self.sub.is_dir().then(|| gitfix::snapshot(&self.sub));
        let want = git(&self.repo)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .args(["status", "--porcelain", "--", "sub"])
            .output()
            .unwrap();
        assert!(
            want.status.success(),
            "{}",
            String::from_utf8_lossy(&want.stderr)
        );
        let want = String::from_utf8(want.stdout).unwrap();
        let got = self.pyn(&["status", "sub"]);
        assert!(
            got.status.success(),
            "{}",
            String::from_utf8_lossy(&got.stderr)
        );
        assert_eq!(
            gitfix::porcelain(&String::from_utf8_lossy(&got.stdout)),
            gitfix::sorted_lines(&want)
        );
        assert_eq!(gitfix::snapshot(&self.sub_git_dir()), before);
        assert_eq!(
            self.sub.is_dir().then(|| gitfix::snapshot(&self.sub)),
            worktree
        );
    }
}

const STATES: [&str; 10] = [
    "clean",
    "forward",
    "backward",
    "detached",
    "tracked",
    "index",
    "untracked",
    "staged",
    "missing",
    "uninitialized",
];

fn each(check: impl Fn(&Fixture)) {
    for format in FORMATS {
        for gitfile in [false, true] {
            check(&Fixture::new(format, gitfile));
        }
    }
}

#[test]
fn short_status_gitlinks() {
    for state in STATES {
        each(|f| {
            f.state(state);
            f.same(&["status", "-s", "--", "sub"]);
        });
    }
}

#[test]
fn porcelain_status_gitlinks() {
    for state in STATES {
        each(|f| {
            f.state(state);
            f.same(&["status", "--porcelain", "--", "sub"]);
        });
    }
}

#[test]
fn default_status_gitlinks() {
    for state in STATES {
        each(|f| {
            f.state(state);
            f.table();
        });
    }
}

#[test]
fn worktree_diff_gitlinks() {
    for state in STATES {
        each(|f| {
            f.state(state);
            f.same(&["diff", "--", "sub"]);
        });
    }
}

#[test]
fn revision_worktree_diff_gitlinks() {
    for state in STATES {
        each(|f| {
            f.state(state);
            f.same(&["diff", "HEAD", "--", "sub"]);
        });
    }
}

#[test]
fn cached_diff_gitlinks() {
    each(|f| {
        f.state("staged");
        f.same(&["diff", "--cached"]);
        f.same(&["diff", "--staged", "HEAD"]);
    });
}

fn record_move(f: &Fixture) {
    f.state("staged");
    run(&f.repo, &["commit", "-qm", "moved"]);
}

#[test]
fn tree_diff_gitlinks() {
    each(|f| {
        record_move(f);
        f.same(&["diff", "HEAD~1", "HEAD"]);
    });
}

#[test]
fn show_gitlinks() {
    each(|f| {
        record_move(f);
        f.same(&["show", "--format=oneline"]);
        f.configure(None, Some("all"), None);
        f.same(&["show", "--format=oneline"]);
    });
}

#[test]
fn log_patch_gitlinks() {
    each(|f| {
        record_move(f);
        f.same(&["log", "-n", "1", "--format=oneline", "-p"]);
        f.configure(None, Some("all"), None);
        f.same(&["log", "-n", "1", "--format=oneline", "-p"]);
    });
}

#[test]
fn dirty_submodule_short_status() {
    each(|f| {
        f.state("tracked");
        f.state("untracked");
        f.same(&["status", "-s"]);
    });
}

#[test]
fn untracked_submodule_short_status() {
    each(|f| {
        f.state("untracked");
        f.same(&["status", "-s"]);
    });
}

#[test]
fn dirty_gitlink_diff() {
    each(|f| {
        f.state("tracked");
        f.same(&["diff"]);
        run(&f.sub, &["checkout", "--", "file"]);
        f.state("detached");
        f.state("tracked");
        f.same(&["diff"]);
    });
}

#[test]
fn untracked_gitlink_diff_config() {
    each(|f| {
        f.state("untracked");
        f.configure(None, Some("none"), None);
        f.same(&["diff"]);
    });
}

#[test]
fn ignore_configuration_status() {
    for state in ["tracked", "index", "untracked", "forward"] {
        each(|f| {
            f.state(state);
            for default in [None, Some("all"), Some("dirty")] {
                for local in [
                    None,
                    Some("none"),
                    Some("untracked"),
                    Some("dirty"),
                    Some("all"),
                ] {
                    f.configure(default, local, None);
                    f.same(&["status", "-s", "--", "sub"]);
                    f.same(&["status", "--porcelain", "--", "sub"]);
                    f.table();
                }
            }
        });
    }
}

#[test]
fn ignore_configuration_diff() {
    for state in ["tracked", "index", "untracked", "forward"] {
        each(|f| {
            f.state(state);
            for default in [None, Some("all"), Some("dirty")] {
                for local in [
                    None,
                    Some("none"),
                    Some("untracked"),
                    Some("dirty"),
                    Some("all"),
                ] {
                    f.configure(default, local, None);
                    f.same(&["diff", "--", "sub"]);
                    f.same(&["diff", "HEAD", "--", "sub"]);
                }
            }
        });
    }
}

#[test]
fn gitmodules_and_config_precedence() {
    for state in ["tracked", "untracked", "forward"] {
        each(|f| {
            f.state(state);
            for modules in ["none", "untracked", "dirty", "all"] {
                for local in [
                    None,
                    Some("none"),
                    Some("untracked"),
                    Some("dirty"),
                    Some("all"),
                ] {
                    f.configure(Some("all"), local, Some(modules));
                    f.same(&["status", "-s", "--", "sub"]);
                    f.table();
                    f.same(&["diff", "--", "sub"]);
                }
            }
        });
    }
}

#[test]
fn staged_gitlinks_under_ignore_all() {
    each(|f| {
        f.state("staged");
        for (default, local) in [
            (Some("all"), None),
            (None, Some("all")),
            (Some("all"), Some("none")),
        ] {
            f.configure(default, local, None);
            f.same(&["status", "-s", "--", "sub"]);
            f.table();
            f.same(&["diff", "--cached", "--", "sub"]);
        }
    });
}

#[test]
fn deleted_gitlinks_diff() {
    each(|f| {
        run(&f.repo, &["update-index", "--force-remove", "sub"]);
        f.same(&["diff", "--cached", "--", "sub"]);
    });
}

#[test]
fn added_gitlinks_diff() {
    each(|f| {
        run(&f.repo, &["update-index", "--force-remove", "sub"]);
        run(&f.repo, &["commit", "-qm", "remove gitlink"]);
        run(
            &f.repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},sub", f.second),
            ],
        );
        f.same(&["diff", "--cached", "--", "sub"]);
    });
}

#[test]
fn malformed_submodule_gitfile() {
    for format in FORMATS {
        let f = Fixture::new(format, true);
        fs::write(f.sub.join(".git"), "invalid gitfile\n").unwrap();
        f.compare(&["status", "-s"], 128);
        f.compare(&["diff"], 128);
    }
}

fn record_stash(f: &Fixture) {
    let base = run(&f.repo, &["rev-parse", "HEAD"]);
    let tree = run(&f.repo, &["write-tree"]);
    let index = run(
        &f.repo,
        &["commit-tree", tree.trim(), "-p", base.trim(), "-m", "index"],
    );
    let untracked = run(&f.repo, &["commit-tree", tree.trim(), "-m", "untracked"]);
    let stash = run(
        &f.repo,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            base.trim(),
            "-p",
            index.trim(),
            "-p",
            untracked.trim(),
            "-m",
            "stash",
        ],
    );
    run(
        &f.repo,
        &[
            "update-ref",
            "--create-reflog",
            "-m",
            "stash",
            "refs/stash",
            stash.trim(),
        ],
    );
}

#[test]
fn stash_show_gitlinks() {
    each(|f| {
        f.state("staged");
        record_stash(f);
        f.same(&["stash", "show", "-p"]);
        f.same(&["stash", "show", "--only-untracked", "-p"]);
        f.configure(None, Some("all"), None);
        f.same(&["stash", "show", "-p"]);
        f.same(&["stash", "show", "--only-untracked", "-p"]);
    });
}

#[test]
fn ignore_all_gitlink_typechange() {
    each(|f| {
        let id = run(&f.repo, &["rev-parse", "HEAD:.gitmodules"]);
        run(
            &f.repo,
            &[
                "update-index",
                "--cacheinfo",
                &format!("100644,{},sub", id.trim()),
            ],
        );
        f.configure(None, Some("all"), None);
        f.same(&["diff", "--cached", "--", "sub"]);
        f.same(&["diff", "--cached", "--name-status", "--", "sub"]);
    });
}

#[test]
fn merge_stat_gitlinks() {
    for ignore in [None, Some("all")] {
        each(|f| {
            f.configure(None, ignore, None);
            record_move(f);
            run(&f.repo, &["branch", "side"]);
            run(&f.repo, &["reset", "--hard", "HEAD~1"]);
            f.state("uninitialized");
            let oracle = f.repo.parent().unwrap().join("oracle");
            gitfix::copy_tree(&f.repo, &oracle);
            let want = run(&oracle, &["merge", "side"]);
            let got = f.pyn(&["merge", "side"]);
            assert!(
                got.status.success(),
                "{}",
                String::from_utf8_lossy(&got.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&got.stdout), want);
        });
    }
}

#[test]
fn nested_untracked_status() {
    for format in FORMATS {
        for gitfile in [false, true] {
            let f = Fixture::new(format, gitfile);
            let nested = f.nested(format);
            f.same(&["status", "-s"]);
            fs::write(nested.join("new"), "untracked\n").unwrap();
            f.same(&["status", "-s"]);
            f.same(&["status", "--porcelain"]);
            f.table();
            fs::write(nested.join("file"), "tracked\n").unwrap();
            f.same(&["status", "-s"]);
            run(&nested, &["add", "file"]);
            f.same(&["status", "-s"]);
            run(&nested, &["commit", "-qm", "nested commit"]);
            f.same(&["status", "-s"]);
        }
    }
}

#[test]
fn nested_untracked_default_diff() {
    for format in FORMATS {
        for gitfile in [false, true] {
            let f = Fixture::new(format, gitfile);
            let nested = f.nested(format);
            fs::write(nested.join("new"), "untracked\n").unwrap();
            assert!(!run(&f.repo, &["status", "-s"]).is_empty());
            assert!(run(&f.repo, &["diff"]).is_empty());
            f.same(&["status", "-s"]);
            f.same(&["diff"]);
            f.same(&["diff", "HEAD"]);
            fs::write(nested.join("file"), "tracked\n").unwrap();
            f.same(&["diff"]);
            f.same(&["diff", "HEAD"]);
        }
    }
}

#[test]
fn nested_untracked_ignore_config() {
    for format in FORMATS {
        for gitfile in [false, true] {
            let f = Fixture::new(format, gitfile);
            let nested = f.nested(format);
            fs::write(nested.join("new"), "untracked\n").unwrap();
            for ignore in ["untracked", "none", "dirty", "all"] {
                f.configure(None, Some(ignore), None);
                f.same(&["status", "-s"]);
                f.same(&["status", "--porcelain"]);
                f.table();
                f.same(&["diff"]);
                f.same(&["diff", "HEAD"]);
            }
            fs::write(nested.join("file"), "tracked\n").unwrap();
            for ignore in ["untracked", "none", "dirty", "all"] {
                f.configure(None, Some(ignore), None);
                f.same(&["status", "-s"]);
                f.same(&["diff"]);
            }
            run(&nested, &["checkout", "--", "file"]);
            run(&nested, &["checkout", "-q", "main"]);
            for ignore in ["untracked", "none", "dirty", "all"] {
                f.configure(None, Some(ignore), None);
                f.same(&["status", "-s"]);
                f.same(&["diff"]);
            }
        }
    }
}

#[test]
fn merge_stat_preserves_gitlink_kind() {
    for replacement in ["file", "gitlink"] {
        each(|f| {
            let id = run(&f.repo, &["rev-parse", "HEAD:sub"]);
            f.state("uninitialized");
            run(&f.repo, &["checkout", "-qb", "side"]);
            run(&f.repo, &["update-index", "--force-remove", "sub"]);
            if replacement == "file" {
                fs::write(
                    f.repo.join("note"),
                    format!("Subproject commit {}\n", id.trim()),
                )
                .unwrap();
                run(&f.repo, &["add", "note"]);
            } else {
                run(
                    &f.repo,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{},renamed", id.trim()),
                    ],
                );
            }
            run(&f.repo, &["commit", "-qm", "replace gitlink"]);
            run(&f.repo, &["checkout", "-q", "main"]);
            let oracle = f.repo.parent().unwrap().join("oracle");
            gitfix::copy_tree(&f.repo, &oracle);
            let want = run(&oracle, &["merge", "side"]);
            let got = f.pyn(&["merge", "side"]);
            assert!(
                got.status.success(),
                "{}",
                String::from_utf8_lossy(&got.stderr)
            );
            assert_eq!(String::from_utf8_lossy(&got.stdout), want, "{replacement}");
        });
    }
}

#[test]
fn missing_gitmodules_source_fallback() {
    for source in ["index", "HEAD"] {
        each(|f| {
            f.configure(None, None, Some("all"));
            run(&f.repo, &["add", ".gitmodules"]);
            if source == "HEAD" {
                run(&f.repo, &["commit", "-qm", "record module settings"]);
                run(&f.repo, &["update-index", "--force-remove", ".gitmodules"]);
            }
            f.state("tracked");
            for local in [None, Some("none"), Some("all")] {
                f.configure(None, local, None);
                fs::remove_file(f.repo.join(".gitmodules")).unwrap();
                f.same(&["status", "-s", "--", "sub"]);
                f.same(&["status", "--porcelain", "--", "sub"]);
                f.table();
                f.same(&["diff", "--", "sub"]);
                f.same(&["diff", "HEAD", "--", "sub"]);
            }
        });
    }
}

#[test]
fn unborn_submodule_content() {
    for state in ["staged", "untracked", "empty"] {
        each(|f| {
            run(&f.sub, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);
            if state != "staged" {
                run(&f.sub, &["read-tree", "--empty"]);
            }
            if state == "empty" {
                fs::remove_file(f.sub.join("file")).unwrap();
            }
            for ignore in [
                None,
                Some("none"),
                Some("untracked"),
                Some("dirty"),
                Some("all"),
            ] {
                f.configure(None, ignore, None);
                f.same(&["status", "-s", "--", "sub"]);
                f.same(&["status", "--porcelain", "--", "sub"]);
                f.table();
                f.same(&["diff", "--", "sub"]);
                f.same(&["diff", "HEAD", "--", "sub"]);
            }
        });
    }
}

#[test]
fn unmerged_gitmodules_does_not_use_head() {
    use std::io::Write;
    use std::process::Stdio;

    each(|f| {
        f.configure(None, None, Some("all"));
        run(&f.repo, &["add", ".gitmodules"]);
        run(&f.repo, &["commit", "-qm", "record module settings"]);
        let id = run(&f.repo, &["rev-parse", "HEAD:.gitmodules"]);
        let conflict = |path: &str| {
            run(&f.repo, &["update-index", "--force-remove", path]);
            let entries: String = (1..=3)
                .map(|stage| format!("100644 {} {stage}\t{path}\n", id.trim()))
                .collect();
            let mut child = git(&f.repo)
                .args(["update-index", "--index-info"])
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(entries.as_bytes())
                .unwrap();
            assert!(child.wait().unwrap().success());
        };
        conflict(".gitmodules");
        fs::remove_file(f.repo.join(".gitmodules")).unwrap();
        f.state("tracked");
        f.same(&["status", "-s", "--", "sub"]);
        f.same(&["status", "--porcelain", "--", "sub"]);
        f.table();
        f.same(&["diff", "--", "sub"]);
        f.same(&["diff", "HEAD", "--", "sub"]);

        run(&f.repo, &["update-index", "--force-remove", ".gitmodules"]);
        run(
            &f.repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{},.gitmodules", id.trim()),
            ],
        );
        conflict("other");
        f.same(&["status", "-s", "--", "sub"]);
        f.table();
        f.same(&["diff", "--", "sub"]);
    });
}

#[test]
fn add_modified_gitlinks() {
    for ignore in [None, Some("all")] {
        for args in [
            vec!["add", "."],
            vec!["add", "-A"],
            vec!["add", "sub"],
            vec!["add", "-u"],
            vec!["add", "-f", "."],
        ] {
            each(|f| {
                f.state("backward");
                f.configure(None, ignore, None);
                fs::write(f.repo.join("newf"), "new\n").unwrap();
                let oracle = f.repo.parent().unwrap().join("oracle");
                gitfix::copy_tree(&f.repo, &oracle);
                run(&oracle, &args);
                let want = run(
                    &oracle,
                    &[
                        "-c",
                        "submodule.library.ignore=none",
                        "diff",
                        "--cached",
                        "--name-status",
                    ],
                );
                eprintln!("git {args:?}: {want:?}");
                assert!(want.contains("M\tsub\n"), "{want}");
                let got = f.pyn(&args);
                assert_eq!(
                    got.status.code(),
                    Some(0),
                    "{args:?}: {}",
                    String::from_utf8_lossy(&got.stderr)
                );
                assert_eq!(
                    run(
                        &f.repo,
                        &[
                            "-c",
                            "submodule.library.ignore=none",
                            "diff",
                            "--cached",
                            "--name-status"
                        ]
                    ),
                    want
                );
                assert_eq!(run(&f.repo, &["write-tree"]), run(&oracle, &["write-tree"]));
            });
        }
    }
}

#[test]
fn commit_all_modified_gitlinks() {
    for ignore in [None, Some("all")] {
        each(|f| {
            f.state("backward");
            f.configure(None, ignore, None);
            fs::write(f.repo.join("newf"), "new\n").unwrap();
            run(&f.repo, &["add", "newf"]);
            run(&f.repo, &["config", "user.name", "t"]);
            run(&f.repo, &["config", "user.email", "t@example.com"]);
            let oracle = f.repo.parent().unwrap().join("oracle");
            gitfix::copy_tree(&f.repo, &oracle);
            run(&oracle, &["commit", "-qam", "stage tracked"]);
            let want = run(
                &oracle,
                &[
                    "-c",
                    "submodule.library.ignore=none",
                    "diff-tree",
                    "--no-commit-id",
                    "--name-status",
                    "-r",
                    "HEAD",
                ],
            );
            eprintln!("git commit -a: {want:?}");
            assert!(
                want.contains("M\tsub\n") && want.contains("A\tnewf\n"),
                "{want}"
            );
            let got = f.pyn(&["commit", "-qam", "stage tracked"]);
            assert_eq!(
                got.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&got.stderr)
            );
            assert_eq!(
                run(&f.repo, &["rev-parse", "HEAD^{tree}"]),
                run(&oracle, &["rev-parse", "HEAD^{tree}"])
            );
        });
    }
}

fn gitlink_stats(option: &str) {
    for state in ["clean", "forward"] {
        each(|f| {
            f.state(state);
            f.state("tracked");
            f.state("untracked");
            let want = run(&f.repo, &["diff", option, "--", "sub"]);
            eprintln!("git diff {option}, {state}: {want:?}");
            assert!(!want.is_empty());
            f.same(&["diff", option, "--", "sub"]);
        });
    }
}

#[test]
fn invalid_submodule_ignore_warns() {
    each(|f| {
        f.state("tracked");
        for (default, local, modules) in [
            (None, None, Some("bogus")),
            (Some("all"), None, Some("bogus")),
        ] {
            f.configure(default, local, modules);
            for args in [vec!["status", "-s", "--", "sub"], vec!["diff", "--", "sub"]] {
                let want = git(&f.repo).args(&args).output().unwrap();
                eprintln!(
                    "git {args:?} ({default:?}, {local:?}, {modules:?}): status={}, stdout={:?}, stderr={:?}",
                    want.status,
                    String::from_utf8_lossy(&want.stdout),
                    String::from_utf8_lossy(&want.stderr)
                );
                assert_eq!(want.status.code(), Some(0));
                let got = f.pyn(&args);
                assert_eq!(
                    got.status.code(),
                    want.status.code(),
                    "{}",
                    String::from_utf8_lossy(&got.stderr)
                );
                assert_eq!(got.stdout, want.stdout);
                assert_eq!(got.stderr, want.stderr);
            }
        }
    });
}

#[test]
fn invalid_diff_ignore_is_fatal() {
    each(|f| {
        f.state("tracked");
        for (default, local) in [
            (Some("bogus"), None),
            (None, Some("bogus")),
            (Some("bogus"), Some("all")),
        ] {
            f.configure(default, local, None);
            for args in [vec!["status", "-s"], vec!["diff"]] {
                let want = git(&f.repo).args(&args).output().unwrap();
                eprintln!(
                    "git {args:?}: status={}, stderr={:?}",
                    want.status,
                    String::from_utf8_lossy(&want.stderr)
                );
                assert_eq!(want.status.code(), Some(128));
                let got = f.pyn(&args);
                assert_eq!(got.status.code(), want.status.code());
                assert_eq!(got.stdout, want.stdout);
                assert_eq!(got.stderr, want.stderr);
            }
        }
    });
}

#[test]
fn malformed_gitfile_reports_os_error_once() {
    for format in FORMATS {
        let f = Fixture::new(format, true);
        fs::write(f.sub.join(".git"), "gitdir: /etc\n").unwrap();
        for args in [vec!["status", "-s"], vec!["diff"]] {
            let want = git(&f.repo).args(&args).output().unwrap();
            eprintln!(
                "git {args:?}: status={}, stderr={:?}",
                want.status,
                String::from_utf8_lossy(&want.stderr)
            );
            let got = f.pyn(&args);
            assert_eq!(got.status.code(), want.status.code());
            let stderr = String::from_utf8_lossy(&got.stderr);
            assert_eq!(stderr.matches("(os error 2)").count(), 1, "{stderr}");
        }
    }
}

#[test]
fn dirty_only_gitlink_stat() {
    gitlink_stats("--stat");
}

#[test]
fn dirty_only_gitlink_shortstat() {
    gitlink_stats("--shortstat");
}

#[test]
fn dirty_only_gitlink_numstat() {
    gitlink_stats("--numstat");
}

fn submodule_config_reads(source: &str, args: &[&str]) {
    for format in FORMATS {
        let f = Fixture::new(format, false);
        let other = f.repo.join("other");
        gitfix::copy_tree(&f.sub, &other);
        fs::write(
            f.repo.join(".gitmodules"),
            format!(
                "{}[submodule \"other\"]\n path = other\n url = ../other\n",
                Fixture::modules(Some("none"))
            ),
        )
        .unwrap();
        let id = run(&f.sub, &["rev-parse", "HEAD"]);
        run(
            &f.repo,
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},other", id.trim()),
            ],
        );
        run(&f.repo, &["add", ".gitmodules"]);
        run(&f.repo, &["commit", "-qm", "two submodules"]);
        if args.contains(&"--cached") || args.contains(&"HEAD~1") {
            for path in ["sub", "other"] {
                run(
                    &f.repo,
                    &[
                        "update-index",
                        "--cacheinfo",
                        &format!("160000,{},{}", f.second, path),
                    ],
                );
            }
            if args.contains(&"HEAD~1") {
                run(&f.repo, &["commit", "-qm", "move both submodules"]);
            }
        }
        if source == "HEAD" {
            run(&f.repo, &["update-index", "--force-remove", ".gitmodules"]);
        }
        if source != "worktree" {
            fs::remove_file(f.repo.join(".gitmodules")).unwrap();
        }
        fs::write(f.sub.join("file"), "dirty\n").unwrap();
        fs::write(other.join("file"), "dirty\n").unwrap();
        let want = run(&f.repo, args);
        assert!(want.contains("sub") && want.contains("other"), "{want}");
        let got = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&f.repo)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", f.repo.parent().unwrap().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TZ", "UTC")
            .env("PYN_DEBUG_SUBMODULE_CONFIG", "1")
            .output()
            .unwrap();
        assert_eq!(
            got.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&got.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&got.stdout), want);
        let stderr = String::from_utf8_lossy(&got.stderr);
        let count = |event: &str| {
            stderr
                .lines()
                .filter(|line| line.starts_with(&format!("submodule config {event}: ")))
                .count()
        };
        assert_eq!(
            stderr
                .lines()
                .filter(|line| *line == validation_line(&f))
                .count(),
            1
        );
        assert_eq!(
            [count("parse"), count("index"), count("head")],
            [
                1,
                usize::from(source != "worktree"),
                usize::from(source == "HEAD")
            ],
            "{args:?}, {source}: {stderr}"
        );
    }
}

fn nested_config_reads(args: &[&str]) {
    each(|f| {
        let format = run(&f.repo, &["rev-parse", "--show-object-format"]);
        let nested = f.nested(format.trim());
        fs::write(nested.join("file"), "dirty\n").unwrap();
        let want = git(&f.repo).args(args).output().unwrap();
        assert!(want.status.success());
        assert!(!want.stdout.is_empty());
        assert!(want.stderr.is_empty());
        let got = f
            .command(args)
            .env("PYN_DEBUG_SUBMODULE_CONFIG", "1")
            .output()
            .unwrap();
        assert_eq!(got.status.code(), want.status.code());
        assert_eq!(got.stdout, want.stdout);
        let stderr = String::from_utf8_lossy(&got.stderr);
        let parses: Vec<_> = stderr
            .lines()
            .filter(|line| {
                line.starts_with("submodule config ")
                    && !line.starts_with("submodule config validation: ")
            })
            .collect();
        assert_eq!(parses.len(), 2, "{args:?}: {stderr}");
        for dir in [f.repo.join(".git"), f.sub_git_dir()] {
            let event = format!("submodule config parse: {}", dir.display());
            assert_eq!(
                parses.iter().filter(|line| **line == event).count(),
                1,
                "{stderr}"
            );
        }
    });
}

fn nested_ignore_warning(args: &[&str]) {
    each(|f| {
        let format = run(&f.repo, &["rev-parse", "--show-object-format"]);
        let nested = f.nested(format.trim());
        fs::write(nested.join("file"), "dirty\n").unwrap();
        fs::write(
            f.sub.join(".gitmodules"),
            "[submodule \"inner\"]\n path = nested\n url = ../inner\n ignore = bogus\n",
        )
        .unwrap();
        let want = git(&f.repo).args(args).output().unwrap();
        eprintln!(
            "git {args:?}: status={}, stdout={:?}, stderr={:?}",
            want.status,
            String::from_utf8_lossy(&want.stdout),
            String::from_utf8_lossy(&want.stderr)
        );
        assert!(want.status.success());
        assert!(!want.stdout.is_empty());
        assert_eq!(String::from_utf8_lossy(&want.stderr).lines().count(), 1);
        let got = f.pyn(args);
        assert_eq!(got.status.code(), want.status.code());
        assert_eq!(got.stdout, want.stdout);
        assert_eq!(
            String::from_utf8_lossy(&got.stderr),
            String::from_utf8_lossy(&want.stderr)
        );
    });
}

#[test]
fn nested_status_parses_submodule_config_once() {
    nested_config_reads(&["status", "-s"]);
}

#[test]
fn nested_diff_parses_submodule_config_once() {
    nested_config_reads(&["diff"]);
}

#[test]
fn nested_status_warns_once() {
    nested_ignore_warning(&["status", "-s"]);
}

#[test]
fn nested_diff_warns_once() {
    nested_ignore_warning(&["diff"]);
}

#[test]
fn status_parses_submodule_config_once() {
    submodule_config_reads("worktree", &["status", "-s", "--", "sub", "other"]);
}

#[test]
fn status_reads_index_submodule_config_once() {
    submodule_config_reads("index", &["status", "-s", "--", "sub", "other"]);
}

#[test]
fn status_reads_head_submodule_config_once() {
    submodule_config_reads("HEAD", &["status", "-s", "--", "sub", "other"]);
}

#[test]
fn diff_parses_submodule_config_once() {
    diff_submodule_config_reads("worktree");
}

#[test]
fn diff_reads_index_submodule_config_once() {
    diff_submodule_config_reads("index");
}

#[test]
fn diff_reads_head_submodule_config_once() {
    diff_submodule_config_reads("HEAD");
}

fn diff_submodule_config_reads(source: &str) {
    for args in [
        vec!["diff", "--cached", "--", "sub", "other"],
        vec!["diff", "--", "sub", "other"],
        vec!["diff", "HEAD", "--", "sub", "other"],
        vec!["diff", "HEAD~1", "HEAD", "--", "sub", "other"],
    ] {
        submodule_config_reads(source, &args);
    }
}

#[test]
fn stash_show_numstat() {
    each(|f| {
        commit(&f.repo, "normal", b"one\n", "normal");
        fs::write(f.repo.join("normal"), "two\n").unwrap();
        run(&f.repo, &["add", "normal"]);
        record_stash(f);
        let want = run(&f.repo, &["stash", "show", "--numstat"]);
        eprintln!("git stash show --numstat: {want:?}");
        assert!(!want.is_empty());
        f.same(&["stash", "show", "--numstat"]);
        f.same(&["stash", "show", "--stat"]);
    });
}

fn numstat_paths(binary: bool) {
    for value in ["true", "false"] {
        each(|f| {
            let names = [
                "plain",
                "with space",
                "line\nbreak",
                "tab\tname",
                "back\\slash",
                "double\"quote",
                "high-é",
            ];
            for name in names {
                fs::write(
                    f.repo.join(name),
                    if binary {
                        b"one\0\n".as_slice()
                    } else {
                        b"one\n".as_slice()
                    },
                )
                .unwrap();
            }
            run(&f.repo, &["add", "."]);
            run(&f.repo, &["commit", "-qm", "names"]);
            for name in names {
                fs::write(
                    f.repo.join(name),
                    if binary {
                        b"two\0\n".as_slice()
                    } else {
                        b"two\n".as_slice()
                    },
                )
                .unwrap();
            }
            let want = run(&f.repo, &["diff", "--numstat"]);
            eprintln!("git numstat binary={binary}: {want:?}");
            assert_eq!(want.lines().count(), names.len());
            run(&f.repo, &["config", "core.quotePath", value]);
            f.same(&["diff", "--numstat"]);
            run(&f.repo, &["add", "."]);
            f.same(&["diff", "--cached", "--numstat"]);
            run(&f.repo, &["commit", "-qm", "changed names"]);
            f.same(&["show", "--format=", "--numstat"]);
        });
    }
}

#[test]
fn numstat_quotes_text_paths() {
    numstat_paths(false);
}

#[test]
fn numstat_quotes_binary_paths() {
    numstat_paths(true);
}

#[test]
fn add_resolves_unmerged_gitlink() {
    use std::io::Write;
    use std::process::Stdio;
    each(|f| {
        let first = run(&f.sub, &["rev-parse", "HEAD"]);
        f.state("forward");
        run(&f.repo, &["update-index", "--force-remove", "sub"]);
        let entries = format!(
            "160000 {} 1\tsub\n160000 {} 2\tsub\n160000 {} 3\tsub\n",
            first.trim(),
            first.trim(),
            f.second
        );
        let mut child = git(&f.repo)
            .args(["update-index", "--index-info"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(entries.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
        let oracle = f.repo.parent().unwrap().join("oracle");
        gitfix::copy_tree(&f.repo, &oracle);
        run(&oracle, &["add", "sub"]);
        let want = run(&oracle, &["ls-files", "--stage", "--", "sub"]);
        eprintln!("git resolved index: {want:?}");
        assert_eq!(want.lines().count(), 1);
        assert!(want.contains(" 0\tsub\n"));
        let got = f.pyn(&["add", "sub"]);
        assert_eq!(
            got.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&got.stderr)
        );
        assert_eq!(run(&f.repo, &["ls-files", "--stage", "--", "sub"]), want);
        assert_eq!(run(&f.repo, &["write-tree"]), run(&oracle, &["write-tree"]));
    });
}

#[test]
fn staged_gitlink_retains_directory_size() {
    each(|f| {
        f.state("forward");
        let oracle = f.repo.parent().unwrap().join("oracle");
        gitfix::copy_tree(&f.repo, &oracle);
        run(&oracle, &["add", "sub"]);
        let want = pyn_git::Repository::open(&oracle)
            .unwrap()
            .read_index()
            .unwrap()
            .unwrap();
        let want = want.entries.iter().find(|e| e.path == b"sub").unwrap();
        assert_eq!(
            u64::from(want.size),
            fs::metadata(oracle.join("sub")).unwrap().len()
        );
        let got = f.pyn(&["add", "sub"]);
        assert_eq!(got.status.code(), Some(0));
        let got = pyn_git::Repository::open(&f.repo)
            .unwrap()
            .read_index()
            .unwrap()
            .unwrap();
        let got = got.entries.iter().find(|e| e.path == b"sub").unwrap();
        assert_eq!(u64::from(got.size), fs::metadata(&f.sub).unwrap().len());
        assert_eq!(run(&f.repo, &["write-tree"]), run(&oracle, &["write-tree"]));
    });
}

#[test]
fn global_ignore_validation_without_gitlinks() {
    each(|f| {
        run(&f.repo, &["update-index", "--force-remove", "sub"]);
        commit(&f.repo, "normal", b"one\n", "normal");
        commit(&f.repo, "normal", b"two\n", "changed normal");
        fs::write(f.repo.join("normal"), "three\n").unwrap();
        run(&f.repo, &["add", "normal"]);
        for value in [None, Some("bogus")] {
            f.configure(value, None, None);
            for args in [
                vec!["diff", "--cached", "--", "normal"],
                vec!["diff", "HEAD~1", "HEAD", "--", "normal"],
                vec!["status", "-s", "--", "normal"],
            ] {
                let want = git(&f.repo).args(&args).output().unwrap();
                eprintln!(
                    "git {args:?}, global={value:?}: exit={:?}, stdout={:?}, stderr={:?}",
                    want.status.code(),
                    String::from_utf8_lossy(&want.stdout),
                    String::from_utf8_lossy(&want.stderr)
                );
                assert_eq!(
                    want.status.code(),
                    Some(if value.is_some() { 128 } else { 0 })
                );
                if value.is_none() {
                    assert!(!want.stdout.is_empty());
                }
                let got = f.pyn(&args);
                assert_eq!(got.status.code(), want.status.code(), "{args:?}");
                assert_eq!(got.stdout, want.stdout, "{args:?}");
                assert_eq!(got.stderr, want.stderr, "{args:?}");
            }
        }
    });
}

#[test]
fn staging_gitlinks_respects_index_bits() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        each(|f| {
            f.state("backward");
            f.configure(None, Some("all"), None);
            run(&f.repo, &["update-index", flag, "sub"]);
            let before = run(&f.repo, &["write-tree"]);
            let oracle = f.repo.parent().unwrap().join("oracle");
            gitfix::copy_tree(&f.repo, &oracle);
            run(&oracle, &["add", "-u"]);
            assert_eq!(run(&oracle, &["write-tree"]), before);
            let got = f.pyn(&["add", "-u"]);
            assert_eq!(
                got.status.code(),
                Some(0),
                "{flag}: {}",
                String::from_utf8_lossy(&got.stderr)
            );
            assert_eq!(run(&f.repo, &["write-tree"]), run(&oracle, &["write-tree"]));
        });
    }
}

fn normal_fixture(f: &Fixture) {
    run(&f.repo, &["update-index", "--force-remove", "sub"]);
    f.state("missing");
    commit(&f.repo, "normal", b"base\n", "normal");
}

fn normal_stash(f: &Fixture) {
    let original = fs::read(f.repo.join("normal")).unwrap();
    let head = run(&f.repo, &["rev-parse", "HEAD"]);
    let tree = run(&f.repo, &["rev-parse", "HEAD^{tree}"]);
    let index = run(
        &f.repo,
        &["commit-tree", tree.trim(), "-p", head.trim(), "-m", "index"],
    );
    fs::write(f.repo.join("normal"), "stash\n").unwrap();
    run(&f.repo, &["add", "normal"]);
    let tree = run(&f.repo, &["write-tree"]);
    let stash = run(
        &f.repo,
        &[
            "commit-tree",
            tree.trim(),
            "-p",
            head.trim(),
            "-p",
            index.trim(),
            "-m",
            "stash",
        ],
    );
    run(
        &f.repo,
        &[
            "update-ref",
            "--create-reflog",
            "-m",
            "stash",
            "refs/stash",
            stash.trim(),
        ],
    );
    run(&f.repo, &["read-tree", "HEAD"]);
    fs::write(f.repo.join("normal"), original).unwrap();
}

fn stash_completion(verb: &str) {
    for default in [None, Some("bogus")] {
        each(|f| {
            normal_fixture(f);
            normal_stash(f);
            f.configure(default, None, None);
            let oracle = f.repo.parent().unwrap().join("oracle");
            gitfix::copy_tree(&f.repo, &oracle);
            let want = git(&oracle).args(["stash", verb]).output().unwrap();
            eprintln!(
                "git stash {verb}, {default:?}: exit={:?}, stderr={:?}",
                want.status.code(),
                String::from_utf8_lossy(&want.stderr)
            );
            assert_eq!(want.status.code(), Some(0));
            let got = f.pyn(&["stash", verb]);
            assert_eq!(
                got.status.code(),
                want.status.code(),
                "{}",
                String::from_utf8_lossy(&got.stderr)
            );
            assert_eq!(got.stderr, want.stderr);
            assert_eq!(
                fs::read(f.repo.join("normal")).unwrap(),
                fs::read(oracle.join("normal")).unwrap()
            );
            assert_eq!(fs::read(f.repo.join("normal")).unwrap(), b"stash\n");
            assert_eq!(run(&f.repo, &["write-tree"]), run(&oracle, &["write-tree"]));
            assert_eq!(
                run(&f.repo, &["ls-files", "--stage"]),
                run(&oracle, &["ls-files", "--stage"])
            );
            let want_ref = git(&oracle)
                .args(["rev-parse", "--verify", "refs/stash"])
                .output()
                .unwrap();
            let got_ref = git(&f.repo)
                .args(["rev-parse", "--verify", "refs/stash"])
                .output()
                .unwrap();
            assert_eq!(want_ref.status.success(), verb == "apply");
            assert_eq!(got_ref.status.code(), want_ref.status.code());
            assert_eq!(got_ref.stdout, want_ref.stdout);
            if default.is_some() {
                assert_eq!(got.stdout, want.stdout);
            } else {
                assert!(!want.stdout.is_empty() && !got.stdout.is_empty());
            }
        });
    }
}

#[test]
fn stash_apply_completion() {
    stash_completion("apply");
}

#[test]
fn stash_pop_completion() {
    stash_completion("pop");
}

#[test]
fn empty_pick_status_reporting() {
    for default in [None, Some("bogus")] {
        each(|f| {
            normal_fixture(f);
            f.configure(default, None, None);
            run(&f.repo, &["config", "user.name", "t"]);
            run(&f.repo, &["config", "user.email", "t@example.com"]);
            let oracle = f.repo.parent().unwrap().join("oracle");
            gitfix::copy_tree(&f.repo, &oracle);
            let want = git(&oracle).args(["cherry-pick", "HEAD"]).output().unwrap();
            eprintln!(
                "git empty pick, {default:?}: exit={:?}, stderr={:?}",
                want.status.code(),
                String::from_utf8_lossy(&want.stderr)
            );
            assert_eq!(
                want.status.code(),
                Some(if default.is_some() { 128 } else { 1 })
            );
            let got = f.pyn(&["cherry-pick", "HEAD"]);
            assert_eq!(got.status.code(), want.status.code());
            for name in ["CHERRY_PICK_HEAD", "MERGE_MSG"] {
                assert_eq!(
                    fs::read(f.repo.join(".git").join(name)).unwrap(),
                    fs::read(oracle.join(".git").join(name)).unwrap()
                );
            }
            assert_eq!(run(&f.repo, &["write-tree"]), run(&oracle, &["write-tree"]));
            assert_eq!(
                run(&f.repo, &["rev-parse", "HEAD"]),
                run(&oracle, &["rev-parse", "HEAD"])
            );
            if default.is_some() {
                assert_eq!(got.stderr, want.stderr);
                assert_eq!(got.stdout, want.stdout);
            } else {
                assert!(!want.stdout.is_empty() && !got.stdout.is_empty());
            }
        });
    }
}

#[test]
fn recursive_status_validates_child_config() {
    for default in [None, Some("bogus")] {
        each(|f| {
            f.state("tracked");
            if let Some(value) = default {
                run(&f.sub, &["config", "diff.ignoreSubmodules", value]);
            }
            let want = git(&f.repo).args(["status", "-s"]).output().unwrap();
            let got = f.pyn(&["status", "-s"]);
            assert_eq!(
                want.status.code(),
                Some(if default.is_some() { 128 } else { 0 })
            );
            assert_eq!(got.status.code(), want.status.code());
            assert_eq!(got.stdout, want.stdout);
            if default.is_some() {
                let want = String::from_utf8_lossy(&want.stderr);
                let got = String::from_utf8_lossy(&got.stderr);
                assert_eq!(got.lines().next(), want.lines().next());
            } else {
                assert!(!want.stdout.is_empty());
                assert_eq!(got.stderr, want.stderr);
            }
        });
    }
}

fn duplicate_global_config(args: &[&str]) {
    each(|f| {
        normal_fixture(f);
        fs::write(f.repo.join("normal"), "changed\n").unwrap();
        run(&f.repo, &["add", "normal"]);
        for (values, code) in [
            (vec!["none", "all"], 0),
            (vec!["bogus", "none"], 128),
            (vec!["none", "bogus"], 128),
            (vec!["bogus-first", "bogus-last", "none"], 128),
        ] {
            f.configure(None, None, None);
            for value in &values {
                run(
                    &f.repo,
                    &["config", "--add", "diff.ignoreSubmodules", value],
                );
            }
            let want = git(&f.repo).args(args).output().unwrap();
            eprintln!(
                "git {args:?}, {values:?}: exit={:?}, stderr={:?}",
                want.status.code(),
                String::from_utf8_lossy(&want.stderr)
            );
            assert_eq!(want.status.code(), Some(code));
            if code == 0 {
                assert!(!want.stdout.is_empty());
            }
            let got = f.pyn(args);
            assert_eq!(got.status.code(), want.status.code());
            assert_eq!(got.stdout, want.stdout);
            assert_eq!(got.stderr, want.stderr);
        }
    });
}

#[test]
fn status_validates_all_global_values() {
    duplicate_global_config(&["status", "-s"]);
}

#[test]
fn diff_validates_all_global_values() {
    duplicate_global_config(&["diff", "--cached", "--", "normal"]);
}

fn validation_line(f: &Fixture) -> String {
    format!(
        "submodule config validation: {}",
        f.repo.join(".git").display()
    )
}

fn observer_config_boundary(args: &[&str]) {
    each(|f| {
        normal_fixture(f);
        commit(&f.repo, "normal", b"changed\n", "changed normal");
        commit(&f.repo, "normal", b"changed again\n", "changed again");
        normal_stash(f);
        let mut counts = Vec::new();
        for default in [None, Some("bogus")] {
            f.configure(default, None, None);
            let before = gitfix::snapshot(&f.repo);
            let want = git(&f.repo).args(args).env("TZ", "UTC").output().unwrap();
            eprintln!(
                "git {args:?}, {default:?}: exit={:?}, stderr={:?}",
                want.status.code(),
                String::from_utf8_lossy(&want.stderr)
            );
            assert_eq!(
                want.status.code(),
                Some(if default.is_some() { 128 } else { 0 })
            );
            if default.is_none() {
                assert!(!want.stdout.is_empty());
            }
            let got = f
                .command(args)
                .env("PYN_DEBUG_SUBMODULE_CONFIG", "1")
                .output()
                .unwrap();
            assert_eq!(got.status.code(), want.status.code());
            assert_eq!(got.stdout, want.stdout);
            let stderr = String::from_utf8_lossy(&got.stderr);
            counts.push(
                stderr
                    .lines()
                    .filter(|line| *line == validation_line(f))
                    .count(),
            );
            let diagnostics: String = stderr
                .split_inclusive('\n')
                .filter(|line| !line.starts_with("submodule config "))
                .collect();
            assert_eq!(diagnostics.as_bytes(), want.stderr);
            assert_eq!(gitfix::snapshot(&f.repo), before);
        }
        assert_eq!(counts, [1, 1], "{args:?}");
    });
}

#[test]
fn show_global_config_boundary() {
    observer_config_boundary(&["show", "--stat"]);
    observer_config_boundary(&["show", "-s"]);
}

#[test]
fn stash_show_global_config_boundary() {
    observer_config_boundary(&["stash", "show", "--numstat"]);
}

#[test]
fn log_global_config_boundary() {
    observer_config_boundary(&["log", "-n", "2", "--stat"]);
    observer_config_boundary(&["log", "-n", "1"]);
}

#[test]
fn merge_global_config_boundary() {
    for stat in ["--stat", "--no-stat"] {
        for default in [Some("bogus"), None] {
            each(|f| {
                normal_fixture(f);
                commit(&f.repo, "normal", b"changed\n", "changed normal");
                run(&f.repo, &["branch", "side"]);
                run(&f.repo, &["reset", "--hard", "HEAD~1"]);
                f.configure(default, None, None);
                let oracle = f.repo.parent().unwrap().join("oracle");
                gitfix::copy_tree(&f.repo, &oracle);
                let want = git(&oracle).args(["merge", stat, "side"]).output().unwrap();
                eprintln!(
                    "git merge {stat}, {default:?}: exit={:?}, stderr={:?}",
                    want.status.code(),
                    String::from_utf8_lossy(&want.stderr)
                );
                assert_eq!(
                    want.status.code(),
                    Some(if default.is_some() { 128 } else { 0 })
                );
                if default.is_none() {
                    assert!(!want.stdout.is_empty());
                }
                let got = f
                    .command(&["merge", stat, "side"])
                    .env("PYN_DEBUG_SUBMODULE_CONFIG", "1")
                    .output()
                    .unwrap();
                assert_eq!(got.status.code(), want.status.code());
                assert_eq!(got.stdout, want.stdout);
                let stderr = String::from_utf8_lossy(&got.stderr);
                let diagnostics: String = stderr
                    .split_inclusive('\n')
                    .filter(|line| !line.starts_with("submodule config "))
                    .collect();
                assert_eq!(diagnostics.as_bytes(), want.stderr);
                assert_eq!(
                    fs::read(f.repo.join("normal")).unwrap(),
                    fs::read(oracle.join("normal")).unwrap()
                );
                assert_eq!(run(&f.repo, &["write-tree"]), run(&oracle, &["write-tree"]));
                assert_eq!(
                    run(&f.repo, &["rev-parse", "HEAD"]),
                    run(&oracle, &["rev-parse", "HEAD"])
                );
                assert_eq!(
                    stderr
                        .lines()
                        .filter(|line| *line == validation_line(f))
                        .count(),
                    1
                );
            });
        }
    }
}

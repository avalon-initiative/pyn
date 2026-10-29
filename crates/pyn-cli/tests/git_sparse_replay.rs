//! Sparse checkout in `restore`, `checkout <paths>`, `stash`, `merge`, `cherry-pick`, `revert`, `rebase` and `apply`,
//! compared with the real command on a copy of one repository: exit status class, `HEAD`, index stages and
//! skip-worktree bits, work tree and in-progress state. Real git is the oracle for meaning only.

mod common;
#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::{FORMATS, copy_tree, snapshot};

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

const IN_PROGRESS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn feed(mut c: Command, input: &str) -> Output {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = c
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
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
        pair.sh(&pair.git, &init);
        copy_tree(&pair.git, &pair.pyn);
        pair
    }

    fn sh(&self, dir: &Path, script: &str) {
        let out = Command::new("sh")
            .args(["-e", "-c", script])
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(out.status.success(), "{script}: {}", text(&out.stderr));
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.git_feed(dir, args, "")
    }

    fn git_feed(&self, dir: &Path, args: &[&str], input: &str) -> Output {
        let mut c = Command::new("git");
        c.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY);
        feed(c, input)
    }

    fn pyn_feed(&self, args: &[&str], input: &str) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
        c.args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO");
        feed(c, input)
    }

    fn state(&self, dir: &Path) -> Vec<String> {
        let g = |args: &[&str]| text(&self.git_in(dir, args).stdout);
        let mut out = vec![
            format!("HEAD {}", g(&["rev-parse", "HEAD"]).trim()),
            format!("branch {}", g(&["symbolic-ref", "-q", "HEAD"]).trim()),
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!("tags\n{}", g(&["ls-files", "-t"])),
            format!(
                "stash {}",
                g(&["rev-parse", "-q", "--verify", "refs/stash"]).trim()
            ),
            format!("sparse {}", g(&["config", "core.sparseCheckout"]).trim()),
        ];
        for name in IN_PROGRESS {
            out.push(format!("{name} {}", dir.join(".git").join(name).exists()));
        }
        for (path, content) in snapshot(dir) {
            if path != ".git" && !path.starts_with(".git/") && !path.starts_with(".pyn") {
                out.push(format!("{path} {content}"));
            }
        }
        out
    }

    /// Runs the same `git <args>` and `pyn <args>`; the status class and the whole resulting state must agree.
    fn same(&self, args: &[&str]) {
        self.same_feed(args, "");
    }

    /// As `same`, and git must have succeeded.
    fn works(&self, args: &[&str]) {
        assert_eq!(self.same_feed(args, ""), 0, "{args:?}");
    }

    /// As `same`, and git must have stopped on a conflict.
    fn conflicts(&self, args: &[&str]) {
        assert_ne!(self.same_feed(args, ""), 0, "{args:?}");
    }

    fn same_feed(&self, args: &[&str], input: &str) -> i32 {
        let (g, p) = (
            self.git_feed(&self.git, args, input),
            self.pyn_feed(args, input),
        );
        let what = format!("{args:?}");
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{what}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        let (want, got) = (self.state(&self.git), self.state(&self.pyn));
        if let Some((w, g)) = want.iter().zip(&got).find(|(w, g)| w != g) {
            panic!(
                "{what}: git has {w:?} but pyn has {g:?}; pyn said {}",
                text(&p.stderr)
            );
        }
        assert_eq!(want.len(), got.len(), "{what}");
        g.status.code().unwrap()
    }

    fn both_sh(&self, script: &str) {
        self.sh(&self.git, script);
        self.sh(&self.pyn, script);
    }

    /// pyn refuses (or git does too) and leaves the state as it was.
    fn refused(&self, args: &[&str]) {
        self.refused_feed(args, "");
    }

    fn refused_feed(&self, args: &[&str], input: &str) {
        let before = self.state(&self.pyn);
        let p = self.pyn_feed(args, input);
        assert_ne!(p.status.code(), Some(0), "{args:?}: {}", text(&p.stderr));
        assert_eq!(before, self.state(&self.pyn), "{args:?}");
    }
}

/// `base`, then `feat` (changes in and out of the patterns) and `main` (more of them), left on `main`.
const REPO: &str = "mkdir -p a/b c/deep d; echo x >x; echo y >a/y; echo z >a/b/z; echo w >c/w; echo q >c/deep/q; echo v >d/v\n\
    git add -A; git commit -qm base\n\
    git checkout -q -b feat\n\
    echo feat >a/y; echo n >a/b/new; echo n >c/new; echo feat >c/w; echo e >d/extra; rm d/v\n\
    git add -A; git commit -qm feat\n\
    git checkout -q main\n\
    echo main >a/b/z; echo main >c/deep/q; echo main >x\n\
    git add -A; git commit -qm main\n";

const CONE: &str = "git sparse-checkout set a/b d\n";
const NON_CONE: &str = "git sparse-checkout set --no-cone '/a/' '!/a/b/' '/x' '/d/'\n";

/// Each format and each pattern style, with `setup` run on both copies after the sparse patterns.
fn each_cone(setup: &str, run: impl Fn(&Pair)) {
    each_history("", setup, run);
}

/// `more` is committed on `main` before the patterns are set.
fn each_history(more: &str, setup: &str, run: impl Fn(&Pair)) {
    each_in(&FORMATS, more, setup, run);
}

/// As `each_history`, for the formats named; git's own `stash` does not work in SHA-256 repositories in every release.
fn each_in(formats: &[&str], more: &str, setup: &str, run: impl Fn(&Pair)) {
    for &format in formats {
        for patterns in [CONE, NON_CONE] {
            let p = Pair::new(format, &format!("{REPO}{more}{patterns}"));
            p.both_sh(setup);
            run(&p);
        }
    }
}

#[test]
fn restore_from_the_index_leaves_skipped_paths_alone() {
    each_cone("echo dirty >d/v; echo dirty >a/y", |p| {
        p.same(&["restore", "d/v"]);
        p.same(&["restore", "c/w"]);
        p.same(&["restore", "."]);
    });
}

#[test]
fn restore_ignoring_skip_worktree_bits_brings_the_path_back() {
    each_cone("", |p| {
        p.same(&["restore", "--ignore-skip-worktree-bits", "c/w"]);
        p.same(&["restore", "--ignore-skip-worktree-bits", "c"]);
    });
}

#[test]
fn restore_from_a_tree_keeps_the_patterns() {
    each_cone("echo dirty >d/v", |p| {
        p.same(&["restore", "--source", "feat", "d"]);
        p.same(&["restore", "--source", "feat", "--staged", "--worktree", "d"]);
        p.same(&["restore", "--source", "feat", "a/y"]);
    });
}

#[test]
fn checkout_of_paths_keeps_the_patterns() {
    each_cone("echo dirty >d/v", |p| {
        p.same(&["checkout", "--", "d/v"]);
        p.same(&["checkout", "--", "c/w"]);
        p.same(&["checkout", "feat", "--", "a/y"]);
        p.same(&["checkout", "feat", "--", "d"]);
        p.same(&["checkout", "--ignore-skip-worktree-bits", "--", "c/w"]);
    });
}

#[test]
fn reset_keep_and_merge_leave_skipped_files_out() {
    for mode in ["--keep", "--merge"] {
        each_cone("", |p| {
            p.same(&["reset", mode, "feat"]);
            p.same(&["reset", mode, "main"]);
        });
        each_cone("echo dirty >d/v", |p| p.same(&["reset", mode, "feat"]));
        each_cone("echo dirty >a/y", |p| {
            p.same(&["reset", mode, "feat"]);
            p.same(&["reset", mode, "HEAD~1"]);
        });
    }
}

#[test]
fn patch_modes_from_the_index_skip_the_skipped_files() {
    each_cone("echo dirty >d/v; echo dirty >a/y", |p| {
        p.same_feed(&["restore", "-p"], "y\nn\n");
        p.same_feed(&["checkout", "-p"], "y\n");
        p.same_feed(&["add", "-p"], "y\ny\n");
        p.same_feed(&["reset", "-p"], "y\ny\n");
        p.same_feed(&["restore", "-p", "--staged"], "y\ny\n");
    });
}

#[test]
fn patch_modes_against_a_tree_that_differs_in_a_skipped_file_refuse() {
    each_cone("echo dirty >d/v", |p| {
        for args in [
            vec!["checkout", "-p", "feat"],
            vec!["restore", "-p", "-s", "feat"],
            vec!["reset", "-p", "feat"],
            vec!["stash", "-p"],
        ] {
            p.refused_feed(&args, "y\ny\ny\ny\ny\ny\n");
        }
    });
}

/// `main` also edits `c/w` and `a/y`, so merging `feat` conflicts both outside and inside the patterns.
const CONFLICTS: &str = "echo main >c/w; echo main >a/y; git add -A; git commit -qm more\n";

#[test]
fn a_merge_that_is_not_a_fast_forward_keeps_the_patterns() {
    each_cone("", |p| p.works(&["merge", "feat"]));
    each_cone("", |p| {
        p.works(&["merge", "--no-commit", "--no-ff", "feat"])
    });
    each_cone("", |p| p.works(&["merge", "--squash", "feat"]));
    each_cone("", |p| p.works(&["merge", "--no-ff", "-X", "ours", "feat"]));
}

#[test]
fn conflicts_outside_the_patterns_are_written_to_the_work_tree() {
    each_history(CONFLICTS, "", |p| p.conflicts(&["merge", "feat"]));
    each_history(CONFLICTS, "", |p| {
        p.conflicts(&["merge", "feat"]);
        p.works(&["merge", "--abort"]);
    });
    each_history(CONFLICTS, "", |p| {
        p.conflicts(&["merge", "feat"]);
        p.works(&["reset", "--merge"]);
    });
    each_history(CONFLICTS, "", |p| {
        p.conflicts(&["merge", "feat"]);
        p.works(&["reset", "--hard"]);
    });
    each_history(CONFLICTS, "", |p| {
        p.conflicts(&["merge", "--squash", "feat"]);
    });
    each_history(CONFLICTS, "", |p| {
        p.works(&["merge", "-X", "theirs", "feat"])
    });
}

#[test]
fn a_merge_abort_puts_skipped_entries_back() {
    each_cone("", |p| {
        p.works(&["merge", "--no-commit", "--no-ff", "feat"]);
        p.works(&["merge", "--abort"]);
    });
}

#[test]
fn cherry_pick_and_revert_keep_the_patterns() {
    each_cone("", |p| p.works(&["cherry-pick", "feat"]));
    each_cone("", |p| p.works(&["cherry-pick", "-n", "feat"]));
    each_cone("", |p| {
        p.works(&["cherry-pick", "feat"]);
        p.works(&["revert", "HEAD"]);
    });
    each_cone("", |p| p.works(&["revert", "HEAD"]));
    each_cone("", |p| p.works(&["revert", "-n", "HEAD"]));
}

#[test]
fn a_pick_that_conflicts_outside_the_patterns_stops_as_git_does() {
    each_history(CONFLICTS, "", |p| p.conflicts(&["cherry-pick", "feat"]));
    each_history(CONFLICTS, "", |p| {
        p.conflicts(&["cherry-pick", "feat"]);
        p.works(&["cherry-pick", "--abort"]);
    });
    each_history(CONFLICTS, "", |p| {
        p.conflicts(&["cherry-pick", "feat"]);
        p.works(&["cherry-pick", "--skip"]);
    });
}

#[test]
fn rebase_keeps_the_patterns() {
    each_history("", "git checkout -q feat", |p| p.works(&["rebase", "main"]));
    each_history(CONFLICTS, "git checkout -q feat", |p| {
        p.conflicts(&["rebase", "main"]);
        p.works(&["rebase", "--abort"]);
    });
    each_history(CONFLICTS, "git checkout -q feat", |p| {
        p.conflicts(&["rebase", "main"]);
    });
    each_history("", "git checkout -q feat", |p| {
        p.works(&["rebase", "-i", "--root"]);
    });
}

/// A stash taken with the whole work tree present, applied once the patterns are set.
const STASHED_FULL: &str = "echo e1 >c/w; echo e2 >a/y; git stash -q\n";

fn each_stash(more: &str, setup: &str, run: impl Fn(&Pair)) {
    each_in(&["sha1"], more, setup, run);
}

#[test]
fn stash_push_keeps_the_patterns() {
    each_stash("", "echo e2 >a/y; echo e3 >d/v", |p| {
        p.works(&["stash"]);
        p.works(&["stash", "pop"]);
    });
    each_stash("", "echo e2 >a/y; echo n >d/n; git add d/n", |p| {
        p.works(&["stash", "push", "--keep-index"]);
        p.works(&["stash", "apply", "--index"]);
    });
    each_stash("", "echo e2 >a/y; echo n >d/n", |p| {
        p.works(&["stash", "push", "-u"]);
        p.works(&["stash", "pop"]);
    });
    each_stash("", "echo e2 >a/y; echo e3 >d/v", |p| {
        p.works(&["stash", "push", "--", "a"]);
        p.works(&["stash", "branch", "nb"]);
    });
}

#[test]
fn a_stash_with_paths_outside_the_patterns_applies_as_git_does() {
    each_stash(STASHED_FULL, "", |p| p.works(&["stash", "apply"]));
    each_stash(STASHED_FULL, "", |p| p.works(&["stash", "pop"]));
    each_stash(STASHED_FULL, "", |p| p.works(&["stash", "branch", "nb"]));
}

/// `git diff main feat` restricted to `paths`, as the file `patch` in both copies.
fn patch_of(p: &Pair, paths: &str) {
    p.both_sh(&format!("git diff main feat -- {paths} >../patch.diff"));
}

#[test]
fn apply_touches_skipped_entries_only_when_the_patch_names_them() {
    each_history("", "", |p| {
        patch_of(p, "a/b/new d");
        p.works(&["apply", "--index", "../patch.diff"]);
    });
    for args in [
        vec!["apply", "--index"],
        vec!["apply", "--cached"],
        vec!["apply", "--3way"],
        vec!["apply"],
    ] {
        for paths in ["a/b/new d", "c/w"] {
            each_in(&["sha1"], "", "", |p| {
                patch_of(p, paths);
                let mut full = args.clone();
                full.push("../patch.diff");
                p.same(&full);
            });
        }
    }
}

#[test]
fn am_keeps_the_patterns() {
    each_history("", "git format-patch -q -1 feat -o ../mail", |p| {
        let mails = fs::read_dir(p.git.parent().unwrap().join("mail")).unwrap();
        let mail = mails.map(|e| e.unwrap().path()).next().unwrap();
        p.works(&["am", mail.to_str().unwrap()]);
    });
    each_history("", "git format-patch -q -1 feat -o ../mail", |p| {
        let mails = fs::read_dir(p.git.parent().unwrap().join("mail")).unwrap();
        let mail = mails.map(|e| e.unwrap().path()).next().unwrap();
        p.works(&["am", "-3", mail.to_str().unwrap()]);
    });
}

#[test]
fn what_pyn_does_not_match_is_refused_and_changes_nothing() {
    each_stash(
        "echo e1 >c/w; echo e2 >a/y; git add -A; git stash -q\n",
        "",
        |p| {
            p.refused(&["stash", "apply", "--index"]);
            p.refused(&["restore", "--source", "feat", "c/new"]);
            p.refused(&["restore", "--source", "feat", "c/w"]);
            p.refused(&["restore", "--source", "feat", "."]);
            p.refused(&["checkout", "feat", "--", "c/new"]);
            p.refused(&[
                "restore",
                "--ignore-skip-worktree-bits",
                "--source",
                "feat",
                "c/w",
            ]);
        },
    );
}

/// `feat` also moves `c/w` into the patterns and `a/y` out of them, and deletes `c/new`.
const MORE_FEAT: &str = "git checkout -q feat\n\
    git mv c/w a/b/w2; echo ay >a/y; git mv a/y c/y2; git rm -q c/new; echo more >>a/b/w2; git add -A\n\
    git commit -qm shuffle\n\
    git checkout -q main\n";

#[test]
fn deletes_and_moves_across_the_patterns_replay_as_git_does() {
    each_history(MORE_FEAT, "", |p| p.works(&["merge", "feat"]));
    each_history(MORE_FEAT, "", |p| p.same(&["cherry-pick", "feat"]));
    each_history(MORE_FEAT, "git checkout -q feat", |p| {
        p.same(&["rebase", "main"]);
    });
    each_history(MORE_FEAT, "", |p| {
        p.works(&["merge", "feat"]);
        p.works(&["revert", "-m", "1", "HEAD"]);
    });
}

#[test]
fn a_modify_delete_conflict_outside_the_patterns_leaves_the_file() {
    let script = "echo main >c/w; git add -A; git commit -qm more\n\
        git checkout -q feat; git rm -q c/w; git commit -qm gone; git checkout -q main\n";
    each_history(script, "", |p| p.conflicts(&["merge", "feat"]));
    each_history(script, "", |p| p.conflicts(&["cherry-pick", "feat"]));
}

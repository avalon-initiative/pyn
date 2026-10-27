//! `pyn merge --squash` on a git repository, compared with the real `git merge --squash` on a copy of the same
//! repository: exit status, standard output, resulting `HEAD`, index, work tree, merge state files, `SQUASH_MSG` and
//! `AUTO_MERGE`, and the commit that concludes it. Needs `git` on the PATH (tests only).

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

/// What `git` is allowed to differ in from pyn.
#[derive(Clone, Copy, Default)]
struct Skip {
    /// The git before 2.50 only records `AUTO_MERGE` for `ort`.
    auto_merge: bool,
    /// Git writes extra reflog entries when it restores the tree after a strategy fails.
    reflog: bool,
    /// `resolve` labels conflict markers with the names of temporary files.
    markers: bool,
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
        // Background auto-gc would remove loose objects while `copy_tree` runs.
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
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    /// A shell command run in both copies, with real git.
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

    /// Everything a merge leaves behind that a person or git can see.
    fn state(&self, dir: &Path, skip: Skip) -> Vec<String> {
        let g = |args: &[&str]| text(&self.git_in(dir, args).stdout);
        let mut out = vec![
            format!("HEAD {}", g(&["rev-parse", "HEAD"]).trim()),
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!("status\n{}", g(&["status", "--porcelain"])),
        ];
        for name in [
            "MERGE_HEAD",
            "MERGE_MODE",
            "MERGE_MSG",
            "ORIG_HEAD",
            "SQUASH_MSG",
        ] {
            out.push(format!(
                "{name} {:?}",
                fs::read_to_string(dir.join(".git").join(name)).ok()
            ));
        }
        if !skip.auto_merge {
            let have = dir.join(".git/AUTO_MERGE").exists();
            let tree = if have {
                g(&["ls-tree", "-r", "AUTO_MERGE"])
            } else {
                String::new()
            };
            out.push(format!("AUTO_MERGE {have}\n{tree}"));
            if have {
                for line in tree.lines() {
                    let id = line.split_whitespace().nth(2).unwrap();
                    out.push(format!("blob {id} {:?}", g(&["cat-file", "blob", id])));
                }
            }
        }
        if !skip.reflog {
            out.push(format!(
                "reflog {}",
                g(&["log", "-g", "-1", "--format=%gs", "HEAD"])
            ));
        }
        for (path, content) in snapshot(dir) {
            if path == ".git" || path.starts_with(".git/") || path.starts_with(".pyn") {
                continue;
            }
            let file = dir.join(&path);
            if skip.markers && file.is_file() {
                let bytes = fs::read_to_string(&file).unwrap_or_default();
                let lines: Vec<&str> = bytes
                    .lines()
                    .map(|l| {
                        if l.starts_with("<<<<<<<")
                            || l.starts_with("|||||||")
                            || l.starts_with(">>>>>>>")
                        {
                            &l[..7]
                        } else {
                            l
                        }
                    })
                    .collect();
                out.push(format!("{path} {lines:?}"));
            } else {
                out.push(format!("{path} {content}"));
            }
        }
        out
    }

    /// Runs the same `git <args>` and `pyn <args>`; status and the whole resulting state must agree.
    fn same(&self, args: &[&str], skip: Skip) {
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
        assert_eq!(text(&g.stdout), text(&p.stdout), "{what}: stdout");
        assert_eq!(
            self.state(&self.git, skip),
            self.state(&self.pyn, skip),
            "{what}"
        );
    }
}

fn each_format(script: &str, run: impl Fn(&Pair)) {
    for format in common::gitfix::FORMATS {
        run(&Pair::new(format, script));
    }
}

/// `main` and `side` both changed `f` (conflict) and each added a file.
const CONFLICT: &str = "printf 'a\\nb\\nc\\nd\\ne\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'a\\nB2\\nc\\nd\\nE\\n' >f; printf 'x\\nside\\n' >g; git add .; git commit -qm side\n\
    git checkout -q main\nprintf 'a\\nB1\\nc\\nd\\ne\\n' >f; printf 'x\\nmain\\n' >h; git add .; git commit -qm main\n";

/// A clean merge needing a line merge in `f`.
const CLEAN: &str = "printf 'a\\nb\\nc\\nd\\ne\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'a\\nb\\nc\\nd\\nE\\n' >f; printf 'x\\nside\\n' >g; git add .; git commit -qm side\n\
    git checkout -q main\nprintf 'A\\nb\\nc\\nd\\ne\\n' >f; printf 'x\\nmain\\n' >h; git add .; git commit -qm main\n";

/// Disjoint files: git's plain in-index merge resolves it.
const TRIVIAL: &str = "printf 'a\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'x2\\n' >g; echo s >s; git add .; git commit -qm side\n\
    git checkout -q main\nprintf 'a2\\n' >f; git add .; git commit -qm main\n";

/// `side` is ahead of `main`.
const AHEAD: &str = "printf 'a\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b side\necho n >n; git add .; git commit -qm side\ngit checkout -q main\n";

/// `side` is behind `main`.
const BEHIND: &str = "printf 'a\\n' >f; git add .; git commit -qm base\ngit branch side\n\
    echo n >n; git add .; git commit -qm main\n";

/// Three commits on `side`, a merge of two of them, and a merge commit on `main`.
const HISTORY: &str = "printf 'a\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b side\necho n1 >n; git add .; git commit -qm 'side one'\n\
    echo n2 >>n; git commit -qam 'side two' -m 'with a body'\n\
    git checkout -q -b more main\necho m1 >m; git add .; git commit -qm 'more one'\n\
    git checkout -q side\ngit merge -q --no-ff -m 'merge more' more\n\
    git checkout -q main\n";

#[test]
fn squash_fast_forward() {
    each_format(AHEAD, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
        p.same(&["commit", "-q", "--no-edit"], Skip::default());
    });
    each_format(HISTORY, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
        p.same(&["commit", "-q", "-m", "squashed"], Skip::default());
    });
}

#[test]
fn squash_disjoint_files() {
    each_format(TRIVIAL, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
        p.same(&["commit", "-q", "--no-edit"], Skip::default());
    });
}

#[test]
fn squash_up_to_date() {
    each_format(BEHIND, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
    });
}

#[test]
fn squash_clean_merge() {
    each_format(CLEAN, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
        p.same(&["status", "--porcelain"], Skip::default());
        p.same(&["commit", "-q", "--no-edit"], Skip::default());
    });
    each_format(CLEAN, |p| {
        p.same(
            &["merge", "--squash", "-s", "ours", "side"],
            Skip::default(),
        );
    });
    each_format(CLEAN, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
        p.same(&["commit", "-q", "-m", "mine"], Skip::default());
    });
}

#[test]
fn squash_with_conflicts() {
    each_format(CONFLICT, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
        p.both_sh("printf 'a\\nB\\nc\\nd\\nE\\n' >f; git add f");
        p.same(&["commit", "-q", "--no-edit"], Skip::default());
    });
    each_format(CONFLICT, |p| {
        p.same(&["merge", "--squash", "side"], Skip::default());
        p.same(&["merge", "--abort"], Skip::default());
        p.same(&["reset", "--hard"], Skip::default());
    });
}

#[test]
fn squash_flags() {
    each_format(CLEAN, |p| {
        p.same(&["merge", "--squash", "--no-ff", "side"], Skip::default());
        p.same(&["merge", "--squash", "--ff-only", "side"], Skip::default());
        p.same(
            &["merge", "--squash", "-m", "ignored", "side"],
            Skip::default(),
        );
        p.same(&["reset", "--hard"], Skip::default());
        p.same(
            &["merge", "--squash", "--no-commit", "side"],
            Skip::default(),
        );
        p.same(&["reset", "--hard"], Skip::default());
        p.same(
            &["merge", "--squash", "--no-squash", "side"],
            Skip::default(),
        );
        p.same(&["merge", "--abort"], Skip::default());
        p.same(
            &["merge", "--no-squash", "--squash", "side"],
            Skip::default(),
        );
        p.same(&["reset", "--hard"], Skip::default());
        p.same(&["merge", "--squash", "nosuch"], Skip::default());
    });
}

#[test]
fn squash_into_an_unborn_branch_and_over_local_changes() {
    each_format(CLEAN, |p| {
        p.both_sh("echo dirty >g");
        let skip = Skip {
            auto_merge: true,
            ..Skip::default()
        };
        p.same(&["merge", "--squash", "side"], skip);
    });
    each_format(CLEAN, |p| {
        p.both_sh("git checkout -q --orphan fresh; git rm -rqf .");
        p.same(&["merge", "--squash", "side"], Skip::default());
    });
}

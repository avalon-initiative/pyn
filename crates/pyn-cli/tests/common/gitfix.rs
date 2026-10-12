//! The git home fixtures shared by the tests that run `pyn` on a git repository: a scenario repository per object
//! format (built once with real git, see `fixtures/git/regenerate.sh`) copied into a temporary directory.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const FORMATS: [&str; 2] = ["sha1", "sha256"];

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/git")
}

pub fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = if name == "dot-gitignore" {
            ".gitignore".into()
        } else {
            name
        };
        let target = to.join(name);
        let kind = entry.file_type().unwrap();
        if kind.is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path()).unwrap(), &target).unwrap();
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            let mut perms = fs::metadata(&target).unwrap().permissions();
            let mode = perms.mode() | 0o600;
            perms.set_mode(mode);
            fs::set_permissions(&target, perms).unwrap();
        }
    }
}

/// The scenario repository of one object format, with a linked worktree, in a temporary directory.
pub struct Repo {
    pub format: &'static str,
    _tmp: tempfile::TempDir,
    pub main: PathBuf,
    pub linked: PathBuf,
    pub home: PathBuf,
}

impl Repo {
    pub fn new(format: &'static str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (main, linked, home) = (root.join("main"), root.join("linked"), root.join("home"));
        fs::create_dir_all(&home).unwrap();
        copy_tree(&fixtures().join("worktree"), &main);
        copy_tree(&fixtures().join(format).join("dot-git"), &main.join(".git"));
        copy_tree(&fixtures().join("linked"), &linked);
        fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/linked").display()),
        )
        .unwrap();
        fs::write(
            main.join(".git/worktrees/linked/gitdir"),
            format!("{}\n", linked.join(".git").display()),
        )
        .unwrap();
        Self {
            format,
            _tmp: tmp,
            main,
            linked,
            home,
        }
    }

    pub fn expected(&self, name: &str) -> String {
        fs::read_to_string(fixtures().join(self.format).join("expected").join(name)).unwrap()
    }

    pub fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("PYN_SERVER")
            .env_remove("PYN_USER")
            .env_remove("PYN_TOKEN")
            .env("GIT_AUTHOR_NAME", "Staging Author")
            .env("GIT_AUTHOR_EMAIL", "staging@example.com")
            .env("GIT_COMMITTER_NAME", "Staging Committer")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0200")
            .env("GIT_COMMITTER_DATE", "1700000100 -0530")
            .env_remove("PYN_DIR")
            .env_remove("PYN_REPO")
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .output()
            .unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> String {
        self.ok_in(&self.main, args)
    }

    pub fn ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.run_in(cwd, args);
        assert!(
            out.status.success(),
            "{} pyn {args:?}: {}{}",
            self.format,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    /// Runs a command that must fail; returns its exit code and standard error.
    pub fn fails(&self, args: &[&str]) -> (i32, String) {
        let out = self.run_in(&self.main, args);
        assert!(!out.status.success(), "pyn {args:?} should have failed");
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// Every file and folder under `dir` with its bytes (or link target) and permission bits.
pub fn snapshot(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let meta = fs::symlink_metadata(&path).unwrap();
            let mode = meta.permissions().mode();
            if meta.file_type().is_symlink() {
                out.insert(
                    rel,
                    format!("-> {}", fs::read_link(&path).unwrap().display()),
                );
            } else if meta.is_dir() {
                out.insert(rel, format!("dir {mode:o}"));
                walk(root, &path, out);
            } else {
                out.insert(rel, format!("{mode:o} {:?}", fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The `STAGED UNSTAGED PATH` table as git's porcelain `XY path` lines, sorted.
pub fn porcelain(out: &str) -> Vec<String> {
    let code = |word: &str| match word {
        "added" => 'A',
        "modified" => 'M',
        "deleted" => 'D',
        "typechange" => 'T',
        "unmerged" => 'U',
        "untracked" => '?',
        "-" => ' ',
        other => panic!("unknown state {other}"),
    };
    let mut rows: Vec<String> = out
        .lines()
        .skip_while(|l| !l.starts_with("STAGED"))
        .skip(1)
        .take_while(|l| !l.contains(" staged, "))
        .map(|l| {
            let mut cells = l.split_whitespace();
            let (x, y, path) = (
                cells.next().unwrap(),
                cells.next().unwrap(),
                cells.next().unwrap(),
            );
            let (x, y) = if y == "untracked" {
                ('?', '?')
            } else {
                (code(x), code(y))
            };
            format!("{x}{y} {path}")
        })
        .collect();
    rows.sort();
    rows
}

pub fn sorted_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines.sort();
    lines
}

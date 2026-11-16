//! Shared by the patch exchange tests: twin repositories, one for real git and one for pyn, and a comparison of what
//! the two tools do on them. Real git is the oracle in these tests only.
#![allow(dead_code)]

#[path = "../../../pyn-gittransport/tests/common/gitsrv.rs"]
pub mod gitsrv;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[path = "../common/oracle.rs"]
pub mod oracle;

#[path = "../common/behavior.rs"]
pub mod behavior;

pub use behavior::{Format, Ran as Out, Stderr, assert_same_behavior};
pub use gitsrv::{FORMATS, git, init, path_env};
use tempfile::{TempDir, tempdir};

/// Two copies of one repository, `git` for the oracle and `pyn` for pyn.
pub struct Twin {
    _tmp: TempDir,
    pub root: PathBuf,
    pub git: PathBuf,
    pub pyn: PathBuf,
    pub format: &'static str,
}

pub fn write(dir: &Path, name: &str, text: &str) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&file);
    fs::write(file, text).unwrap();
}

pub fn lines(prefix: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{prefix}{i}\n")).collect()
}

pub fn run(dir: &Path, args: &[&str]) -> String {
    gitsrv::run(dir, args)
}

/// Commits everything with a distinct, fixed author date and committer date.
pub fn commit(dir: &Path, tick: &mut i64, author: &str, message: &str) {
    *tick += 3600;
    let date = format!("{tick} +0200");
    let committer = format!("{} +0000", *tick + 5);
    run(dir, &["add", "-A"]);
    let out = git(dir)
        .args([
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
            "--author",
            author,
        ])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &committer)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "commit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Twin {
    pub fn new(format: &'static str, build: impl Fn(&Path)) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let (g, p) = (root.join("git"), root.join("pyn"));
        for dir in [&g, &p] {
            init(dir, format, false);
            build(dir);
        }
        Self {
            _tmp: tmp,
            root,
            git: g,
            pyn: p,
            format,
        }
    }

    pub fn exec(&self, bin: &str, cwd: &Path, args: &[&str], stdin: &[u8]) -> Out {
        let mut c = Command::new(bin);
        c.args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "Commit Ter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .env("GIT_COMMITTER_DATE", "1800000000 +0000")
            .env("GIT_EDITOR", "true")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        let _ = child.stdin.take().unwrap().write_all(stdin);
        let out = child.wait_with_output().unwrap();
        Out {
            code: out.status.code().unwrap(),
            stdout: out.stdout,
            stderr: out.stderr,
        }
    }

    pub fn run_git(&self, sub: &str, args: &[&str], stdin: &[u8]) -> Out {
        self.exec("git", &self.git.join(sub), args, stdin)
    }

    pub fn run_pyn(&self, sub: &str, args: &[&str], stdin: &[u8]) -> Out {
        self.exec(env!("CARGO_BIN_EXE_pyn"), &self.pyn.join(sub), args, stdin)
    }

    /// Runs `args` with both tools in `sub` and compares what they did (human-readable output).
    pub fn same(&self, sub: &str, args: &[&str], stdin: &[u8]) -> (Out, Out) {
        self.same_as(Format::Human, Stderr::Ignored, sub, args, stdin)
    }

    /// Like `same`, with `Format::Machine` also demanding identical standard output and `Stderr::Quiet` silence.
    pub fn same_as(
        &self,
        format: Format,
        stderr: Stderr,
        sub: &str,
        args: &[&str],
        stdin: &[u8],
    ) -> (Out, Out) {
        let want = self.run_git(sub, args, stdin);
        let got = self.run_pyn(sub, args, stdin);
        let what = format!("{} {args:?} in '{sub}'", self.format);
        assert_same_behavior(&what, format, stderr, (&want, &self.git), (&got, &self.pyn));
        (want, got)
    }

    /// Runs real git on both copies.
    pub fn both(&self, args: &[&str]) {
        for dir in [&self.git, &self.pyn] {
            run(dir, args);
        }
    }

    pub fn each_dir(&self, f: impl Fn(&Path)) {
        for dir in [&self.git, &self.pyn] {
            f(dir);
        }
    }
}

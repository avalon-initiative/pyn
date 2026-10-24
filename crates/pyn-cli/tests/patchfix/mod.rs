//! Shared by the patch exchange tests: twin repositories, one for real git and one for pyn, and a comparison of what
//! the two tools do on them. Real git is the oracle in these tests only.
#![allow(dead_code)]

#[path = "../../../pyn-gittransport/tests/common/gitsrv.rs"]
pub mod gitsrv;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub use gitsrv::{FORMATS, git, init, path_env};
use tempfile::{TempDir, tempdir};

pub struct Out {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Out {
    pub fn out(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn err(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

/// Two copies of one repository, `git` for the oracle and `pyn` for pyn.
pub struct Twin {
    _tmp: TempDir,
    pub root: PathBuf,
    pub git: PathBuf,
    pub pyn: PathBuf,
    pub format: &'static str,
}

/// What a stopped `am` says about how to go on, which newer git sends to standard error instead, and the lines
/// that differ with the merge backend git uses.
pub fn without_resolve_hints(text: &str) -> String {
    text.lines()
        .filter(|l| {
            ![
                "When you have resolved this problem",
                "If you prefer to skip this patch",
                "To record the empty patch",
                "To restore the original branch",
                // merge-recursive (git 2.43) says this when a patch removes a file, merge-ort does not.
                "Removing ",
            ]
            .iter()
            .any(|p| l.starts_with(p))
        })
        .map(|l| format!("{l}\n"))
        .collect()
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
        child.stdin.take().unwrap().write_all(stdin).unwrap();
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

    /// Runs `args` with both tools in `sub` and demands the same status, standard output (pyn's `pyn ` read as
    /// `git `), use of standard error and resulting repository state. Returns both outputs.
    pub fn same(&self, sub: &str, args: &[&str], stdin: &[u8]) -> (Out, Out) {
        let want = self.run_git(sub, args, stdin);
        let got = self.run_pyn(sub, args, stdin);
        let what = format!("{} {args:?} in '{sub}'", self.format);
        assert_eq!(
            without_resolve_hints(&got.out().replace("pyn ", "git ")),
            without_resolve_hints(&want.out()),
            "stdout of {what} (pyn stderr: {})",
            got.err()
        );
        assert_eq!(
            got.code,
            want.code,
            "exit status of {what} (pyn stderr: {})",
            got.err()
        );
        // git 2.55 prints the hints of a stopped `am` on standard error, 2.43 on standard output.
        let hinted = |o: &Out| {
            o.out().contains("git am --continue") || o.err().contains("git am --continue")
        };
        if !hinted(&want) {
            assert_eq!(
                got.stderr.is_empty(),
                want.stderr.is_empty(),
                "stderr use of {what}: pyn {:?}, git {:?}",
                got.err(),
                want.err()
            );
        }
        assert_eq!(
            snapshot(&self.pyn),
            snapshot(&self.git),
            "state after {what}"
        );
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

fn files(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path.strip_prefix(root).unwrap().display().to_string();
        if rel == ".git" {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            files(root, &path, out);
        } else if meta.file_type().is_symlink() {
            out.insert(
                rel,
                format!("-> {}", fs::read_link(&path).unwrap().display()),
            );
        } else {
            let exec = meta.permissions().mode() & 0o111 != 0;
            let text = String::from_utf8_lossy(&fs::read(&path).unwrap())
                .replace("constructed fake ancestor", "constructed merge base");
            out.insert(rel, format!("{exec} {text:?}"));
        }
    }
}

/// The work tree, the index entries, `HEAD` and which session files `.git` holds.
pub fn snapshot(dir: &Path) -> String {
    let mut tree = BTreeMap::new();
    files(dir, dir, &mut tree);
    let head = git(dir).args(["rev-parse", "HEAD"]).output().unwrap();
    let session: Vec<String> = fs::read_dir(dir.join(".git"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n == "rebase-apply" || n.ends_with("_HEAD") && n != "ORIG_HEAD")
        .collect();
    format!(
        "{tree:?}\n{}{}{session:?}",
        run(dir, &["ls-files", "-s"]),
        String::from_utf8_lossy(&head.stdout),
    )
}

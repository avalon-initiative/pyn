//! A real `git` as an oracle: used by tests only.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pyn_object::{Algorithm, ObjectId};

pub struct Git {
    pub dir: PathBuf,
    pub algorithm: Algorithm,
}

pub const ALGORITHMS: [Algorithm; 2] = [Algorithm::Sha1, Algorithm::Sha256];

impl Git {
    /// A new repository (bare or not) in `dir`.
    pub fn init(dir: &Path, algorithm: Algorithm, bare: bool) -> Self {
        std::fs::create_dir_all(dir).unwrap();
        let g = Self {
            dir: dir.to_path_buf(),
            algorithm,
        };
        let mut args = vec!["init", "-q", "-b", "main"];
        if bare {
            args.push("--bare");
        }
        let format = format!("--object-format={}", algorithm.name());
        args.push(&format);
        g.run(&args);
        g
    }

    pub fn git_dir(&self) -> PathBuf {
        if self.dir.join(".git").is_dir() {
            self.dir.join(".git")
        } else {
            self.dir.clone()
        }
    }

    pub fn objects_dir(&self) -> PathBuf {
        self.git_dir().join("objects")
    }

    pub fn db(&self) -> pyn_git::ObjectDb {
        pyn_git::ObjectDb::open(&self.objects_dir(), self.algorithm).unwrap()
    }

    fn command(&self, args: &[&str], time: i64) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.dir)
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(["-c", "core.fsync=none", "-c", "advice.detachedHead=false"])
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_AUTHOR_DATE", format!("@{time} +0000"))
            .env("GIT_COMMITTER_DATE", format!("@{time} +0000"));
        c
    }

    /// Runs git, returns stdout, panics on failure.
    pub fn run(&self, args: &[&str]) -> Vec<u8> {
        self.run_stdin(args, b"", 1_700_000_000)
    }

    pub fn run_at(&self, args: &[&str], time: i64) -> Vec<u8> {
        self.run_stdin(args, b"", time)
    }

    pub fn run_stdin(&self, args: &[&str], stdin: &[u8], time: i64) -> Vec<u8> {
        let out = self.try_stdin(args, stdin, time);
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    pub fn try_stdin(&self, args: &[&str], stdin: &[u8], time: i64) -> std::process::Output {
        let mut child = self
            .command(args, time)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let data = stdin.to_vec();
        let writer = std::thread::spawn(move || {
            let _ = input.write_all(&data);
        });
        let out = child.wait_with_output().unwrap();
        writer.join().unwrap();
        out
    }

    pub fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.run(args)).unwrap().trim().to_owned()
    }

    pub fn rev(&self, name: &str) -> ObjectId {
        ObjectId::from_hex(self.algorithm, &self.text(&["rev-parse", name])).unwrap()
    }

    pub fn write_file(&self, path: &str, content: &str) {
        let p = self.dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    pub fn commit_all(&self, message: &str, time: i64) -> ObjectId {
        self.run(&["add", "-A"]);
        self.run_at(&["commit", "-q", "--allow-empty", "-m", message], time);
        self.rev("HEAD")
    }

    /// The ids in `git rev-list --objects` style output (first token of each line, boundary `-` dropped).
    pub fn id_set(&self, out: &[u8], skip_boundary: bool) -> std::collections::HashSet<ObjectId> {
        String::from_utf8_lossy(out)
            .lines()
            .filter(|l| !(skip_boundary && l.starts_with('-')))
            .map(|l| l.trim_start_matches('-').split(' ').next().unwrap())
            .filter(|t| !t.is_empty())
            .map(|t| ObjectId::from_hex(self.algorithm, t).unwrap())
            .collect()
    }

    pub fn fsck(&self) {
        let out = self.try_stdin(&["fsck", "--strict", "--no-dangling"], b"", 0);
        assert!(
            out.status.success(),
            "fsck failed: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn body(file: usize, rev: usize) -> String {
    let mut s = String::new();
    for i in 0..300 {
        if (i + file) % 41 == rev % 41 && rev > 0 {
            s.push_str(&format!(
                "changed line {i} of file {file} in revision {rev}\n"
            ));
        } else {
            s.push_str(&format!(
                "line {i} of file {file}: some text that compresses and deltas well\n"
            ));
        }
    }
    s
}

pub struct History {
    pub c: [ObjectId; 5],
    pub side: ObjectId,
    pub tag: ObjectId,
    pub tag_of_tag: ObjectId,
}

/// A history with a side branch merged back, shared subtrees, an empty file and two annotated tags.
pub fn build_history(g: &Git) -> History {
    let t = 1_700_000_000;
    g.write_file("a/x.txt", &body(1, 0));
    g.write_file("a/y.txt", &body(2, 0));
    g.write_file("b/z.txt", &body(3, 0));
    g.write_file("b/deep/w.txt", &body(4, 0));
    g.write_file("empty", "");
    g.write_file("README", "readme\n");
    let c1 = g.commit_all("c1", t + 10);
    g.write_file("a/x.txt", &body(1, 1));
    g.write_file("c/new.txt", &body(5, 0));
    let c2 = g.commit_all("c2", t + 20);
    g.run(&["checkout", "-q", "-b", "side", &c1.to_hex()]);
    g.write_file("b/z.txt", &body(3, 2));
    let c3 = g.commit_all("c3", t + 30);
    g.run(&["checkout", "-q", "main"]);
    g.run_at(
        &["merge", "-q", "--no-edit", "-m", "c4 merge", "side"],
        t + 40,
    );
    let c4 = g.rev("HEAD");
    g.write_file("a/x.txt", &body(1, 3));
    let c5 = g.commit_all("c5", t + 50);
    g.run_at(
        &["tag", "-a", "-m", "release one", "v1", &c2.to_hex()],
        t + 60,
    );
    g.run_at(&["tag", "-a", "-m", "tag of tag", "v1.1", "v1"], t + 70);
    History {
        c: [c1, c2, c3, c4, c5],
        side: c3,
        tag: g.rev("v1"),
        tag_of_tag: g.rev("v1.1"),
    }
}

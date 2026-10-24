//! `pyn diff` and `pyn show` with `--shortstat`, `-w` and `--check` on a git repository compared with real git on the
//! same repository: output and exit code. Real git is the oracle in these tests only.

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
    format: &'static str,
}

impl Fixture {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        init(&repo, format, false);
        let f = Self {
            _tmp: tmp,
            repo,
            format,
        };
        f.build();
        f
    }

    fn write(&self, path: &str, content: &[u8]) {
        if let Some(dir) = self.repo.join(path).parent() {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(self.repo.join(path), content).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let out = git(&self.repo)
            .env("GIT_AUTHOR_NAME", "Ann")
            .env("GIT_AUTHOR_EMAIL", "ann@example.com")
            .env("GIT_COMMITTER_NAME", "Ann")
            .env("GIT_COMMITTER_EMAIL", "ann@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {:?} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// Three commits, then staged and unstaged edits: whitespace-only, mixed, trailing blanks, a binary file.
    fn build(&self) {
        self.write("a.txt", b"a b\nkeep\nx\ty\nlast\n");
        self.write("ws.txt", b"q\nr\n");
        self.write("blank.txt", b"p\nq\n");
        self.write("bin.dat", b"x\0y");
        self.write("gone.txt", b"bye\n");
        self.commit("one");
        self.write("a.txt", b"ab\nkeep  \nx y\nlast\n");
        self.write("ws.txt", b"q \n r\n");
        self.write("blank.txt", b"p\nq\n\n\n");
        self.write(
            "mix.txt",
            b" \tx  \n\t \ty\n \t\n  \tz\nz \t\n\r\nc\r\nnoeol  ",
        );
        self.write("bin.dat", b"x\0z");
        self.git(&["rm", "-q", "gone.txt"]);
        self.commit("two");
        self.write("a.txt", b"ab\nkeep\n\tx  y\nlast\nmore \n");
        self.git(&["add", "a.txt"]);
        self.git(&["commit", "-q", "-m", "three"]);
        self.write("ws.txt", b"q\nr\n");
        self.write("mix.txt", b"fine\n");
        self.git(&["add", "ws.txt", "mix.txt"]);
        self.write("ws.txt", b"q \nr\n\n");
    }

    fn same(&self, args: &[&str]) {
        let want = git(&self.repo)
            .env("TZ", "UTC")
            .args(args)
            .output()
            .unwrap();
        let got = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.repo)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.repo.parent().unwrap())
            .env("TZ", "UTC")
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&got.stderr);
        assert_eq!(
            got.status.code(),
            want.status.code(),
            "{} {args:?}: {err}",
            self.format
        );
        assert_eq!(
            String::from_utf8_lossy(&got.stdout),
            String::from_utf8_lossy(&want.stdout),
            "{} {args:?}",
            self.format
        );
    }
}

// Git versions differ on whether -w drops a whitespace-only file from --name-only, --name-status and --check,
// so those cases never include ws.txt here; see `whitespace_only_files_under_w`.
fn each_format(check: impl Fn(&Fixture)) {
    for format in FORMATS {
        check(&Fixture::new(format));
    }
}

#[test]
fn shortstat_matches_git() {
    each_format(|f| {
        for args in [
            &["diff", "--shortstat"][..],
            &["diff", "--shortstat", "--cached"],
            &["diff", "--shortstat", "HEAD~2", "HEAD"],
            &["diff", "--shortstat", "HEAD~1", "HEAD", "--", "a.txt"],
            &["diff", "--shortstat", "HEAD", "HEAD"],
            &["show", "--shortstat"],
            &["show", "--shortstat", "HEAD~1"],
            &["show", "--shortstat", "HEAD~2"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn ignoring_whitespace_matches_git() {
    each_format(|f| {
        for args in [
            &["diff", "-w"][..],
            &["diff", "-w", "--cached"],
            &["diff", "-w", "HEAD~2", "HEAD"],
            &["diff", "-w", "-U0", "HEAD~2", "HEAD"],
            &["diff", "-w", "--stat", "HEAD~2", "HEAD"],
            &["diff", "-w", "--shortstat", "HEAD~2", "HEAD"],
            &["diff", "-w", "--stat"],
            &[
                "diff",
                "-w",
                "--name-only",
                "HEAD~2",
                "HEAD",
                "--",
                "a.txt",
                "mix.txt",
            ],
            &["diff", "-w", "--name-status", "--", "a.txt", "mix.txt"],
            &["diff", "-w", "--exit-code", "--", "ws.txt"],
            &["diff", "--exit-code", "--", "ws.txt"],
            &["diff", "-w", "--quiet", "--", "ws.txt"],
            &["diff", "--ignore-all-space", "HEAD~1", "HEAD"],
            &["show", "-w"],
            &["show", "-w", "HEAD~1"],
            &["show", "-w", "--stat", "HEAD~1"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn check_matches_git() {
    each_format(|f| {
        for args in [
            &["diff", "--check"][..],
            &["diff", "--check", "--cached"],
            &["diff", "--check", "HEAD~2", "HEAD"],
            &["diff", "--check", "HEAD~1", "HEAD", "--", "a.txt"],
            &["diff", "--check", "HEAD~2", "HEAD~1"],
            &[
                "diff", "--check", "-w", "HEAD~2", "HEAD", "--", "a.txt", "mix.txt",
            ],
            &["diff", "--check", "HEAD", "HEAD"],
            &["diff", "--check", "--", "a.txt"],
            &["show", "--check"],
            &["show", "--check", "HEAD~1"],
            &["show", "--check", "HEAD~2"],
        ] {
            f.same(args);
        }
    });
}

#[test]
fn incompatible_formats_are_usage_errors() {
    let f = Fixture::new("sha1");
    for args in [
        &["diff", "--check", "--stat"][..],
        &["diff", "--check", "--name-only"],
        &["diff", "--shortstat", "--name-status"],
        &["diff", "--shortstat", "--stat"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&f.repo)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", f.repo.parent().unwrap())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn numstat_combinations_are_usage_errors() {
    let f = Fixture::new("sha1");
    for command in [&["diff"][..], &["show"], &["stash", "show"]] {
        for other in [
            "--stat",
            "--shortstat",
            "--name-only",
            "--name-status",
            "--check",
        ] {
            let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
                .current_dir(&f.repo)
                .args(command)
                .args(["--numstat", other])
                .env_clear()
                .env("PATH", path_env())
                .env("HOME", f.repo.parent().unwrap())
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(129), "{command:?} {other}");
            assert!(out.stdout.is_empty(), "{command:?} {other}");
        }
    }
}

#[test]
fn whitespace_only_files_under_w() {
    let f = Fixture::new("sha1");
    let pyn = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&f.repo)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", f.repo.parent().unwrap())
            .output()
            .unwrap();
        (out.status.code(), String::from_utf8(out.stdout).unwrap())
    };
    let range = ["HEAD~2", "HEAD", "--", "ws.txt"];
    for (flag, want) in [
        ("--name-only", "ws.txt\n"),
        ("--name-status", "M\tws.txt\n"),
        ("--check", "ws.txt:1: trailing whitespace.\n+q \n"),
    ] {
        let mut args = vec!["diff", "-w", flag];
        args.extend(range);
        let (code, out) = pyn(&args);
        assert_eq!(out, want, "{flag}");
        assert_eq!(code, Some(if flag == "--check" { 2 } else { 0 }), "{flag}");
    }
}

#[test]
fn stat_prints_binary_sizes_as_git_does() {
    each_format(|f| {
        f.write("grow.bin", b"\0ab");
        f.write("shrink.bin", &[0u8; 1234]);
        f.write("same.bin", b"\0\x01\x02");
        f.write("gone.bin", &[5u8; 300]);
        f.write(
            "dir/with/a/rather/long/directory/name/and_a_long_file_name.bin",
            b"\0\0",
        );
        f.commit("bins");
        f.write("grow.bin", &[1u8; 100_000]);
        f.write("shrink.bin", b"");
        f.write("same.bin", b"\0\x02\x01");
        f.write("added.bin", b"\0\0\0\0\0");
        f.write("t.txt", b"one\ntwo\n");
        f.write(
            "dir/with/a/rather/long/directory/name/and_a_long_file_name.bin",
            b"\0\0\0\0\0\0\0",
        );
        f.git(&["rm", "-q", "gone.bin"]);
        f.commit("bins2");
        f.write("grow.bin", b"\0");
        f.write("t.txt", b"one\n");
        f.write("same.bin", b"\0\x03\x03\x03");
        f.write("new.bin", &[9u8; 7]);
        f.git(&["add", "new.bin"]);
        f.write("grow.bin", b"\0xy");
        // git 2.43 cannot stash in a sha256 repository.
        if f.format == "sha1" {
            f.git(&["stash", "push", "-q"]);
            f.same(&["stash", "show", "--stat"]);
            f.same(&["stash", "show", "--numstat"]);
            f.git(&["stash", "pop", "-q", "--index"]);
        }
        for args in [
            &["diff", "--stat"][..],
            &["diff", "--cached", "--stat"],
            &["diff", "--stat", "HEAD~1", "HEAD"],
            &["diff", "--stat", "HEAD~2", "HEAD"],
            &["diff", "--stat", "HEAD~1", "HEAD", "--", "same.bin"],
            &[
                "diff",
                "--stat",
                "HEAD~1",
                "HEAD",
                "--",
                "grow.bin",
                "shrink.bin",
            ],
            &["diff", "--numstat", "HEAD~1", "HEAD"],
            &["diff", "--numstat", "--cached"],
            &["show", "--stat"],
            &["show", "--stat", "HEAD~1"],
            &["show", "--numstat"],
        ] {
            f.same(args);
        }
    });
}

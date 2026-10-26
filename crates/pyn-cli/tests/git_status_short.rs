//! `pyn status -s`, `--porcelain` and `-b` on a git repository compared with the real `git status` on the same
//! repository: the set of lines, never git's wording. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new(format: &str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let repo = root.join("repo");
        init(&repo, format, false);
        Self {
            _tmp: tmp,
            root,
            repo,
        }
    }

    fn write(&self, path: &str, content: &str) {
        let file = self.repo.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        run(&self.repo, args)
    }

    /// The oracle's lines, with git's redirect warnings removed.
    fn oracle(&self, cwd: &Path, args: &[&str]) -> Vec<String> {
        let out = git(cwd).arg("status").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        lines(&String::from_utf8_lossy(&out.stdout))
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .arg("status")
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn pyn_lines(&self, cwd: &Path, args: &[&str]) -> Vec<String> {
        let (code, out, err) = self.pyn(cwd, args);
        assert_eq!(code, 0, "pyn status {args:?}: {err}");
        lines(&out)
    }

    /// Both tools must print the same set of lines, with the branch line (if any) first and equal.
    fn same(&self, cwd: &Path, args: &[&str]) {
        let (mut want, mut got) = (self.oracle(cwd, args), self.pyn_lines(cwd, args));
        let branch = |v: &mut Vec<String>| {
            let first = v.first().filter(|l| l.starts_with("## ")).cloned();
            if first.is_some() {
                v.remove(0);
            }
            first
        };
        assert_eq!(
            branch(&mut got),
            branch(&mut want),
            "branch line for {args:?}"
        );
        want.sort();
        got.sort();
        assert_eq!(got, want, "status {args:?} in {}", cwd.display());
    }
}

fn lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.starts_with("warning: redirecting to "))
        .map(str::to_owned)
        .collect()
}

/// A repository with every kind of change git's short format can show.
fn changed(format: &str) -> Fixture {
    let f = Fixture::new(format);
    for (p, c) in [
        ("a.txt", "a\n"),
        ("b.txt", "b\n"),
        ("c.txt", "c\n"),
        ("d/e.txt", "e\n"),
        ("d/f.txt", "f\n"),
        ("d/g.txt", "g\n"),
        ("s p.txt", "sp\n"),
        ("gone.txt", "gone\n"),
        ("k.txt", "k\n"),
        ("tab\u{e9}.txt", "t\n"),
    ] {
        f.write(p, c);
    }
    symlink("a.txt", f.repo.join("link")).unwrap();
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "base"]);

    f.write("b.txt", "b2\n");
    f.git(&["add", "b.txt"]);
    f.write("b.txt", "b3\n");
    f.write("c.txt", "c2\n");
    f.write("new.txt", "n\n");
    f.git(&["add", "new.txt"]);
    f.git(&["rm", "-q", "d/e.txt"]);
    fs::remove_file(f.repo.join("d/f.txt")).unwrap();
    f.git(&["rm", "-q", "--cached", "k.txt"]);
    f.write("intent.txt", "i\n");
    f.git(&["add", "-N", "intent.txt"]);
    f.write("s p.txt", "sp2\n");
    fs::remove_file(f.repo.join("link")).unwrap();
    f.write("link", "now a file\n");
    f.write("u.txt", "u\n");
    f.write("ud/x.txt", "x\n");
    f.write("d/untracked.txt", "x\n");
    f.write("with space.txt", "x\n");
    f.write("caf\u{e9}.txt", "x\n");
    f.git(&["rm", "-q", "--cached", "tab\u{e9}.txt"]);
    f
}

const FORMATS: [&str; 2] = ["sha1", "sha256"];

#[test]
fn short_and_porcelain_match_git() {
    for format in FORMATS {
        let f = changed(format);
        for args in [
            &["-s"][..],
            &["--short"],
            &["--porcelain"],
            &["--porcelain=v1"],
            &["-s", "-b"],
            &["-sb"],
            &["--porcelain", "-b"],
            &["--porcelain", "--branch"],
        ] {
            f.same(&f.repo, args);
        }
    }
}

#[test]
fn pathspecs_limit_the_listing_like_git() {
    let f = changed("sha1");
    for spec in [
        &["d"][..],
        &["d/"],
        &["a.txt", "b.txt"],
        &["d/g.txt"],
        &["c.txt", "d", "u.txt"],
        &["nothing-here"],
        &["ud"],
        &["--", "b.txt"],
        &["."],
    ] {
        for flag in ["-s", "--porcelain"] {
            let mut args = vec![flag];
            args.extend_from_slice(spec);
            f.same(&f.repo, &args);
        }
    }
}

#[test]
fn short_paths_follow_where_you_stand_and_porcelain_does_not() {
    let f = changed("sha1");
    let sub = f.repo.join("d");
    f.same(&sub, &["-s"]);
    f.same(&sub, &["--porcelain"]);
    f.same(&sub, &["-s", "."]);
    f.same(&sub, &["-s", "../c.txt", "f.txt"]);
    f.same(&sub, &["--porcelain", "../c.txt"]);
}

#[test]
fn a_clean_work_tree_prints_nothing_but_the_branch_line() {
    let f = Fixture::new("sha1");
    f.write("a.txt", "a\n");
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.same(&f.repo, &["-s"]);
    f.same(&f.repo, &["-s", "-b"]);
    assert!(f.pyn_lines(&f.repo, &["--porcelain"]).is_empty());
}

#[test]
fn every_kind_of_unmerged_path_matches_git() {
    for format in FORMATS {
        let f = Fixture::new(format);
        for p in ["both.txt", "du.txt", "ud.txt", "gone-both.txt"] {
            f.write(p, "base\n");
        }
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "base"]);
        f.git(&["branch", "other"]);

        f.write("both.txt", "ours\n");
        f.write("added-both.txt", "ours\n");
        f.write("ud.txt", "ours\n");
        f.git(&["rm", "-q", "du.txt", "gone-both.txt"]);
        f.write("added-ours.txt", "x\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "ours"]);

        f.git(&["switch", "-q", "other"]);
        f.write("both.txt", "theirs\n");
        f.write("added-both.txt", "theirs\n");
        f.write("du.txt", "theirs\n");
        f.git(&["rm", "-q", "ud.txt", "gone-both.txt"]);
        f.write("added-theirs.txt", "y\n");
        f.git(&["add", "."]);
        f.git(&["commit", "-q", "-m", "theirs"]);

        f.git(&["switch", "-q", "main"]);
        let merge = git(&f.repo).args(["merge", "other"]).output().unwrap();
        assert_eq!(merge.status.code(), Some(1), "{format}");
        let want = f.oracle(&f.repo, &["--porcelain"]);
        for code in ["UU", "AA", "DU", "UD"] {
            assert!(want.iter().any(|l| l.starts_with(code)), "{code}: {want:?}");
        }
        f.same(&f.repo, &["-s"]);
        f.same(&f.repo, &["--porcelain", "-b"]);
        f.same(&f.repo, &["-s", "both.txt", "du.txt"]);
    }
}

#[test]
fn the_remaining_unmerged_pairs_match_git() {
    let f = Fixture::new("sha1");
    for p in ["dd.txt", "au.txt"] {
        f.write(p, "base\n");
    }
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "base"]);
    f.git(&["rm", "-q", "dd.txt"]);
    f.git(&["rm", "-q", "--cached", "au.txt"]);
    let blob = |c: &str| {
        let file = f.repo.join("blob.tmp");
        fs::write(&file, c).unwrap();
        f.git(&["hash-object", "-w", "blob.tmp"]).trim().to_owned()
    };
    let id = blob("x\n");
    let info = format!(
        "0 {z} 0\tau.txt\n100644 {id} 2\tau.txt\n100644 {id} 3\tua.txt\n100644 {id} 1\tdd.txt\n",
        z = "0".repeat(40)
    );
    let mut child = git(&f.repo)
        .args(["update-index", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(info.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    fs::remove_file(f.repo.join("blob.tmp")).unwrap();
    let want = f.oracle(&f.repo, &["--porcelain"]);
    for code in ["AU", "UA", "DD"] {
        assert!(want.iter().any(|l| l.starts_with(code)), "{code}: {want:?}");
    }
    f.same(&f.repo, &["--porcelain"]);
}

fn with_upstream() -> Fixture {
    let f = Fixture::new("sha1");
    f.write("a.txt", "a\n");
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "one"]);
    let bare = f.root.join("remote.git");
    init(&bare, "sha1", true);
    f.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
    f.git(&["push", "-q", "-u", "origin", "main"]);
    f
}

#[test]
fn the_branch_line_counts_ahead_and_behind_like_git() {
    let f = with_upstream();
    f.same(&f.repo, &["-sb"]);

    f.write("b.txt", "b\n");
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "ahead"]);
    f.same(&f.repo, &["-sb"]);
    f.same(&f.repo, &["--porcelain", "-b"]);

    let other = f.root.join("other");
    run(
        &f.root,
        &[
            "clone",
            "-q",
            f.root.join("remote.git").to_str().unwrap(),
            "other",
        ],
    );
    fs::write(other.join("c.txt"), "c\n").unwrap();
    run(&other, &["add", "."]);
    run(&other, &["commit", "-q", "-m", "theirs"]);
    run(&other, &["push", "-q", "origin", "main"]);
    f.git(&["fetch", "-q", "origin"]);
    f.same(&f.repo, &["-sb"]);

    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.same(&f.repo, &["-sb"]);

    f.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    f.same(&f.repo, &["-sb"]);
}

#[test]
fn the_branch_line_without_an_upstream_or_a_branch_matches_git() {
    let f = Fixture::new("sha1");
    f.same(&f.repo, &["-sb"]);
    f.write("a.txt", "a\n");
    f.git(&["add", "."]);
    f.same(&f.repo, &["-sb"]);
    f.git(&["commit", "-q", "-m", "one"]);
    f.same(&f.repo, &["-sb"]);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.git(&["config", "branch.topic.remote", "."]);
    f.git(&["config", "branch.topic.merge", "refs/heads/main"]);
    f.write("b.txt", "b\n");
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "two"]);
    f.same(&f.repo, &["-sb"]);
    f.git(&["checkout", "-q", "--detach"]);
    f.same(&f.repo, &["-sb"]);
    f.same(&f.repo, &["--porcelain", "-b"]);
}

#[test]
fn unsupported_variants_are_usage_errors() {
    let f = changed("sha1");
    for (args, status) in [
        (&["-z"][..], 129),
        (&["--porcelain=v2"], 129),
        (&["--porcelain=v3"], 128),
    ] {
        let (code, out, err) = f.pyn(&f.repo, args);
        assert_eq!(code, status, "{args:?}: {out}{err}");
        assert!(out.is_empty(), "{args:?}: {out}");
    }
}

#[test]
fn glob_and_magic_pathspecs_limit_the_listing_like_git() {
    let f = changed("sha1");
    for args in [
        &["-s", "*.txt"][..],
        &["-s", "d/*"],
        &["-s", "a?.txt"],
        &["-s", "[ab].txt"],
        &["-s", ":(glob)*.txt"],
        &["-s", ":(glob)d/*"],
        &["-s", ":!b.txt"],
        &["-s", ":(icase)B.TXT"],
        &["--porcelain", ":(exclude)d"],
    ] {
        f.same(&f.repo, args);
    }
}

#[test]
fn a_file_named_like_a_glob_is_taken_literally() {
    let f = Fixture::new("sha1");
    f.write("a.txt", "a\n");
    f.git(&["add", "."]);
    f.git(&["commit", "-q", "-m", "one"]);
    f.write("star*.txt", "s\n");
    let got = f.pyn_lines(&f.repo, &["--porcelain", "star*.txt"]);
    assert_eq!(got, ["?? star*.txt"]);
}

#[test]
fn the_table_is_unchanged_and_takes_pathspecs() {
    let f = changed("sha1");
    let out = f.pyn_lines(&f.repo, &[]);
    assert!(out[0].starts_with("On branch main"), "{out:?}");
    let limited = f.pyn_lines(&f.repo, &["c.txt"]).join("\n");
    assert!(
        limited.contains("c.txt") && !limited.contains("b.txt"),
        "{limited}"
    );
    let branch_only = f.pyn_lines(&f.repo, &["-b"]);
    assert_eq!(branch_only, out);
}

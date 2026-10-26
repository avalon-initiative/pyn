//! `pyn sparse-checkout` and the commands that respect it, on a git repository. Every scenario runs on two copies of one
//! repository, pyn on one and the real git on the other, and compares the state git reads: the patterns file, the
//! skip-worktree tags of `git ls-files -t` and the files in the work tree. Real git is the oracle in these tests only;
//! comparisons are of behaviour, not of git's exact bytes.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::{FORMATS, git, init, path_env};
use tempfile::{TempDir, tempdir};

struct Run {
    code: i32,
    out: String,
    err: String,
}

struct Pair {
    _tmp: TempDir,
    root: PathBuf,
    pyn: PathBuf,
    oracle: PathBuf,
    format: &'static str,
}

fn write(dir: &Path, path: &str, text: &str) {
    let file = dir.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

/// The message without the line that names the setting, which git words differently across versions.
fn advice(err: &str) -> Vec<&str> {
    err.lines()
        .filter(|l| !l.starts_with("hint: Disable this message"))
        .collect()
}

fn files(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let path = e.path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.push(path.strip_prefix(base).unwrap().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

impl Pair {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let oracle = root.join("oracle");
        init(&oracle, format, false);
        for (p, t) in [
            ("x", "x\n"),
            ("a/y", "y\n"),
            ("a/b/z", "z\n"),
            ("c/w", "w\n"),
            ("c/deep/q", "q\n"),
            ("d/v", "v\n"),
        ] {
            write(&oracle, p, t);
        }
        let g = |args: &[&str]| {
            let out = git(&oracle)
                .args(args)
                .env("GIT_AUTHOR_DATE", "1700000000 +0000")
                .env("GIT_COMMITTER_DATE", "1700000000 +0000")
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
        };
        g(&["add", "-A"]);
        g(&["commit", "-q", "-m", "base"]);
        g(&["switch", "-c", "feat"]);
        write(&oracle, "a/y", "changed\n");
        write(&oracle, "a/b/new", "n\n");
        write(&oracle, "c/new", "n\n");
        write(&oracle, "d/extra", "e\n");
        fs::remove_file(oracle.join("d/v")).unwrap();
        fs::remove_file(oracle.join("c/w")).unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-q", "-m", "feat"]);
        g(&["switch", "-"]);
        let pyn = root.join("pyn");
        let status = Command::new("cp")
            .args(["-a"])
            .arg(&oracle)
            .arg(&pyn)
            .status()
            .unwrap();
        assert!(status.success());
        Self {
            _tmp: tmp,
            root,
            pyn,
            oracle,
            format,
        }
    }

    fn run(&self, mut c: Command, dir: &Path, args: &[&str], stdin: &str) -> Run {
        c.args(args)
            .current_dir(dir)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "n")
            .env("GIT_AUTHOR_EMAIL", "n@e")
            .env("GIT_COMMITTER_NAME", "n")
            .env("GIT_COMMITTER_EMAIL", "n@e")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        Run {
            code: out.status.code().unwrap(),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn pyn_in(&self, dir: &Path, args: &[&str], stdin: &str) -> Run {
        self.run(Command::new(env!("CARGO_BIN_EXE_pyn")), dir, args, stdin)
    }

    fn git_in(&self, dir: &Path, args: &[&str], stdin: &str) -> Run {
        self.run(Command::new("git"), dir, args, stdin)
    }

    /// Runs `args` with pyn on one copy and git on the other and requires the same exit status.
    fn both(&self, args: &[&str]) -> (Run, Run) {
        self.both_in("", args, "")
    }

    fn both_in(&self, sub: &str, args: &[&str], stdin: &str) -> (Run, Run) {
        let got = self.pyn_in(&self.pyn.join(sub), args, stdin);
        let want = self.git_in(&self.oracle.join(sub), args, stdin);
        assert_eq!(
            got.code, want.code,
            "{} status of {args:?}: pyn {:?}, git {:?}",
            self.format, got.err, want.err
        );
        (got, want)
    }

    fn tags(&self, dir: &Path) -> String {
        self.git_in(dir, &["ls-files", "-t"], "").out
    }

    /// The patterns file, the tags and the files must be identical on both copies.
    fn same_state(&self, what: &str) {
        let read = |d: &Path| fs::read_to_string(d.join(".git/info/sparse-checkout")).ok();
        assert_eq!(
            read(&self.pyn),
            read(&self.oracle),
            "{} patterns {what}",
            self.format
        );
        assert_eq!(
            self.tags(&self.pyn),
            self.tags(&self.oracle),
            "{} tags {what}",
            self.format
        );
        assert_eq!(
            files(&self.pyn),
            files(&self.oracle),
            "{} files {what}",
            self.format
        );
        let porcelain = |d: &Path| self.git_in(d, &["status", "--porcelain"], "").out;
        assert_eq!(
            porcelain(&self.pyn),
            porcelain(&self.oracle),
            "{} status {what}",
            self.format
        );
        let enabled = |d: &Path| self.git_in(d, &["config", "core.sparseCheckout"], "").out;
        assert_eq!(enabled(&self.pyn), enabled(&self.oracle), "{what}");
        let cone = |d: &Path| {
            self.git_in(d, &["config", "core.sparseCheckoutCone"], "")
                .out
        };
        assert_eq!(cone(&self.pyn), cone(&self.oracle), "{what}");
        let ours = self.pyn_in(&self.pyn, &["ls-files", "-t"], "").out;
        assert_eq!(ours, self.tags(&self.pyn), "pyn ls-files -t {what}");
        let fsck = self.git_in(&self.pyn, &["fsck", "--full"], "");
        assert_eq!(fsck.code, 0, "{}", fsck.err);
    }

    fn set_both(&self, args: &[&str]) {
        let (got, _) = self.both(args);
        assert_eq!(got.code, 0, "{args:?}: {}", got.err);
    }
}

#[test]
fn cone_set_add_list_disable_match_git() {
    for format in FORMATS {
        let p = Pair::new(format);
        p.set_both(&["sparse-checkout", "set", "a/b", "d"]);
        p.same_state("after set");
        assert_eq!(
            p.tags(&p.pyn)
                .lines()
                .filter(|l| l.starts_with('S'))
                .count(),
            2
        );
        let (got, want) = p.both(&["sparse-checkout", "list"]);
        assert_eq!(got.out, want.out);
        p.set_both(&["sparse-checkout", "add", "c/deep"]);
        p.same_state("after add");
        let (got, want) = p.both(&["sparse-checkout", "list"]);
        assert_eq!(got.out, want.out);
        p.set_both(&["sparse-checkout", "set", "a"]);
        p.same_state("after replacing the set");
        p.set_both(&["sparse-checkout", "reapply"]);
        p.same_state("after reapply");
        p.set_both(&["sparse-checkout", "disable"]);
        p.same_state("after disable");
        assert_eq!(files(&p.pyn).len(), 6);
        let (got, want) = p.both(&["sparse-checkout", "list"]);
        assert_eq!(got.err.is_empty(), want.err.is_empty());
        assert!(got.err.contains("not sparse"), "{}", got.err);
    }
}

#[test]
fn init_keeps_root_files_and_is_read_by_git() {
    for format in FORMATS {
        let p = Pair::new(format);
        p.set_both(&["sparse-checkout", "init", "--cone"]);
        p.same_state("after init");
        assert_eq!(files(&p.pyn), vec!["x".to_string()]);
        p.set_both(&["sparse-checkout", "init", "--no-cone"]);
        p.same_state("after init --no-cone");
    }
}

#[test]
fn non_cone_patterns_match_git() {
    for format in FORMATS {
        let p = Pair::new(format);
        let set = [
            "sparse-checkout",
            "set",
            "--no-cone",
            "/a/",
            "!/a/b/",
            "/x",
            "c/deep/",
        ];
        p.set_both(&set);
        p.same_state("after non-cone set");
        let (got, want) = p.both(&["sparse-checkout", "list"]);
        assert_eq!(got.out, want.out);
        p.set_both(&["sparse-checkout", "add", "/d/v"]);
        p.same_state("after non-cone add");
        p.set_both(&["sparse-checkout", "set", "--no-cone", "*.txt", "/*", "!/*/"]);
        p.same_state("after set with globs");
    }
}

#[test]
fn pyn_reads_the_state_git_wrote() {
    for format in FORMATS {
        let p = Pair::new(format);
        let o = &p.oracle;
        assert_eq!(
            p.git_in(o, &["sparse-checkout", "set", "a/b", "d"], "")
                .code,
            0
        );
        let listed = p.pyn_in(o, &["sparse-checkout", "list"], "");
        assert_eq!(listed.out, "a/b\nd\n", "{}", listed.err);
        let tags = p.pyn_in(o, &["ls-files", "-t"], "");
        assert_eq!(tags.out, p.tags(o));
        let status = p.pyn_in(o, &["status"], "");
        let want = p.git_in(o, &["status"], "").out;
        let percent = want
            .lines()
            .find(|l| l.contains("sparse checkout"))
            .unwrap();
        assert!(status.out.contains(percent), "{}", status.out);
        let moved = p.pyn_in(o, &["switch", "feat"], "");
        assert_eq!(moved.code, 0, "{}", moved.err);
        let git_view = p.git_in(o, &["status", "--porcelain"], "");
        assert_eq!(git_view.out, "");
        assert!(o.join("a/b/new").exists());
        assert!(!o.join("c/new").exists());
        let off = p.pyn_in(o, &["sparse-checkout", "disable"], "");
        assert_eq!(off.code, 0, "{}", off.err);
        assert!(o.join("c/new").exists());
        assert!(!p.tags(o).contains("S "));
    }
}

#[test]
fn switching_branches_keeps_the_skip_bits_right() {
    for format in FORMATS {
        let p = Pair::new(format);
        p.set_both(&["sparse-checkout", "set", "a/b", "d"]);
        p.set_both(&["switch", "feat"]);
        p.same_state("on feat");
        p.set_both(&["switch", "-"]);
        p.same_state("back on the base");
        p.set_both(&["checkout", "feat"]);
        p.same_state("checkout of feat");
        p.set_both(&["reset", "-q", "--hard", "HEAD~1"]);
        p.same_state("after reset --hard");
        p.set_both(&["merge", "feat"]);
        p.same_state("after a fast-forward merge");
    }
}

#[test]
fn reset_mixed_and_restore_staged_keep_the_bits() {
    for format in FORMATS {
        let p = Pair::new(format);
        p.set_both(&["sparse-checkout", "set", "a/b", "d"]);
        p.set_both(&["reset", "-q", "feat"]);
        p.same_state("after reset --mixed");
        let g = p.git_in(&p.oracle, &["reset", "-q", "--hard"], "");
        assert_eq!(g.code, 0);
        let o = p.pyn_in(&p.pyn, &["reset", "-q", "--hard"], "");
        assert_eq!(o.code, 0, "{}", o.err);
        p.same_state("after reset --hard");
        p.set_both(&["restore", "--staged", "--source", "feat", "."]);
        p.same_state("after restore --staged");
    }
}

#[test]
fn work_tree_commands_leave_skipped_files_alone() {
    for format in FORMATS {
        let p = Pair::new(format);
        p.set_both(&["sparse-checkout", "set", "a/b", "d"]);
        for dir in [&p.pyn, &p.oracle] {
            write(dir, "a/b/z", "edited\n");
            write(dir, "d/fresh", "f\n");
        }
        p.same_state("after edits");
        p.set_both(&["add", "-A"]);
        p.same_state("after add -A");
        p.set_both(&["commit", "-q", "-m", "work"]);
        p.same_state("after commit");
        let (got, want) = p.both(&["status", "--porcelain"]);
        assert_eq!(got.code, want.code);
        let (got, want) = p.both(&["add", "c/w"]);
        assert_eq!(got.code, 1);
        assert_eq!(advice(&got.err), advice(&want.err), "{format} add outside");
        let (got, want) = p.both(&["rm", "c/w"]);
        assert_eq!(got.code, 1);
        assert_eq!(advice(&got.err), advice(&want.err), "{format} rm outside");
        p.same_state("after refused add and rm");
        let (got, want) = p.both(&["ls-files", "-v"]);
        assert_eq!(got.out, want.out);
        let (got, want) = p.both(&["ls-files", "-t", "--sparse"]);
        assert_eq!(got.out, want.out);
        let (got, want) = p.both(&["rm", "-q", "d/v"]);
        assert_eq!(got.out, want.out);
        p.same_state("after rm of an included file");
    }
}

#[test]
fn local_changes_outside_the_patterns_are_left() {
    for format in FORMATS {
        let p = Pair::new(format);
        p.set_both(&["sparse-checkout", "set", "c", "d"]);
        for dir in [&p.pyn, &p.oracle] {
            write(dir, "c/w", "mine\n");
        }
        let (got, want) = p.both(&["sparse-checkout", "set", "d"]);
        assert!(got.err.contains("c/w"), "{}", got.err);
        assert!(want.err.contains("c/w"), "{}", want.err);
        p.same_state("with a dirty file left behind");
        for dir in [&p.pyn, &p.oracle] {
            p.git_in(dir, &["checkout", "--", "c/w"], "");
            p.git_in(dir, &["update-index", "--refresh"], "");
        }
        p.set_both(&["sparse-checkout", "reapply"]);
        p.same_state("after reapply");
    }
}

#[test]
fn cone_arguments_are_checked() {
    for args in [
        vec!["sparse-checkout", "set", "/a"],
        vec!["sparse-checkout", "set", "a*"],
        vec!["sparse-checkout", "set", "x"],
        vec!["sparse-checkout", "add", "a"],
        vec!["sparse-checkout", "reapply"],
        vec!["sparse-checkout", "list"],
    ] {
        let p = Pair::new("sha1");
        let (got, want) = p.both(&args);
        assert_eq!(got.err, want.err, "{args:?}");
        assert_eq!(files(&p.pyn), files(&p.oracle));
    }
    let p = Pair::new("sha1");
    let got = p.pyn_in(
        &p.pyn,
        &["sparse-checkout", "set", "--sparse-index", "a"],
        "",
    );
    assert_ne!(got.code, 0);
    assert_eq!(files(&p.pyn), files(&p.oracle));
}

#[test]
fn check_rules_matches_git() {
    for format in FORMATS {
        let p = Pair::new(format);
        p.set_both(&["sparse-checkout", "set", "a/b", "d"]);
        let paths = "x\na/y\na/b/z\na/b/deep/f\nc/w\nd/v\nd/sub/f\nnewdir/f\n";
        let (got, want) = p.both_in("", &["sparse-checkout", "check-rules"], paths);
        assert_eq!(got.out, want.out, "{format}");
        let (got, want) = p.both_in(
            "",
            &["sparse-checkout", "check-rules", "-z"],
            &paths.replace('\n', "\0"),
        );
        assert_eq!(got.out, want.out, "{format} -z");
        p.set_both(&[
            "sparse-checkout",
            "set",
            "--no-cone",
            "/a/",
            "!/a/b/",
            "*.v",
        ]);
        let (got, want) = p.both_in("", &["sparse-checkout", "check-rules"], paths);
        assert_eq!(got.out, want.out, "{format} non-cone");
        let rules = p.root.join("rules");
        fs::write(&rules, "/*\n!/*/\n/c/\n").unwrap();
        let file = rules.to_str().unwrap();
        let (got, want) = p.both_in(
            "",
            &[
                "sparse-checkout",
                "check-rules",
                "--cone",
                "--rules-file",
                file,
            ],
            paths,
        );
        assert_eq!(got.out, want.out, "{format} rules file");
    }
}

#[test]
fn clone_sparse_checks_out_the_root_files() {
    for format in FORMATS {
        let p = Pair::new(format);
        let src = p.oracle.to_str().unwrap().to_string();
        let pyn_copy = p.root.join("cloned-by-pyn");
        let git_copy = p.root.join("cloned-by-git");
        let got = p.pyn_in(
            &p.root,
            &["clone", "-q", "--sparse", &src, "cloned-by-pyn"],
            "",
        );
        assert_eq!(got.code, 0, "{}", got.err);
        let want = p.git_in(
            &p.root,
            &["clone", "-q", "--sparse", &src, "cloned-by-git"],
            "",
        );
        assert_eq!(want.code, 0, "{}", want.err);
        assert_eq!(files(&pyn_copy), files(&git_copy), "{format}");
        assert_eq!(p.tags(&pyn_copy), p.tags(&git_copy), "{format}");
        let read = |d: &Path| fs::read_to_string(d.join(".git/info/sparse-checkout")).unwrap();
        assert_eq!(read(&pyn_copy), read(&git_copy));
        let listed = p.git_in(&pyn_copy, &["sparse-checkout", "list"], "");
        assert_eq!(listed.code, 0, "{}", listed.err);
        let set = p.pyn_in(&pyn_copy, &["sparse-checkout", "set", "a"], "");
        assert_eq!(set.code, 0, "{}", set.err);
        assert!(pyn_copy.join("a/y").exists());
        assert_eq!(p.git_in(&pyn_copy, &["status", "--porcelain"], "").out, "");
    }
}

#[test]
fn commands_that_cannot_keep_the_bits_refuse_and_change_nothing() {
    let p = Pair::new("sha1");
    p.set_both(&["sparse-checkout", "set", "a/b", "d"]);
    write(&p.pyn, "d/v", "dirty\n");
    let before = (p.tags(&p.pyn), files(&p.pyn));
    for args in [
        vec!["stash"],
        vec!["restore", "d/v"],
        vec!["reset", "--keep", "HEAD"],
        vec!["merge", "--no-ff", "feat"],
    ] {
        let got = p.pyn_in(&p.pyn, &args, "");
        assert_ne!(got.code, 0, "{args:?}");
        assert!(got.err.contains("sparse"), "{args:?}: {}", got.err);
    }
    assert_eq!((p.tags(&p.pyn), files(&p.pyn)), before);
}

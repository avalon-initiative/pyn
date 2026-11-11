//! `add --renormalize`, `-X renormalize`, and conversion in `apply`, `checkout -p` and `restore -p` on a git repository,
//! compared with real git on identical repositories: staged blob ids, merged trees, work tree bytes and exit status
//! class. Real git is the oracle in these tests only; wording is never compared.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Pair {
    _tmp: TempDir,
    root: PathBuf,
    oracle: PathBuf,
    ours: PathBuf,
}

impl Pair {
    fn new(format: &str, config: &str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (oracle, ours) = (root.join("oracle"), root.join("ours"));
        for repo in [&oracle, &ours] {
            init(repo, format, false);
            run(repo, &["config", "user.name", "t"]);
            run(repo, &["config", "user.email", "t@example.com"]);
            run(repo, &["config", "core.filemode", "true"]);
            let mut text = fs::read_to_string(repo.join(".git/config")).unwrap();
            text.push_str("[merge]\n\tconflictStyle = diff3\n");
            text.push_str(config);
            fs::write(repo.join(".git/config"), text).unwrap();
        }
        Self {
            _tmp: tmp,
            root,
            oracle,
            ours,
        }
    }

    fn put(&self, path: &str, bytes: &[u8]) {
        for repo in [&self.oracle, &self.ours] {
            write(repo, path, bytes);
        }
    }

    fn each(&self, args: &[&str]) {
        for repo in [&self.oracle, &self.ours] {
            run(repo, args);
        }
    }

    fn pyn(&self, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.ours)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git(&self, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
        let mut child = git(&self.oracle)
            .args(args)
            .env("GIT_EDITOR", "true")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Runs `args` with both tools and requires the same exit status class.
    fn both(&self, args: &[&str]) -> (i32, String, String) {
        self.both_in(args, b"")
    }

    fn both_in(&self, args: &[&str], stdin: &[u8]) -> (i32, String, String) {
        let (want, got) = (self.git(args, stdin), self.pyn(args, stdin));
        assert_eq!(
            oracle::status_class(got.0),
            oracle::status_class(want.0),
            "{args:?}: pyn said {}",
            got.2
        );
        want
    }

    fn same_index(&self, what: &str) {
        let ls = |dir: &Path| run(dir, &["ls-files", "-s"]);
        assert_eq!(ls(&self.ours), ls(&self.oracle), "{what}: index");
    }

    fn same_head(&self, what: &str) {
        let rev = |dir: &Path| run(dir, &["rev-parse", "HEAD^{tree}"]);
        assert_eq!(rev(&self.ours), rev(&self.oracle), "{what}: HEAD tree");
    }

    fn same_tree(&self, what: &str) {
        assert_eq!(tree(&self.ours), tree(&self.oracle), "{what}: work tree");
    }

    /// Compares what differs from the index, from `HEAD` and what is untracked; `status` itself can report a converted
    /// file as modified after git's own `restore -p` leaves a stale stat entry.
    fn same_status(&self, what: &str) {
        let conflicted = !run(&self.oracle, &["ls-files", "--unmerged"]).is_empty();
        for args in [
            &["diff", "--name-status"][..],
            &["diff", "--cached", "--name-status"],
            &["ls-files", "--others", "--exclude-standard"],
        ] {
            if conflicted && args[0] == "diff" {
                continue;
            }
            let (got, want) = (self.pyn(args, b""), self.git(args, b""));
            assert_eq!(got.1, want.1, "{what}: {args:?}");
        }
    }

    fn same_all(&self, what: &str) {
        self.same_index(what);
        self.same_tree(what);
        self.same_status(what);
    }
}

fn write(repo: &Path, path: &str, bytes: &[u8]) {
    let file = repo.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, bytes).unwrap();
}

fn tree(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let path = e.path();
            if e.file_name() == ".git" {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(
                    rel,
                    format!("{:?}", String::from_utf8_lossy(&fs::read(&path).unwrap())),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

const CRLF_BASE: &[u8] = b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n";

/// Files committed with CRLF before any conversion rule exists, then a rule that would store them as LF.
fn committed_crlf(format: &str, config: &str, rules: &str) -> Pair {
    let p = Pair::new(format, config);
    p.put("a.txt", CRLF_BASE);
    p.put("b.txt", b"x\r\ny\r\n");
    p.put("lf.txt", b"p\nq\n");
    p.put("dir/c.txt", b"m\r\nn\r\n");
    p.put("keep.bin", b"\x00\r\n\x01\r\n");
    p.both(&["add", "-A"]);
    p.both(&["commit", "-q", "-m", "base"]);
    p.put(".gitattributes", rules.as_bytes());
    p
}

const RULES: &[&str] = &[
    "*.txt text=auto\n",
    "*.txt text\n*.bin -text\n",
    "*.txt text eol=crlf\n",
    "* text=auto eol=lf\n",
];

#[test]
fn add_renormalize_restages_tracked_files_like_git() {
    for format in FORMATS {
        for rules in RULES {
            let p = committed_crlf(format, "", rules);
            p.both(&["add", "-u"]);
            p.same_index("add -u leaves unchanged stat alone");
            p.both(&["add", "--renormalize", "--verbose", "."]);
            p.same_all(rules);
            let staged = run(&p.ours, &["diff", "--cached", "--name-only"]);
            assert!(
                staged.contains("a.txt"),
                "{rules}: renormalizing stages a.txt"
            );
            p.both(&["commit", "-q", "-m", "renormalized"]);
            p.same_head(rules);
            p.same_status("after commit");
        }
    }
}

#[test]
fn add_renormalize_takes_pathspecs_and_skips_untracked_files() {
    for format in FORMATS {
        let p = committed_crlf(format, "[core]\n\tautocrlf = true\n", "*.txt text\n");
        p.put("new.txt", b"n\r\n");
        p.both(&["add", "--renormalize", "dir"]);
        p.same_index("pathspec");
        p.both(&["add", "--renormalize", "-A"]);
        p.same_index("-A");
        p.both(&["add", "--renormalize", "-n", "-v", "."]);
        p.same_index("dry run");
        p.same_status("untracked stays untracked");
    }
}

#[test]
fn add_renormalize_runs_ident_too() {
    for format in FORMATS {
        let p = committed_crlf(format, "", "*.txt text\n*.id ident\n");
        p.put("t.id", b"$Id: old $\r\n");
        p.both(&["add", "t.id"]);
        p.both(&["commit", "-q", "-m", "id"]);
        p.put("t.id", b"$Id: edited $\r\n");
        p.both(&["add", "--renormalize", "."]);
        p.same_index("ident");
    }
}

/// `side` renormalizes every file to LF while `main` edits CRLF files in place, which `text=auto` keeps as CRLF.
fn diverged(format: &str, config: &str) -> Pair {
    let p = committed_crlf(format, config, "*.txt text=auto\n");
    p.each(&["add", ".gitattributes"]);
    p.each(&["commit", "-q", "-m", "rules"]);
    p.each(&["checkout", "-q", "-b", "side"]);
    p.both(&["add", "--renormalize", "."]);
    p.put("a.txt", b"1\n2\n3\n4\n5\nside\n");
    p.put("b.txt", b"x\ny\nside\n");
    p.each(&["add", "-A"]);
    p.each(&["commit", "-q", "-m", "side"]);
    p.each(&["checkout", "-q", "main"]);
    p.put("a.txt", b"main\r\n2\r\n3\r\n4\r\n5\r\n6\r\n");
    p.put("b.txt", b"main\r\ny\r\n");
    p.put("dir/c.txt", b"m\r\nmain\r\n");
    p.each(&["add", "-A"]);
    p.each(&["commit", "-q", "-m", "main"]);
    p
}

#[test]
fn merge_renormalize_merges_the_normalized_sides() {
    for format in FORMATS {
        for config in ["", "[core]\n\tautocrlf = true\n"] {
            let p = diverged(format, config);
            let (code, ..) = p.both(&["merge", "--no-edit", "-X", "renormalize", "side"]);
            assert_eq!(code, 0, "clean once normalized");
            p.same_head("merge");
            p.same_all("merge");
        }
    }
}

#[test]
fn merge_without_renormalize_still_conflicts_like_git() {
    for format in FORMATS {
        let p = diverged(format, "");
        let (code, ..) = p.both(&["merge", "--no-edit", "side"]);
        assert_eq!(code, 1, "line endings alone conflict");
        p.same_index("conflicted index");
        p.same_tree("conflicted tree");
    }
}

#[test]
fn renormalize_reaches_cherry_pick_revert_and_rebase() {
    for format in FORMATS {
        let p = diverged(format, "");
        p.both(&["cherry-pick", "-X", "renormalize", "side"]);
        p.same_head("cherry-pick");
        p.same_all("cherry-pick");

        let p = diverged(format, "");
        p.each(&["checkout", "-q", "-b", "work", "main"]);
        p.both(&["rebase", "-X", "renormalize", "side"]);
        p.same_head("rebase");
        p.same_all("rebase");

        let p = diverged(format, "");
        p.both(&["revert", "--no-edit", "-X", "renormalize", "side"]);
        p.same_all("revert");
    }
}

#[test]
fn renormalize_runs_the_clean_filter_on_every_side() {
    for format in FORMATS {
        let config = "[filter \"up\"]\n\tclean = tr a-z A-Z\n\tsmudge = cat\n";
        let p = Pair::new(format, config);
        p.put("a.txt", b"hello\nworld\nend\n");
        p.both(&["add", "-A"]);
        p.both(&["commit", "-q", "-m", "plain"]);
        p.each(&["checkout", "-q", "-b", "side"]);
        p.put("a.txt", b"hello\nworld\nside\n");
        p.each(&["commit", "-q", "-a", "-m", "side"]);
        p.each(&["checkout", "-q", "main"]);
        p.put(".gitattributes", b"*.txt filter=up\n");
        p.put("a.txt", b"main\nworld\nend\n");
        p.each(&["add", "-A"]);
        p.each(&["commit", "-q", "-m", "main"]);
        let (code, ..) = p.both(&["merge", "--no-edit", "-X", "renormalize", "side"]);
        assert_eq!(code, 0);
        p.same_head("filter merge");
        p.same_all("filter merge");
    }
}

fn patch_for(path: &str) -> String {
    format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,3 +1,3 @@\n 1\n-2\n+two\n 3\n"
    )
}

#[test]
fn apply_converts_the_work_tree_file_and_the_result() {
    let cases: &[(&str, &str, &[u8])] = &[
        ("", "*.txt text eol=crlf\n", b"1\n2\n3\n"),
        ("[core]\n\tautocrlf = true\n", "", b"1\n2\n3\n"),
        ("", "*.txt text=auto eol=crlf\n", b"1\n2\n3\n"),
        ("", "*.txt ident\n", b"1\n2\n3\n"),
    ];
    for format in FORMATS {
        for (config, rules, base) in cases {
            for flags in [&[][..], &["--index"], &["--cached"], &["--check"]] {
                let p = Pair::new(format, config);
                p.put(".gitattributes", rules.as_bytes());
                p.put("a.txt", base);
                p.both(&["add", "-A"]);
                p.both(&["commit", "-q", "-m", "one"]);
                p.each(&["checkout", "-q", "--", "."]);
                let patch = patch_for("a.txt");
                p.put("p.patch", patch.as_bytes());
                let mut args = vec!["apply"];
                args.extend_from_slice(flags);
                args.push("p.patch");
                p.both(&args);
                p.same_all(&format!("apply {flags:?} {rules}"));
            }
        }
    }
}

#[test]
fn apply_creates_and_deletes_converted_files() {
    for format in FORMATS {
        let p = Pair::new(format, "[core]\n\tautocrlf = true\n");
        p.put("old.txt", b"gone\n");
        p.both(&["add", "-A"]);
        p.both(&["commit", "-q", "-m", "one"]);
        let patch = "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n\
                     diff --git a/old.txt b/old.txt\ndeleted file mode 100644\n--- a/old.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-gone\n";
        p.put("p.patch", patch.as_bytes());
        p.both(&["apply", "--index", "p.patch"]);
        p.same_all("create and delete");
    }
}

fn pick_session(format: &str, config: &str, rules: &str, verb: &str, keys: &[u8]) {
    let p = Pair::new(format, config);
    p.put(".gitattributes", rules.as_bytes());
    p.put("a.txt", b"1\n2\n3\n4\n5\n6\n7\n8\n9\n");
    p.put("b.txt", b"x\ny\n");
    p.both(&["add", "-A"]);
    p.both(&["commit", "-q", "-m", "one"]);
    p.each(&["checkout", "-q", "--", "."]);
    p.put("a.txt", b"one\n2\n3\n4\n5\n6\n7\n8\nnine\n");
    p.put("b.txt", b"x\nwhy\n");
    p.both(&["add", "b.txt"]);
    p.both_in(&[verb, "-p"], keys);
    p.same_all(&format!("{verb} -p {rules}"));
    p.both_in(&[verb, "-p", "HEAD"], keys);
    p.same_all(&format!("{verb} -p HEAD {rules}"));
}

#[test]
fn restore_and_checkout_patch_modes_write_converted_files() {
    for format in FORMATS {
        for (config, rules) in [
            ("", "*.txt text eol=crlf\n"),
            ("[core]\n\tautocrlf = true\n", ""),
            ("", "*.txt ident\n"),
            ("", "*.txt text=auto eol=crlf\n"),
        ] {
            for verb in ["restore", "checkout"] {
                pick_session(format, config, rules, verb, b"y\nn\ny\nn\n");
                pick_session(format, config, rules, verb, b"a\n");
            }
        }
    }
}

/// `side` only renormalizes `a.txt` while `main` deletes it (or the other way round with `delete_on_side`).
fn modify_delete(format: &str, delete_on_side: bool) -> Pair {
    let p = committed_crlf(format, "", "*.txt text=auto\n");
    p.each(&["add", ".gitattributes"]);
    p.each(&["commit", "-q", "-m", "rules"]);
    let (renormalizer, deleter) = if delete_on_side {
        ("main", "side")
    } else {
        ("side", "main")
    };
    p.each(&["checkout", "-q", "-b", "side"]);
    p.each(&["checkout", "-q", renormalizer]);
    p.both(&["add", "--renormalize", "a.txt"]);
    p.each(&["commit", "-q", "-m", "normalize"]);
    p.each(&["checkout", "-q", deleter]);
    p.each(&["rm", "-q", "a.txt"]);
    p.each(&["commit", "-q", "-m", "delete"]);
    p.each(&["checkout", "-q", "main"]);
    p
}

#[test]
fn renormalize_lets_a_deletion_win_over_a_conversion_only_change() {
    for format in FORMATS {
        for delete_on_side in [false, true] {
            let p = modify_delete(format, delete_on_side);
            let (code, ..) = p.both(&["merge", "--no-edit", "-X", "renormalize", "side"]);
            assert_eq!(code, 0, "the conversion alone is not a modification");
            p.same_head("modify/delete");
            p.same_all("modify/delete");

            let p = modify_delete(format, delete_on_side);
            let (code, ..) = p.both(&["merge", "--no-edit", "side"]);
            assert_eq!(code, 1, "without -X renormalize it is a modify/delete");
            p.same_index("modify/delete conflict");
            p.same_tree("modify/delete conflict");
        }
    }
}

#[test]
fn renormalize_keeps_a_real_edit_against_a_deletion_conflicted() {
    for format in FORMATS {
        let p = modify_delete(format, false);
        p.each(&["checkout", "-q", "side"]);
        p.put("a.txt", b"1\n2\n3\n4\n5\nedit\n");
        p.each(&["commit", "-q", "-a", "-m", "edit"]);
        p.each(&["checkout", "-q", "main"]);
        let (code, ..) = p.both(&["merge", "--no-edit", "-X", "renormalize", "side"]);
        assert_eq!(code, 1);
        p.same_index("edit/delete");
        p.same_tree("edit/delete");
    }
}

#[test]
fn pull_takes_renormalize_from_a_local_remote() {
    for format in FORMATS {
        let p = diverged(format, "");
        for repo in [&p.oracle, &p.ours] {
            let other = repo.parent().unwrap().join(format!(
                "remote-{}",
                repo.file_name().unwrap().to_string_lossy()
            ));
            run(
                repo.parent().unwrap(),
                &[
                    "clone",
                    "-q",
                    "--bare",
                    "--branch",
                    "side",
                    repo.to_str().unwrap(),
                    other.to_str().unwrap(),
                ],
            );
            run(repo, &["remote", "add", "up", other.to_str().unwrap()]);
        }
        p.both(&["fetch", "-q", "up"]);
        let (code, ..) = p.both(&[
            "pull",
            "--no-rebase",
            "--no-edit",
            "-X",
            "renormalize",
            "up",
            "side",
        ]);
        assert_eq!(code, 0, "clean once normalized");
        p.same_head("pull");
        p.same_all("pull");

        let p = diverged(format, "");
        for repo in [&p.oracle, &p.ours] {
            run(repo, &["remote", "add", "up", repo.to_str().unwrap()]);
        }
        let (code, ..) = p.both(&["pull", "--no-rebase", "--no-edit", "up", "side"]);
        assert_eq!(code, 1, "line endings alone conflict");
        p.same_index("pull conflict");
        p.same_tree("pull conflict");
    }
}

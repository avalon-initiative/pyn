//! `pyn add -p`, `restore -p` and `stash push -p` on a git repository: the same answers given to real git and to
//! pyn on copies of one repository must end in the same index, work tree and stash. Needs `git` on the PATH (tests
//! only); the wording of git's prompts differs between versions, so only the outcome and the pinned prompts of pyn's
//! own transcript tests are checked.

mod common;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::gitfix::{copy_tree, tree_dump};

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
    editors: PathBuf,
}

fn numbered(edits: &[(usize, &str)]) -> String {
    let mut lines: Vec<String> = (1..=40).map(|i| i.to_string()).collect();
    for (n, text) in edits {
        lines[n - 1] = text.to_string();
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn feed(mut cmd: Command, input: &str) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let bytes = input.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&bytes);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    out
}

impl Pair {
    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TZ", "UTC")
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .envs(IDENTITY);
        cmd
    }

    fn git_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut cmd = self.command("git", cwd);
        cmd.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args);
        feed(cmd, "")
    }

    fn write(&self, name: &str, text: &str) {
        for root in [&self.git, &self.pyn] {
            if root.join(".git").exists() || root == &self.git {
                let file = root.join(name);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                let _ = fs::remove_file(&file);
                fs::write(file, text).unwrap();
            }
        }
    }

    fn state(&self, dir: &Path) -> String {
        let text =
            |args: &[&str]| String::from_utf8_lossy(&self.git_in(dir, args).stdout).into_owned();
        format!(
            "{}\n{}{}{}{}",
            tree_dump(dir),
            text(&["ls-files", "-s"]),
            text(&["stash", "list"]),
            text(&["rev-parse", "-q", "--verify", "refs/stash"]),
            text(&["status", "--porcelain"]),
        )
    }

    /// A script that edits a hunk the way `sed -e <script>` would.
    fn editor(&self, name: &str, script: &str) -> String {
        let path = self.editors.join(name);
        fs::write(&path, format!("#!/bin/sh\nsed -i -e '{script}' \"$1\"\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }
}

/// Two copies of a repository with `f` (three separate hunks once edited), `g`, `h` and `d/n` committed.
fn pair(setup: impl Fn(&Pair)) -> Pair {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let p = Pair {
        home: root.join("home"),
        git: root.join("git"),
        pyn: root.join("pyn"),
        editors: root.join("editors"),
        _tmp: tmp,
    };
    for dir in [&p.home, &p.git, &p.editors] {
        fs::create_dir_all(dir).unwrap();
    }
    let ok = |args: &[&str]| {
        let out = p.git_in(&p.git, args);
        assert!(out.status.success(), "git {args:?}");
    };
    ok(&["init", "-q", "-b", "main"]);
    ok(&["config", "gc.auto", "0"]);
    ok(&["config", "maintenance.auto", "false"]);
    p.write("f", &numbered(&[]));
    p.write("g", "x\n");
    p.write("h", "h\n");
    p.write("d/n", "n1\nn2\n");
    ok(&["add", "-A"]);
    ok(&["commit", "-q", "-m", "base"]);
    copy_tree(&p.git, &p.pyn);
    setup(&p);
    p
}

fn edit_f(p: &Pair) {
    p.write("f", &numbered(&[(3, "three"), (20, "twenty"), (38, "t38")]));
}

fn edit_all(p: &Pair) {
    edit_f(p);
    p.write("g", "x\nmore\n");
    let mut perms = fs::metadata(p.git.join("g")).unwrap().permissions();
    perms.set_mode(0o755);
    for root in [&p.git, &p.pyn] {
        fs::set_permissions(root.join("g"), perms.clone()).unwrap();
    }
    p.write("d/n", "n1\nN2\nn3\n");
}

fn split_me(p: &Pair) {
    let mut lines: Vec<String> = (1..=40).map(|i| i.to_string()).collect();
    lines[4] = "five".into();
    lines[6] = "seven".into();
    lines[10] = "eleven".into();
    lines.insert(9, "nine-half".into());
    p.write(
        "f",
        &lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
    );
}

fn staged_all(p: &Pair) {
    edit_all(p);
    for root in [&p.git, &p.pyn] {
        assert!(p.git_in(root, &["add", "-A"]).status.success());
    }
}

fn run(p: &Pair, args: &[&str], input: &str, editor: Option<&str>) -> (Output, Output) {
    let editor = editor.unwrap_or(":");
    let mut git = p.command("git", &p.git);
    git.args(["-c", "gc.auto=0"])
        .args(args)
        .env("GIT_EDITOR", editor);
    let mut pyn = p.command(env!("CARGO_BIN_EXE_pyn"), &p.pyn);
    pyn.args(args).env("GIT_EDITOR", editor);
    (feed(git, input), feed(pyn, input))
}

/// Gives the same answers to both and compares the exit status and what is left behind.
fn same(setup: impl Fn(&Pair), args: &[&str], input: &str) {
    same_with(setup, args, input, None);
}

fn same_with(setup: impl Fn(&Pair), args: &[&str], input: &str, editor: Option<(&str, &str)>) {
    let p = pair(setup);
    let editor = editor.map(|(name, script)| p.editor(name, script));
    let (g, y) = run(&p, args, input, editor.as_deref());
    let what = format!("{args:?} with {input:?}");
    assert_eq!(
        y.status.code(),
        g.status.code(),
        "{what}: exit; pyn: {}; git: {}",
        String::from_utf8_lossy(&y.stderr),
        String::from_utf8_lossy(&g.stderr)
    );
    assert_eq!(
        p.state(&p.pyn),
        p.state(&p.git),
        "{what}: state (pyn, then git)"
    );
    let note = |out: &Output| {
        let both = [out.stdout.as_slice(), out.stderr.as_slice()].concat();
        String::from_utf8_lossy(&both)
            .lines()
            .filter(|l| l.starts_with("No changes") || l.starts_with("Only binary"))
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(note(&y), note(&g), "{what}: notes");
}

const WALK: &[&str] = &[
    "y\nn\ny\ny\ny\ny\ny\n",
    "n\nn\nn\nn\nn\nn\nn\n",
    "q\n",
    "y\nq\n",
    "a\n",
    "d\na\n",
    "j\ny\ny\nq\n",
    "J\nJ\ny\nd\nq\n",
    "g\n3\ny\nq\n",
    "/\ntwen\ny\nq\n",
    "/\nzzz\ny\nq\n",
    "\nY\nN\nq\n",
    "?\nq\n",
    "y\ny\ny\ny\ny\n",
];

#[test]
fn add_follows_every_answer_like_git() {
    for input in WALK {
        same(edit_all, &["add", "-p"], input);
    }
}

#[test]
fn add_limited_to_paths_matches_git() {
    for args in [
        &["add", "-p", "f"][..],
        &["add", "-p", "g", "d"],
        &["add", "-p", "d"],
        &["add", "-p", "nothing"],
        &["add", "--patch", "."],
    ] {
        same(edit_all, args, "y\nn\ny\ny\ny\n");
    }
}

#[test]
fn add_ignores_the_flags_git_ignores_with_patch() {
    for flag in ["-u", "-A", "-f", "-v"] {
        same(edit_all, &["add", "-p", flag], "y\nn\ny\ny\ny\n");
    }
}

#[test]
fn add_with_nothing_to_stage_matches_git() {
    same(|_| {}, &["add", "-p"], "y\n");
    same(
        |p| {
            p.write("b", "a\0b");
            for root in [&p.git, &p.pyn] {
                assert!(p.git_in(root, &["add", "b"]).status.success());
                assert!(
                    p.git_in(root, &["commit", "-q", "-m", "b"])
                        .status
                        .success()
                );
            }
            p.write("b", "a\0c");
        },
        &["add", "-p"],
        "y\n",
    );
}

#[test]
fn add_split_works_like_git() {
    for input in [
        "s\ny\nn\ny\nn\n",
        "s\nn\ny\nn\ny\n",
        "s\ns\nn\ny\nq\n",
        "s\nj\nj\ny\nn\n",
        "s\na\n",
        "s\nd\n",
        "s\ny\ny\ny\ny\n",
    ] {
        same(split_me, &["add", "-p"], input);
    }
}

#[test]
fn add_edit_works_like_git() {
    let cases = [
        ("keep", "s/^+five$/+FIVE/;s/^-7$/ 7/", "e\ny\n"),
        ("none", "/^[^#]/d", "e\ny\n"),
        ("bad", "s/^ 3$/ 33/", "e\nn\ne\ny\nn\n"),
        ("drop", "/^+seven$/d", "e\nq\n"),
        ("twice", "s/^+five$/+FIVE/", "e\ne\nq\n"),
    ];
    for (name, script, input) in cases {
        same_with(split_me, &["add", "-p"], input, Some((name, script)));
        same_with(edit_all, &["add", "-p"], input, Some((name, script)));
    }
}

#[test]
fn deleted_new_and_mode_changed_files_match_git() {
    same(
        |p| {
            edit_f(p);
            for root in [&p.git, &p.pyn] {
                fs::remove_file(root.join("h")).unwrap();
            }
        },
        &["add", "-p"],
        "y\ny\ny\ny\ny\ny\n",
    );
    same(
        |p| {
            p.write("new", "one\ntwo\n");
            for root in [&p.git, &p.pyn] {
                assert!(p.git_in(root, &["add", "-N", "new"]).status.success());
            }
        },
        &["add", "-p"],
        "y\ny\n",
    );
    same(
        |p| {
            for root in [&p.git, &p.pyn] {
                let mut perms = fs::metadata(root.join("h")).unwrap().permissions();
                perms.set_mode(0o755);
                fs::set_permissions(root.join("h"), perms).unwrap();
            }
        },
        &["add", "-p"],
        "y\n",
    );
}

#[test]
fn files_without_final_newline_and_with_crlf_match_git() {
    same(
        |p| {
            p.write("f", "a\nb");
            for root in [&p.git, &p.pyn] {
                assert!(p.git_in(root, &["add", "f"]).status.success());
                assert!(
                    p.git_in(root, &["commit", "-q", "-m", "n"])
                        .status
                        .success()
                );
            }
            p.write("f", "a\nc");
        },
        &["add", "-p"],
        "y\ny\n",
    );
    same(
        |p| p.write("f", &numbered(&[(3, "three\r")])),
        &["add", "-p"],
        "y\ny\n",
    );
}

#[test]
fn restore_worktree_matches_git() {
    for input in WALK {
        same(edit_all, &["restore", "-p"], input);
    }
    same(edit_all, &["restore", "-p", "f", "d"], "y\nn\ny\ny\n");
    same(
        |p| {
            edit_all(p);
            for root in [&p.git, &p.pyn] {
                fs::remove_file(root.join("h")).unwrap();
            }
        },
        &["restore", "-p"],
        "y\ny\ny\ny\ny\ny\ny\n",
    );
}

#[test]
fn restore_staged_matches_git() {
    for input in WALK {
        same(staged_all, &["restore", "--staged", "-p"], input);
        same(staged_all, &["restore", "-S", "-p"], input);
    }
    same(
        |p| {
            staged_all(p);
            p.write("f", &numbered(&[(3, "three"), (4, "four")]));
        },
        &["restore", "-p", "--staged"],
        "y\ny\ny\ny\ny\ny\n",
    );
}

#[test]
fn restore_from_a_revision_matches_git() {
    let later = |p: &Pair| {
        edit_all(p);
        for root in [&p.git, &p.pyn] {
            assert!(p.git_in(root, &["add", "-A"]).status.success());
            assert!(
                p.git_in(root, &["commit", "-q", "-m", "two"])
                    .status
                    .success()
            );
        }
        p.write(
            "f",
            &numbered(&[(3, "three"), (20, "twenty"), (38, "t38"), (40, "end")]),
        );
    };
    for args in [
        &["restore", "-p", "--source=HEAD~1"][..],
        &["restore", "-p", "--source", "HEAD"],
        &["restore", "-p", "-s", "HEAD~0"],
        &["restore", "-p", "--staged", "--source=HEAD~1"],
        &["restore", "-p", "--staged", "--worktree", "--source=HEAD~1"],
        &["restore", "-p", "--staged", "--worktree"],
        &["restore", "-p", "-SW", "--source=HEAD"],
    ] {
        same(later, args, "y\nn\ny\ny\ny\ny\ny\n");
        same(later, args, "y\ny\ny\ny\ny\ny\ny\n");
    }
}

#[test]
fn restore_to_index_and_worktree_asks_when_the_index_differs() {
    let differs = |p: &Pair| {
        staged_all(p);
        p.write(
            "f",
            &numbered(&[(3, "three"), (20, "twenty"), (38, "t38"), (40, "end")]),
        );
    };
    for input in [
        "y\ny\ny\ny\ny\ny\n",
        "y\ny\ny\nn\n",
        "y\ny\ny\nz\ny\n",
        "y\ny\ny\n",
    ] {
        same(differs, &["restore", "-p", "-SW", "--source=HEAD~0"], input);
        same(differs, &["restore", "-p", "-SW"], input);
    }
}

#[test]
fn stash_follows_the_answers_like_git() {
    for input in WALK {
        same(edit_all, &["stash", "push", "-p"], input);
    }
    for args in [
        &["stash", "push", "-p", "--", "f"][..],
        &["stash", "-p"],
        &["stash", "push", "--patch", "-m", "named"],
        &["stash", "push", "-p", "--no-keep-index"],
        &["stash", "push", "-p", "-k"],
        &["stash", "push", "-p", "-u"],
    ] {
        same(staged_all, args, "y\nn\ny\ny\ny\ny\ny\n");
        same(edit_all, args, "y\nn\ny\ny\ny\ny\ny\n");
    }
}

#[test]
fn stash_with_nothing_to_save_or_choose_matches_git() {
    same(|_| {}, &["stash", "push", "-p"], "y\n");
    same(edit_all, &["stash", "push", "-p"], "n\nn\nn\nn\nn\nn\nn\n");
    same(edit_all, &["stash", "push", "-p"], "");
}

#[test]
fn stash_only_looks_at_files_of_head() {
    same(
        |p| {
            edit_f(p);
            p.write("new", "one\n");
            for root in [&p.git, &p.pyn] {
                assert!(p.git_in(root, &["add", "new"]).status.success());
            }
            p.write("new", "one\ntwo\n");
        },
        &["stash", "push", "-p"],
        "y\ny\ny\ny\ny\n",
    );
    same(
        |p| {
            edit_f(p);
            for root in [&p.git, &p.pyn] {
                assert!(p.git_in(root, &["rm", "-q", "h"]).status.success());
            }
        },
        &["stash", "push", "-p"],
        "y\ny\ny\ny\ny\n",
    );
}

#[test]
fn stash_edit_that_cannot_be_taken_out_still_saves_like_git() {
    same_with(
        split_me,
        &["stash", "push", "-p"],
        "e\ny\n",
        Some(("keep", "s/^+five$/+FIVE/;s/^-7$/ 7/")),
    );
}

#[test]
fn a_stash_made_by_patch_applies_back() {
    let p = pair(edit_all);
    let before = tree_dump(&p.pyn);
    let (_, y) = run(&p, &["stash", "push", "-p"], "a\na\na\na\n", None);
    assert!(y.status.success());
    assert_ne!(tree_dump(&p.pyn), before);
    let out = feed(
        {
            let mut c = p.command(env!("CARGO_BIN_EXE_pyn"), &p.pyn);
            c.args(["stash", "pop"]);
            c
        },
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(tree_dump(&p.pyn), before);
}

#[test]
fn patch_needs_a_git_repository_and_excludes_other_modes() {
    let p = pair(edit_all);
    for args in [&["add", "-p", "-n"][..], &["restore", "-p", "--ours"]] {
        let out = feed(
            {
                let mut c = p.command(env!("CARGO_BIN_EXE_pyn"), &p.pyn);
                c.args(args);
                c
            },
            "",
        );
        assert!(!out.status.success(), "{args:?}");
    }
}

fn transcript(p: &Pair, args: &[&str], input: &str) -> String {
    let (_, y) = run(p, args, input, None);
    String::from_utf8_lossy(&y.stdout).into_owned()
}

#[test]
fn the_prompt_shows_git_s_header_hunks_keys_and_help() {
    let p = pair(edit_all);
    let got = transcript(&p, &["add", "-p", "f"], "j\n?\nn\nq\n");
    let hunk1 = "@@ -1,6 +1,6 @@\n 1\n 2\n-3\n+three\n 4\n 5\n 6\n";
    let hunk2 = "@@ -17,7 +17,7 @@\n 17\n 18\n 19\n-20\n+twenty\n 21\n 22\n 23\n";
    let hunk3 = "@@ -35,6 +35,6 @@\n 35\n 36\n 37\n-38\n+t38\n 39\n 40\n";
    let help = "y - stage this hunk\n\
        n - do not stage this hunk\n\
        q - quit; do not stage this hunk or any of the remaining ones\n\
        a - stage this hunk and all later hunks in the file\n\
        d - do not stage this hunk or any of the later hunks in the file\n\
        j - go to the next undecided hunk, roll over at the bottom\n\
        J - go to the next hunk, roll over at the bottom\n\
        k - go to the previous undecided hunk, roll over at the top\n\
        K - go to the previous hunk, roll over at the top\n\
        g - select a hunk to go to\n\
        / - search for a hunk matching the given regex\n\
        e - manually edit the current hunk\n\
        p - print the current hunk\n\
        P - print the current hunk using the pager\n\
        ? - print help\n";
    let expected = format!(
        "diff --git a/f b/f\nindex 1c99002..0bc4f08 100644\n--- a/f\n+++ b/f\n{hunk1}\
         (1/3) Stage this hunk [y,n,q,a,d,k,K,j,J,g,/,e,p,P,?]? {hunk2}\
         (2/3) Stage this hunk [y,n,q,a,d,k,K,j,J,g,/,e,p,P,?]? {help}\
         (2/3) Stage this hunk [y,n,q,a,d,k,K,j,J,g,/,e,p,P,?]? {hunk3}\
         (3/3) Stage this hunk [y,n,q,a,d,k,K,j,J,g,/,e,p,P,?]? \n"
    );
    assert_eq!(got, expected);
}

#[test]
fn the_prompt_names_mode_changes_deletions_and_each_mode_s_verb() {
    let p = pair(|p| {
        edit_all(p);
        fs::remove_file(p.pyn.join("h")).unwrap();
    });
    let got = transcript(&p, &["add", "-p", "g", "h"], "n\nn\n");
    assert!(got.contains(
        "old mode 100644\nnew mode 100755\n(1/2) Stage mode change [y,n,q,a,d,k,K,j,J,g,/,p,P,?]? "
    ));
    assert!(got.contains("deleted file mode 100644\nindex 6e9f0da..0000000\n--- a/h\n+++ /dev/null\n@@ -1 +0,0 @@\n-h\n(1/1) Stage deletion [y,n,q,a,d,p,P,?]? "));
    for (args, prompt) in [
        (
            &["restore", "-p", "f"][..],
            "Discard this hunk from worktree [y,n,q,a,d,",
        ),
        (
            &["restore", "-p", "--source=HEAD", "f"],
            "Discard this hunk from worktree [y,n,q,a,d,",
        ),
        (
            &["restore", "-p", "--source=HEAD~0", "f"],
            "Apply this hunk to worktree [y,n,q,a,d,",
        ),
        (
            &["restore", "-p", "-SW", "f"],
            "Discard this hunk from index and worktree [y,n,q,a,d,",
        ),
        (
            &["restore", "-p", "-SW", "-s", "HEAD~0", "f"],
            "Apply this hunk to index and worktree [y,n,q,a,d,",
        ),
        (
            &["restore", "-p", "-S", "-s", "HEAD~0", "f"],
            "Apply this hunk to index [y,n,q,a,d,",
        ),
        (
            &["stash", "push", "-p", "--", "f"],
            "Stash this hunk [y,n,q,a,d,",
        ),
    ] {
        let p = pair(|p| {
            staged_all(p);
            p.write("f", &numbered(&[(3, "three"), (5, "five"), (20, "twenty")]));
        });
        assert!(transcript(&p, args, "q\n").contains(prompt), "{args:?}");
    }
    let p = pair(staged_all);
    let got = transcript(&p, &["restore", "--staged", "-p", "f"], "q\n");
    assert!(got.contains("(1/3) Unstage this hunk [y,n,q,a,d,k,K,j,J,g,/,e,p,P,?]? "));
}

#[test]
fn splitting_reports_the_count_and_shares_context() {
    let p = pair(split_me);
    let got = transcript(&p, &["add", "-p"], "s\nq\n");
    assert!(got.contains(
        "[y,n,q,a,d,s,e,p,P,?]? Split into 4 hunks.\n@@ -2,5 +2,5 @@\n 2\n 3\n 4\n-5\n+five\n 6\n(1/4) "
    ));
}

#[test]
fn g_lists_hunks_with_their_state_and_first_changed_line() {
    let p = pair(edit_f);
    let got = transcript(&p, &["add", "-p"], "y\ng\n2\nq\n");
    assert!(got.contains("+ 1:  -1,6 +1,6          -3\n  2:  -17,7 +17,7        -20\n  3:  -35,6 +35,6        -38\ngo to which hunk? "));
}

//! `pyn add -i` on a git repository against the real git: the same scripted input is given to both on copies of one
//! repository, and the transcript on standard output and error, the exit status, the index and the work tree must
//! agree. Needs `git` on the PATH (tests only). The menu is the same in git 2.43 and 2.55; the hunk prompt of the
//! `patch` command is only compared from 2.55 on.

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
            .env_remove("GIT_EDITOR")
            .envs(IDENTITY);
        cmd
    }

    fn git_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut cmd = self.command("git", cwd);
        cmd.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args);
        feed(cmd, "")
    }

    fn both(&self, args: &[&str]) {
        for root in [&self.git, &self.pyn] {
            let out = self.git_in(root, args);
            assert!(out.status.success(), "git {args:?}");
        }
    }

    fn write(&self, name: &str, text: &str) {
        for root in [&self.git, &self.pyn] {
            let file = root.join(name);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            let _ = fs::remove_file(&file);
            fs::write(file, text).unwrap();
        }
    }

    fn remove(&self, name: &str) {
        for root in [&self.git, &self.pyn] {
            fs::remove_file(root.join(name)).unwrap();
        }
    }

    fn make_executable(&self, name: &str) {
        for root in [&self.git, &self.pyn] {
            let mut perms = fs::metadata(root.join(name)).unwrap().permissions();
            perms.set_mode(0o755);
            fs::set_permissions(root.join(name), perms).unwrap();
        }
    }

    fn state(&self, dir: &Path) -> String {
        let text =
            |args: &[&str]| String::from_utf8_lossy(&self.git_in(dir, args).stdout).into_owned();
        format!(
            "{}\n{}{}",
            tree_dump(dir),
            text(&["ls-files", "-s"]),
            text(&["status", "--porcelain"]),
        )
    }
}

fn numbered(edits: &[(usize, &str)]) -> String {
    let mut lines: Vec<String> = (1..=40).map(|i| i.to_string()).collect();
    for (n, text) in edits {
        lines[n - 1] = text.to_string();
    }
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// Two copies of a repository holding `f`, `g`, `h`, `far`, `foo`, `bin` and `sub/{s,t}`.
fn pair(format: &str, setup: impl Fn(&Pair)) -> Pair {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let p = Pair {
        home: root.join("home"),
        git: root.join("git"),
        pyn: root.join("pyn"),
        _tmp: tmp,
    };
    fs::create_dir_all(&p.home).unwrap();
    fs::create_dir_all(&p.git).unwrap();
    let out = p.git_in(
        &p.git,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
    );
    assert!(out.status.success());
    for (name, text) in [
        ("f", numbered(&[])),
        ("g", "x\n".into()),
        ("h", "h\n".into()),
        ("far", "far\n".into()),
        ("foo", "foo\n".into()),
        ("bin", "bin\0".into()),
        ("sub/s", "s1\n".into()),
        ("sub/t", "s2\n".into()),
    ] {
        let file = p.git.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
    }
    let ok = |args: &[&str]| assert!(p.git_in(&p.git, args).status.success(), "git {args:?}");
    ok(&["add", "-A"]);
    ok(&["commit", "-q", "-m", "base"]);
    copy_tree(&p.git, &p.pyn);
    setup(&p);
    p
}

fn changed(p: &Pair) {
    p.write("f", &numbered(&[(3, "three"), (20, "twenty"), (38, "t38")]));
    p.write("g", "x\nmore\n");
    p.remove("h");
    p.write("far", "far\nfar2\n");
    p.write("foo", "foo\nfoo2\n");
    p.write("bin", "bin2\0");
    p.write("sub/s", "s1\ns1b\n");
    p.make_executable("sub/s");
    p.write("u1", "new\n");
    p.write("sub/u2", "new2\n");
    p.both(&["add", "g", "sub/t"]);
    p.write("g", "x\nmore\nmore2\n");
    p.write("sub/t", "s2\nt2\n");
}

fn git_prints_the_current_prompt() -> bool {
    let out = Command::new("git").arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let version: Vec<u32> = text
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .split('.')
        .filter_map(|n| n.parse().ok())
        .collect();
    version >= vec![2, 55]
}

/// Runs `add -i <args>` in both with `input` and compares what they print, their exit status and what they leave.
fn same(format: &str, setup: impl Fn(&Pair), sub: &str, args: &[&str], input: &str) {
    let p = pair(format, setup);
    let mut git = p.command("git", &p.git.join(sub));
    git.args(["-c", "gc.auto=0", "add", "-i"]).args(args);
    let mut pyn = p.command(env!("CARGO_BIN_EXE_pyn"), &p.pyn.join(sub));
    pyn.args(["add", "-i"]).args(args);
    let (g, y) = (feed(git, input), feed(pyn, input));
    let what = format!("{args:?} in {sub:?} with {input:?}");
    let text = |out: &[u8]| String::from_utf8_lossy(out).into_owned();
    for (stream, pyn_text, git_text) in [
        ("standard output", text(&y.stdout), text(&g.stdout)),
        ("standard error", text(&y.stderr), text(&g.stderr)),
    ] {
        if let Some((n, (a, b))) = pyn_text
            .lines()
            .zip(git_text.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
        {
            panic!(
                "{what}: {stream} differs at line {}: pyn {a:?}, git {b:?}",
                n + 1
            );
        }
        assert_eq!(pyn_text, git_text, "{what}: {stream}");
    }
    assert_eq!(y.status.code(), g.status.code(), "{what}: exit status");
    assert_eq!(
        p.state(&p.pyn),
        p.state(&p.git),
        "{what}: state (pyn, then git)"
    );
}

const FORMATS: [&str; 2] = ["sha1", "sha256"];

#[test]
fn the_menu_status_and_help_match_git() {
    for format in FORMATS {
        for input in [
            "",
            "q\n",
            "s\nq\n",
            "\n\n7\n",
            "h\n?\nx\nq\n",
            "status\nquit\n",
            "st\nqu\n",
            " s \t\n q \n",
            "8\n9\n0\n1-2\n*\n-\nq\n",
            "s",
        ] {
            same(format, changed, "", &[], input);
        }
    }
}

#[test]
fn selecting_with_numbers_ranges_prefixes_and_signs_matches_git() {
    for format in FORMATS {
        for input in [
            "u\n1 3-4,\n?\n-3\n\nq\n",
            "u\n*\n\nq\n",
            "u\nf\nfo\nfar\n\nq\n",
            "u\nsub\nsub/s\nsub/t\n\nq\n",
            "u\n-\n- \nzz\n5-\n3-1\n0\n9\n1x\n\nq\n",
            "u\n1x\n-2\n\nq\n",
            "u\n1-3 -2 5,7\n\nq\n",
            "u\n99999999999999999999999\n-0\n\nq\n",
            "u\n1\n",
            "u\n1",
            "u\n\nq\n",
            "u\n?\n\nq\n",
        ] {
            same(format, changed, "", &[], input);
        }
    }
}

#[test]
fn update_revert_and_add_untracked_match_git() {
    for format in FORMATS {
        for input in [
            "u\n*\n\nq\n",
            "u\n2 6\n\ns\nq\n",
            "r\n*\n\ns\nq\n",
            "r\n1-3\n\nu\n*\n\nr\n*\n\nq\n",
            "a\n*\n\ns\nq\n",
            "a\n1-2\n\nr\n*\n\nq\n",
            "a\nu\n\nq\n",
            "d\n*\nq\n",
            "d\n2-3 7\nq\n",
            "d\nxx\nq\n",
            "d\n",
            "u\n*\n\na\n\nr\n\nd\n\np\n\nq\n",
        ] {
            same(format, changed, "", &[], input);
        }
    }
}

#[test]
fn files_of_every_kind_are_listed_and_staged_like_git() {
    let kinds = |p: &Pair| {
        p.write("empty", "");
        p.write("a b", "spaced\n");
        p.write("ü", "uml\n");
        p.write("sub/deep/x", "x\n");
        for root in [&p.git, &p.pyn] {
            fs::remove_file(root.join("g")).unwrap();
            std::os::unix::fs::symlink("f", root.join("g")).unwrap();
            fs::write(root.join("far"), "far\n").unwrap();
        }
        p.make_executable("foo");
        p.both(&["add", "-N", "empty", "a b"]);
        p.both(&["rm", "-q", "--cached", "sub/t"]);
    };
    for format in FORMATS {
        for input in [
            "s\nq\n",
            "u\n*\n\nr\n*\n\ns\nq\n",
            "a\n*\n\ns\nq\n",
            "r\n*\n\nd\n*\nq\n",
        ] {
            same(format, kinds, "", &[], input);
        }
    }
}

#[test]
fn paths_narrow_the_lists_and_names_stay_relative_to_the_root() {
    for (sub, args) in [
        ("", &["sub"][..]),
        ("", &["f", "far"]),
        ("", &["nosuch", "sub"]),
        ("sub", &["."]),
        ("sub", &["../f"]),
        ("sub", &[]),
        ("", &["--", "g"]),
    ] {
        same("sha1", changed, sub, args, "s\nu\n*\n\na\n*\n\nr\n*\n\nq\n");
    }
}

#[test]
fn ignored_flags_and_refused_combinations_match_git() {
    for args in [
        &["-N"][..],
        &["-v"],
        &["-f"],
        &["-u"],
        &["-A"],
        &["-n"],
        &["-p", "-n"],
    ] {
        same("sha1", changed, "", args, "q\n");
    }
}

#[test]
fn a_repository_without_commits_and_one_without_changes_match_git() {
    let unborn = |p: &Pair| {
        p.both(&["update-ref", "-d", "refs/heads/main"]);
        p.write("new1", "n\n");
    };
    for format in FORMATS {
        for input in ["s\nu\n1-2\n\nr\n*\n\na\n1\n\nd\n*\nq\n", "r\n*\n\ns\nq\n"] {
            same(format, unborn, "", &[], input);
        }
        same(format, |_| {}, "", &[], "s\nu\nr\na\np\nd\nq\n");
    }
}

#[test]
fn intent_to_add_files_and_binary_files_match_git() {
    let marked = |p: &Pair| {
        p.write("n1", "one\ntwo\n");
        p.write("bin", "a\0c");
        p.both(&["add", "-N", "n1"]);
        p.write("n1", "one\ntwo\nthree\n");
        p.write("f", &numbered(&[(2, "two")]));
    };
    for format in FORMATS {
        for input in ["s\nq\n", "r\n*\n\nq\n", "u\n*\n\nd\n*\nq\n", "p\n\nq\n"] {
            same(format, marked, "", &[], input);
        }
    }
}

#[test]
fn the_patch_command_asks_hunks_like_git() {
    if !git_prints_the_current_prompt() {
        return;
    }
    for format in FORMATS {
        for input in [
            "p\n*\n\ny\nn\nq\nq\n",
            "p\n2 5\n\ny\ny\nq\n",
            "p\n*\n\ny\nn\ns\nq\n",
            "p\n4\n\nq\n",
            "p\n\nq\n",
            "p\n1\n\ny\n",
            "p\n*\n\na\n",
            "u\r\n\r\nq\r\n",
            "u\r\n1\r\n\r\nq\r\n",
        ] {
            same(format, changed, "", &[], input);
        }
        same(format, |_| {}, "", &[], "p\nq\n");
    }
}

/// `g` conflicts (both modified), `f` is deleted on one side and changed on the other.
fn conflicted(p: &Pair) {
    p.both(&["checkout", "-q", "-b", "other"]);
    p.write("g", "other\n");
    p.write("f", &numbered(&[(2, "two")]));
    p.both(&["commit", "-q", "-am", "other"]);
    p.both(&["checkout", "-q", "main"]);
    p.write("g", "main\n");
    p.both(&["rm", "-q", "f"]);
    p.both(&["commit", "-q", "-am", "main"]);
    for root in [&p.git, &p.pyn] {
        let _ = p.git_in(root, &["merge", "other"]);
    }
    p.write("far", "far\nfar2\n");
    p.write("u1", "new\n");
}

#[test]
fn unmerged_paths_are_listed_and_handled_like_git() {
    for format in FORMATS {
        for input in [
            "s\nq\n",
            "u\n*\n\ns\nq\n",
            "u\n1\n\ns\nq\n",
            "r\n*\n\ns\nq\n",
            "r\n1\n\ns\nq\n",
            "d\n*\nq\n",
            "d\n1\n\nq\n",
            "a\n*\n\ns\nq\n",
            "u\n*\n\nr\n*\n\nd\n*\nq\n",
        ] {
            same(format, conflicted, "", &[], input);
        }
        same(format, conflicted, "", &["g"], "s\nu\n*\n\nq\n");
        same(format, conflicted, "", &["far"], "s\nq\n");
    }
}

#[test]
fn the_patch_command_skips_unmerged_paths_like_git() {
    if !git_prints_the_current_prompt() {
        return;
    }
    for format in FORMATS {
        for input in ["p\n*\n\ny\nq\n", "p\n1\n\nn\nq\n", "p\n\nq\n"] {
            same(format, conflicted, "", &[], input);
        }
    }
}

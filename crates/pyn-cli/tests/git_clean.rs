//! `pyn clean` on a git repository, compared with the real `git clean` on a copy of the same work tree: the exit
//! status, the work tree afterwards and the set of `Would remove` / `Removing` lines. Needs `git` on the PATH (tests only).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::gitfix::copy_tree;

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

struct Env {
    home: PathBuf,
}

/// Runs the command with `input` on its standard input.
fn run_with(mut cmd: Command, input: &str) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(input.as_bytes());
    drop(stdin);
    child.wait_with_output().unwrap()
}

impl Env {
    fn git_cmd(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY);
        cmd
    }

    fn pyn_cmd(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pyn"));
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("TZ", "UTC")
            .env_remove("COLUMNS")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO");
        cmd
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.git_cmd(cwd, args).output().expect("git is installed")
    }

    fn ok(&self, cwd: &Path, args: &[&str]) {
        let out = self.git(cwd, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Output {
        self.pyn_cmd(cwd, args).output().unwrap()
    }
}

fn write(dir: &Path, name: &str, text: &str) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&file);
    fs::write(file, text).unwrap();
}

/// Every path in the work tree outside `.git` with its content, or the link target.
fn tree(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path.strip_prefix(root).unwrap().display().to_string();
        if rel == ".git" || rel.ends_with("/.git") {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() {
            out.insert(
                rel,
                format!("-> {}", fs::read_link(&path).unwrap().display()),
            );
        } else if meta.is_dir() {
            out.insert(rel.clone(), "dir".into());
            tree(root, &path, out);
        } else {
            out.insert(rel, format!("{:?}", fs::read(&path).unwrap()));
        }
    }
}

fn lines(out: &Output) -> Vec<String> {
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    v.sort();
    v
}

struct Pair {
    _tmp: tempfile::TempDir,
    env: Env,
    git: PathBuf,
    pyn: PathBuf,
}

/// A committed repository, then untracked, ignored and nested-repository content of every kind.
fn repo(format: &str, ignore: &str) -> Pair {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let env = Env {
        home: root.join("home"),
    };
    fs::create_dir_all(&env.home).unwrap();
    let git = root.join("git");
    fs::create_dir_all(&git).unwrap();
    env.ok(
        &git,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
    );
    write(&git, ".gitignore", ignore);
    write(&git, "a", "one\n");
    write(&git, "dir/b", "b1\n");
    write(&git, "dir/sub/c", "c1\n");
    write(&git, "other/d", "d1\n");
    env.ok(&git, &["config", "gc.auto", "0"]);
    env.ok(&git, &["config", "maintenance.auto", "false"]);
    env.ok(&git, &["add", "-A"]);
    env.ok(&git, &["commit", "-q", "-m", "first"]);
    write(&git, "u", "untracked\n");
    write(&git, "x.log", "ignored\n");
    write(&git, "dir/u2", "untracked in dir\n");
    write(&git, "dir/v.log", "ignored in dir\n");
    write(&git, "dir/sub/u3", "deep\n");
    write(&git, "newd/f", "folder\n");
    write(&git, "newd/g/h", "folder\n");
    write(&git, "mixed/keep.log", "ignored\n");
    write(&git, "mixed/gone", "untracked\n");
    write(&git, "mixed/sub/gone2", "untracked\n");
    write(&git, "ign/i", "ignored folder\n");
    write(&git, "onlyign/z.log", "ignored\n");
    fs::create_dir_all(git.join("empty/inner")).unwrap();
    write(&git, "holder/plain", "plain\n");
    write(&git, "holder/rest/k", "k\n");
    write(&git, "nested/n", "nested\n");
    env.ok(&git, &["init", "-q", "nested"]);
    write(&git, "holder/repo/n", "nested\n");
    env.ok(&git, &["init", "-q", "holder/repo"]);
    write(&git, "dir/inner/n", "nested in a tracked folder\n");
    env.ok(&git, &["init", "-q", "dir/inner"]);
    let outside = root.join("outside");
    write(&outside, "keep", "must survive\n");
    std::os::unix::fs::symlink(&outside, git.join("outlink")).unwrap();
    std::os::unix::fs::symlink(outside.join("keep"), git.join("newd/filelink")).unwrap();
    std::os::unix::fs::symlink("a", git.join("alink")).unwrap();
    let pyn = root.join("pyn");
    copy_tree(&git, &pyn);
    Pair {
        _tmp: tmp,
        env,
        git,
        pyn,
    }
}

impl Pair {
    fn outside(&self) -> PathBuf {
        self.git.parent().unwrap().join("outside")
    }

    /// Runs `clean <args>` in both from `sub`; the exit status, the work tree and the lines must agree.
    fn same(&self, sub: &str, args: &[&str], label: &str) {
        let mut full = vec!["clean"];
        full.extend(args);
        let g = self.env.git(&self.git.join(sub), &full);
        let p = self.env.pyn(&self.pyn.join(sub), &full);
        let what = format!("{label}: {args:?} in '{sub}'");
        assert_eq!(
            p.status.code(),
            g.status.code(),
            "{what}: exit status; pyn said {}; git said {}",
            String::from_utf8_lossy(&p.stderr),
            String::from_utf8_lossy(&g.stderr)
        );
        let (mut gt, mut pt) = (BTreeMap::new(), BTreeMap::new());
        tree(&self.git, &self.git, &mut gt);
        tree(&self.pyn, &self.pyn, &mut pt);
        assert_eq!(pt, gt, "{what}: work tree (pyn, then git)");
        assert_eq!(lines(&p), lines(&g), "{what}: output (pyn, then git)");
        assert!(
            self.outside().join("keep").is_file(),
            "{what}: left the repository"
        );
    }
}

/// What was done to the work tree, from the output of an interactive run (the prompts and lists are left out).
fn actions(out: &Output) -> Vec<String> {
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| {
            let after = |mark: &str, l: &str| l.rsplit(mark).next().unwrap_or(l).to_string();
            after("[y/N]? ", &after("> ", l))
        })
        .filter(|l| {
            let listing = l.contains("following item");
            (l.starts_with("Removing ") || l.starts_with("Would remove ") && !listing)
                || l.starts_with("Skipping ")
                || l.starts_with("Would skip ")
                || l.contains("current working directory")
        })
        .collect();
    v.sort();
    v
}

impl Pair {
    /// Like [`Pair::same`], with `input` typed at the prompts; the actions taken must agree, not the prompt text.
    fn same_typed(&self, sub: &str, args: &[&str], input: &str, label: &str) {
        let mut full = vec!["clean"];
        full.extend(args);
        let g = run_with(self.env.git_cmd(&self.git.join(sub), &full), input);
        let p = run_with(self.env.pyn_cmd(&self.pyn.join(sub), &full), input);
        let what = format!("{label}: {args:?} in '{sub}' typing {input:?}");
        assert_eq!(
            p.status.code(),
            g.status.code(),
            "{what}: exit status; pyn said {}; git said {}",
            String::from_utf8_lossy(&p.stderr),
            String::from_utf8_lossy(&g.stderr)
        );
        let (mut gt, mut pt) = (BTreeMap::new(), BTreeMap::new());
        tree(&self.git, &self.git, &mut gt);
        tree(&self.pyn, &self.pyn, &mut pt);
        assert_eq!(pt, gt, "{what}: work tree (pyn, then git)");
        assert_eq!(actions(&p), actions(&g), "{what}: actions (pyn, then git)");
        assert!(
            self.outside().join("keep").is_file(),
            "{what}: left the repository"
        );
    }
}

const FORMATS: [&str; 2] = ["sha1", "sha256"];

#[test]
fn dry_runs_match_git() {
    for format in FORMATS {
        for args in [
            &["-n"][..],
            &["-nd"],
            &["-nx"],
            &["-ndx"],
            &["-nX"],
            &["-ndX"],
            &["-ndff"],
            &["-ndxff"],
            &["-n", "-d", "-e", "*.tmp"],
            &["-ndx", "-e", "x.log"],
            &["-ndX", "-e", "u*"],
            &["-nd", "-e", "newd/"],
            &["-nd", "-e", "!x.log", "-x"],
            &["-n", "dir"],
            &["-n", "dir/sub"],
            &["-n", "newd"],
            &["-n", "newd/g"],
            &["-n", "newd/g/h"],
            &["-n", "mixed"],
            &["-nX", "mixed"],
            &["-nx", "mixed"],
            &["-n", "holder"],
            &["-n", "holder", "-ff"],
            &["-n", "nested"],
            &["-n", "-ff", "nested"],
            &["-nd", "dir/inner"],
            &["-n", "."],
            &["-n", "u", "alink"],
            &["-nd", "--", "u"],
            &["-nd", "nothing-here"],
            &["-ndq"],
            &["-n", "--exclude=u"],
        ] {
            repo(format, "*.log\nign/\n").same("", args, format);
        }
    }
}

#[test]
fn removals_match_git() {
    for format in FORMATS {
        for args in [
            &["-f"][..],
            &["-fd"],
            &["-fx"],
            &["-fdx"],
            &["-fX"],
            &["-fdX"],
            &["-fdff"],
            &["-fdxff"],
            &["-fdq"],
            &["--force", "-d", "-e", "newd/"],
            &["-fd", "dir"],
            &["-f", "newd"],
            &["-f", "-ff", "holder"],
            &["-fdx", "mixed"],
            &["-fd", "u", "x.log"],
            &["-fdx", "alink", "outlink"],
            &["-f", "outlink"],
        ] {
            repo(format, "*.log\nign/\n").same("", args, format);
        }
    }
}

#[test]
fn a_folder_you_stand_in_is_scoped_and_shown_relative() {
    for sub in ["dir", "dir/sub", "newd", "newd/g", "mixed"] {
        for args in [
            &["-n"][..],
            &["-nd"],
            &["-ndx"],
            &["-ndX"],
            &["-nd", ".."],
            &["-nd", "../other"],
            &["-n", "."],
            &["-fd"],
        ] {
            repo("sha1", "*.log\nign/\n").same(sub, args, sub);
        }
    }
}

#[test]
fn gitignore_files_in_untracked_folders_apply() {
    for args in [&["-nd"][..], &["-ndX"], &["-ndx"], &["-fd"]] {
        let p = repo("sha1", "*.log\n");
        for side in [&p.git, &p.pyn] {
            write(side, "nest/.gitignore", "*.dat\n!keep.dat\n/only\n");
            write(side, "nest/a.dat", "x");
            write(side, "nest/keep.dat", "x");
            write(side, "nest/only", "x");
            write(side, "nest/deeper/only", "x");
            write(side, "nest/deeper/.gitignore", "!a.dat\n");
            write(side, "nest/deeper/a.dat", "x");
        }
        p.same("", args, "nested ignore files");
    }
}

#[test]
fn info_exclude_and_global_excludes_apply() {
    let p = repo("sha1", "");
    for side in [&p.git, &p.pyn] {
        let exclude = side.join(".git/info/exclude");
        fs::create_dir_all(exclude.parent().unwrap()).unwrap();
        fs::write(exclude, "excluded\nmixed/\n").unwrap();
        write(side, "excluded", "x");
    }
    p.same("", &["-nd"], "exclude");
    p.same("", &["-ndX"], "exclude");
    p.same("", &["-fdX"], "exclude");
}

#[test]
fn refuses_without_force_or_dry_run() {
    for args in [&[][..], &["-d"], &["-x"], &["-q"], &["-dx", "u"]] {
        repo("sha1", "*.log\n").same("", args, "no force");
    }
    let p = repo("sha1", "*.log\n");
    let out = p.env.pyn(&p.pyn, &["clean", "-d"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("clean.requireForce defaults to true and neither -i, -n, nor -f given")
    );
    assert!(p.pyn.join("u").exists());
}

#[test]
fn require_force_can_be_turned_off_in_the_config() {
    let p = repo("sha1", "*.log\n");
    for side in [&p.git, &p.pyn] {
        p.env.ok(side, &["config", "clean.requireForce", "false"]);
    }
    p.same("", &["-d"], "requireForce=false");
    let p = repo("sha1", "*.log\n");
    for side in [&p.git, &p.pyn] {
        p.env.ok(side, &["config", "clean.requireForce", "true"]);
    }
    p.same("", &["-d"], "requireForce=true");
}

#[test]
fn x_and_capital_x_cannot_be_combined() {
    repo("sha1", "*.log\n").same("", &["-nxX"], "x and X");
}

#[test]
fn interactive_sessions_match_git() {
    let inputs = [
        "",
        "1\n",
        "c\n",
        "5\n",
        "q\n",
        "6\n1\n",
        "?\nxyz\n\n1\n",
        "4\ny\ny\ny\ny\ny\ny\ny\ny\n",
        "4\ny\nn\ny\nn\ny\nn\n",
        "4\nyes\nYES\nye\nyy\n y\nn\n",
        "4\ny",
        "3\n1 3\n\n1\n",
        "3\n1-2,4\n\n1\n",
        "3\n*\n-2\n\n1\n",
        "3\n2-\n\n1\n",
        "3\nnew\n\n1\n",
        "3\nd\n9\n0\n-\n3-1\n\n1\n",
        "3\n1\n",
        "2\n*\n1\n",
        "2\n*.log u\n\n1\n",
        "2\nu\nnew*\n\n1\n",
        "2\ndir/*\n\n1\n",
        "2\nnewd\n\n1\n",
        "2\nnewd/\n\n1\n",
        "2\n!u *\n\n1\n",
        "2\n*\nu\n1\n",
        "2\nnothing\n\n1\n",
        "2\nsub\n\n1\n",
        "2\ndir\n\n1\n",
        "2\ndir/s*\n\n1\n",
        "2\n*3 u?\n\n1\n",
        "2\n/u\n\n1\n",
        "2\nsub/*\ndir/\n\n1\n",
        "3\ndir/s\n\n1\n",
        "3\ndir/u\n\n1\n",
        "3\nDIR/U2\n\n1\n",
        "2\nu\n3\n1\n\n1\n",
    ];
    for args in [
        &["-id"][..],
        &["-i"],
        &["-idx"],
        &["-idX"],
        &["-idff"],
        &["-idn"],
        &["-id", "dir"],
    ] {
        for input in inputs {
            repo("sha1", "*.log\nign/\n").same_typed("", args, input, "root");
        }
    }
    for sub in ["dir", "dir/sub", "newd/g", "holder"] {
        for input in ["1\n", "4\ny\ny\ny\n", "3\n1\n\n1\n", "2\n*\n"] {
            repo("sha1", "*.log\nign/\n").same_typed(sub, &["-id"], input, sub);
            repo("sha1", "*.log\nign/\n").same_typed(sub, &["-id", ".."], input, sub);
        }
    }
}

#[test]
fn interactive_clean_needs_no_force_and_nothing_to_clean_is_silent() {
    let p = repo("sha1", "*.log\n");
    p.same_typed("", &["-i"], "1\n", "no force");
    let q = repo("sha1", "*.log\n");
    q.same("", &["-fdxff"], "everything");
    q.same_typed("", &["-idx"], "1\n", "nothing left");
    let out = run_with(q.env.pyn_cmd(&q.pyn, &["clean", "-id"]), "1\n");
    assert!(out.status.success() && out.stdout.is_empty(), "{out:?}");
}

#[test]
fn glob_pathspecs_match_git() {
    for spec in [
        "*.log",
        "u*",
        "dir/*",
        "dir/s*",
        "*/u2",
        "*u*",
        "new*",
        "newd/*",
        "newd/g*",
        "*b*",
        "?",
        "??",
        "[ux]*",
        "[!u]*",
        "mixed/*",
        "*/*",
        "**",
        "dir/**/u3",
        "*.nothing",
        "holder/*",
        "\\u",
        "u[",
    ] {
        for args in [
            &["-nd"][..],
            &["-n"],
            &["-ndx"],
            &["-ndX"],
            &["-ndff"],
            &["-fd"],
            &["-fdx"],
        ] {
            let mut full = args.to_vec();
            full.push(spec);
            repo("sha1", "*.log\nign/\n").same("", &full, spec);
        }
    }
}

#[test]
fn glob_pathspecs_are_relative_to_the_current_folder() {
    for sub in ["dir", "dir/sub", "newd"] {
        for spec in [
            "*", "*.log", "u*", "s*", "../u*", "../*/u2", "../new*", "../*",
        ] {
            repo("sha1", "*.log\nign/\n").same(sub, &["-ndx", spec], spec);
            repo("sha1", "*.log\nign/\n").same(sub, &["-fd", spec], spec);
        }
    }
}

#[test]
fn a_literal_name_that_looks_like_a_glob_still_matches_itself() {
    let p = repo("sha1", "");
    for side in [&p.git, &p.pyn] {
        write(side, "odd[1].txt", "x");
        write(side, "odd1.txt", "x");
    }
    p.same("", &["-n", "odd[1].txt"], "literal");
    p.same("", &["-f", "odd[1].txt"], "literal removal");
}

fn small_repo() -> (tempfile::TempDir, Env, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let env = Env {
        home: root.join("home"),
    };
    fs::create_dir_all(&env.home).unwrap();
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    for config in [
        &["init", "-q", "-b", "main"][..],
        &["config", "gc.auto", "0"],
        &["config", "maintenance.auto", "false"],
    ] {
        env.ok(&work, config);
    }
    write(&work, "tracked", "t\n");
    env.ok(&work, &["add", "-A"]);
    env.ok(&work, &["commit", "-q", "-m", "first"]);
    for name in ["a.txt", "b.log", "c.txt", "d/x", "e/z"] {
        write(&work, name, "");
    }
    (tmp, env, work)
}

#[test]
fn the_transcript_of_a_clean_session() {
    let (_tmp, env, work) = small_repo();
    let out = run_with(env.pyn_cmd(&work, &["clean", "-id"]), "xyz\n\n6\n1\n");
    let menu = "*** Commands ***\n    1: clean                2: filter by pattern    3: select by numbers\n    4: ask each             5: quit                 6: help\nWhat now> ";
    let help = "clean               - start cleaning\nfilter by pattern   - exclude items from deletion\nselect by numbers   - select items to be deleted by numbers\nask each            - confirm each deletion (like \"rm -i\")\nquit                - stop cleaning\nhelp                - this screen\n?                   - help for prompt selection\n";
    let list = "Would remove the following items:\n  a.txt  b.log  c.txt  d/     e/\n";
    let expected = format!(
        "{list}{menu}Huh (xyz)?\n{menu}{menu}{help}{list}{menu}Removing a.txt\nRemoving b.log\nRemoving c.txt\nRemoving d/\nRemoving e/\n"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    assert!(out.status.success());
    assert!(work.join("tracked").exists() && !work.join("a.txt").exists());
}

#[test]
fn the_transcript_of_filtering_selecting_and_asking() {
    let (_tmp, env, work) = small_repo();
    let menu = "*** Commands ***\n    1: clean                2: filter by pattern    3: select by numbers\n    4: ask each             5: quit                 6: help\nWhat now> ";
    let out = run_with(
        env.pyn_cmd(&work, &["clean", "-id"]),
        "2\n*.txt\nzz\n\n3\n1-2 9\n?\n\n4\ny\n",
    );
    let expected = [
        "Would remove the following items:",
        "  a.txt  b.log  c.txt  d/     e/",
        &format!("{menu}  a.txt  b.log  c.txt  d/     e/"),
        "Input ignore patterns>>   b.log  d/     e/",
        "Input ignore patterns>> WARNING: Cannot find items matched by: zz",
        "Input ignore patterns>> Would remove the following items:",
        "  b.log  d/     e/",
        &format!("{menu}    1: b.log    2: d/       3: e/"),
        "Select items to delete>> Huh (9)?",
        "  * 1: b.log  * 2: d/       3: e/",
        "Select items to delete>> Prompt help:",
        "1          - select a single item",
        "3-5        - select a range of items",
        "2-3,6-9    - select multiple ranges",
        "foo        - select item based on unique prefix",
        "-...       - unselect specified items",
        "*          - choose all items",
        "           - (empty) finish selecting",
        "  * 1: b.log  * 2: d/       3: e/",
        "Select items to delete>> Would remove the following items:",
        "  b.log  d/",
        &format!("{menu}Remove b.log [y/N]? Remove d/ [y/N]? "),
        "Removing b.log",
        "",
    ]
    .join("\n");
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected);
    assert!(
        !work.join("b.log").exists() && work.join("d/x").exists() && work.join("a.txt").exists()
    );
}

#[test]
fn columns_follow_the_columns_variable() {
    let (_tmp, env, work) = small_repo();
    let mut cmd = env.pyn_cmd(&work, &["clean", "-id"]);
    cmd.env("COLUMNS", "10");
    let out = run_with(cmd, "5\n");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.starts_with("Would remove the following items:\n  a.txt\n  b.log\n  c.txt\n  d/\n  e/\n*** Commands ***\n    1: clean\n    2: filter by pattern\n"),
        "{text}"
    );
    assert!(text.ends_with("What now> Bye.\n"));
}

#[test]
fn end_of_input_quits_and_unreadable_answers_remove_nothing() {
    let (_tmp, env, work) = small_repo();
    for input in ["", "3\n", "3\n1", "2\n", "4\nn\nn\nn\nn\nn\n", "4"] {
        let out = run_with(env.pyn_cmd(&work, &["clean", "-id"]), input);
        assert!(out.status.success(), "{input:?}: {out:?}");
        assert!(work.join("a.txt").exists(), "{input:?} removed something");
    }
}

#[test]
fn links_are_removed_not_followed() {
    let p = repo("sha1", "*.log\n");
    let out = p.env.pyn(&p.pyn, &["clean", "-fdx"]);
    assert!(out.status.success(), "{out:?}");
    let outside = p.outside();
    assert_eq!(
        fs::read_to_string(outside.join("keep")).unwrap(),
        "must survive\n"
    );
    assert!(fs::symlink_metadata(p.pyn.join("outlink")).is_err());
    assert!(!p.pyn.join("newd").exists());
}

#[test]
fn a_path_through_a_link_or_outside_the_repository_removes_nothing() {
    let p = repo("sha1", "*.log\n");
    let before = {
        let mut t = BTreeMap::new();
        tree(&p.pyn, &p.pyn, &mut t);
        t
    };
    for args in [
        &["-fd", "outlink/keep"][..],
        &["-fd", "alink/x"],
        &["-fd", "../outside"],
        &["-fd", "/"],
    ] {
        let mut full = vec!["clean"];
        full.extend(args);
        let out = p.env.pyn(&p.pyn, &full);
        let mut after = BTreeMap::new();
        tree(&p.pyn, &p.pyn, &mut after);
        assert_eq!(after, before, "{args:?}: {out:?}");
        assert!(p.outside().join("keep").is_file());
    }
}

#[test]
fn tracked_files_and_the_pyn_folder_are_never_removed() {
    let p = repo("sha1", "*.log\n.pyn/\n");
    for side in [&p.git, &p.pyn] {
        write(side, ".pyn/local_only/state", "keep me");
    }
    let out = p.env.pyn(&p.pyn, &["clean", "-fdxff"]);
    assert!(out.status.success(), "{out:?}");
    for kept in [
        "a",
        "dir/b",
        "dir/sub/c",
        "other/d",
        ".gitignore",
        ".pyn/local_only/state",
    ] {
        assert!(p.pyn.join(kept).exists(), "{kept}");
    }
    assert!(p.pyn.join(".git").is_dir());
}

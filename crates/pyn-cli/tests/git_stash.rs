//! `pyn stash push -- <paths>`, `-k`, `show`, `branch`, `create`, `store` and `--index` on a git repository, compared
//! with the real `git stash` on a copy of the same repository: exit status, messages, work tree, index, refs and
//! reflogs (so the stash commits have the same ids). Needs `git` on the PATH (tests only).

mod common;
#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::{copy_tree, logs_dump, tree_dump};

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

impl Pair {
    fn git_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "merge.conflictStyle=diff3"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn pyn_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    fn ls_files(&self, dir: &Path) -> String {
        String::from_utf8_lossy(&self.git_in(dir, &["ls-files", "-s"]).stdout).into_owned()
    }

    fn refs(&self, dir: &Path) -> String {
        let out = self.git_in(dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
        let head = self.git_in(dir, &["symbolic-ref", "-q", "HEAD"]);
        let orig = fs::read_to_string(dir.join(".git/ORIG_HEAD")).unwrap_or_default();
        format!(
            "{}{}ORIG_HEAD {orig}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&head.stdout)
        )
    }
}

/// The lines whose wording is git's own and does not depend on the status layout.
fn keep(text: &[u8], everything: bool) -> String {
    let text = String::from_utf8_lossy(text);
    text.lines()
        .filter(|l| {
            everything
                || l.starts_with("error:")
                || l.starts_with("fatal:")
                || l.starts_with("Did you forget")
                || l.starts_with("Saved working")
                || l.starts_with("Dropped ")
                || l.starts_with("No local changes")
                || l.starts_with("No stash")
                || l.starts_with("Switched to")
                || l.starts_with("Index was not")
                || l.starts_with("The stash entry")
                || (l.len() >= 40 && l.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .filter(|l| {
            // wording only some git versions have
            !l.starts_with("error: patch failed")
                && !l.ends_with("patch does not apply")
                && *l != "error: could not write index"
        })
        .map(|l| {
            let l = l.replace("(,prefix", "(prefix");
            let l = l
                .split(" is outside repository at")
                .next()
                .unwrap_or(&l)
                .to_string();
            format!("{}\n", l.trim_end_matches('.'))
        })
        .collect()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&file);
    fs::write(file, bytes).unwrap();
}

fn pair() -> Pair {
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
    let ok = |args: &[&str]| {
        let out = p.git_in(&p.git, args);
        assert!(out.status.success(), "git {args:?}");
    };
    ok(&["init", "-q", "-b", "main"]);
    write(&p.git, "a.txt", b"a1\na2\na3\n");
    write(&p.git, "b.txt", b"b1\nb2\nb3\n");
    write(&p.git, "c.txt", b"c\n");
    write(&p.git, "d/e.txt", b"e1\ne2\ne3\n");
    write(&p.git, "d/f.txt", b"f\n");
    write(&p.git, "s.sh", b"#!/bin/sh\n");
    write(&p.git, "bin", b"x\0y\n");
    write(
        &p.git,
        "big.txt",
        b"l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\n",
    );
    std::os::unix::fs::symlink("a.txt", p.git.join("l")).unwrap();
    ok(&["update-index", "--add", "--chmod=+x", "s.sh"]);
    ok(&["add", "-A"]);
    ok(&["commit", "-q", "-m", "base"]);
    write(&p.git, "a.txt", b"a1\na2\na3\na4\n");
    ok(&["commit", "-q", "-am", "second"]);
    p
}

/// One step: `@git ..` runs git on both, `@w path text` writes a file in both, `@rm path` removes one, `@in dir
/// ..` runs the rest from that folder; anything else runs in both `git` and `pyn`, which must agree.
type Step<'a> = &'a [&'a str];

/// Every kind of local change at once.
const MIXED: &[Step<'static>] = &[
    &["@w", "a.txt", "A1\na2\na3\na4\n"],
    &["@w", "b.txt", "b1\nB2\nb3\n"],
    &["@git", "add", "b.txt"],
    &["@w", "b.txt", "b1\nB2\nB3\n"],
    &["@rm", "c.txt"],
    &["@w", "d/e.txt", "e1\nE2\ne3\n"],
    &["@git", "add", "d/e.txt"],
    &["@w", "d/new.txt", "new\n"],
    &["@git", "add", "d/new.txt"],
    &["@git", "rm", "-q", "d/f.txt"],
    &["@git", "update-index", "--chmod=-x", "s.sh"],
    &["@w", "u.txt", "untracked\n"],
    &["@w", "ud/x.txt", "x\n"],
    &["@w", "ud/y/z.txt", "z\n"],
];

fn run(steps: &[Step<'_>]) {
    let p = pair();
    let mut copied = false;
    let mut cmp_all = false;
    for (n, step) in steps.iter().enumerate() {
        let (git_dir, pyn_dir);
        let mut step: Step<'_> = step;
        let mut sub = "";
        if step[0] == "@in" {
            sub = step[1];
            step = &step[2..];
        }
        if step[0] == "@show" {
            cmp_all = true;
            step = &step[1..];
        }
        if !copied && !step[0].starts_with('@') {
            copy_tree(&p.git, &p.pyn);
            copied = true;
        }
        match step[0] {
            "@git" | "@w" | "@rm" => {
                for root in [&p.git, &p.pyn] {
                    if copied || root == &p.git {
                        match step[0] {
                            "@git" => {
                                let out = p.git_in(root, &step[1..]);
                                assert!(
                                    out.status.success(),
                                    "step {n}: git {:?}: {}",
                                    &step[1..],
                                    String::from_utf8_lossy(&out.stderr)
                                );
                            }
                            "@w" => write(root, step[1], step[2].replace("\\n", "\n").as_bytes()),
                            _ => fs::remove_file(root.join(step[1])).unwrap(),
                        }
                    }
                }
                continue;
            }
            _ => {}
        }
        git_dir = p.git.join(sub);
        pyn_dir = p.pyn.join(sub);
        let g = p.git_in(&git_dir, step);
        let y = p.pyn_in(&pyn_dir, step);
        let what = format!("step {n} {step:?} in '{sub}'");
        assert_eq!(
            oracle::status_class(y.status.code().unwrap()),
            oracle::status_class(g.status.code().unwrap()),
            "{what}: exit; pyn: {}; git: {}",
            String::from_utf8_lossy(&y.stderr),
            String::from_utf8_lossy(&g.stderr)
        );
        assert_eq!(
            keep(&y.stderr, false),
            keep(&g.stderr, false),
            "{what}: stderr (pyn, then git)"
        );
        assert_eq!(
            keep(&y.stdout, cmp_all),
            keep(&g.stdout, cmp_all),
            "{what}: stdout (pyn, then git)"
        );
        assert_eq!(
            tree_dump(&p.pyn),
            tree_dump(&p.git),
            "{what}: work tree (pyn, then git)"
        );
        assert_eq!(
            p.ls_files(&p.pyn),
            p.ls_files(&p.git),
            "{what}: index (pyn, then git)"
        );
        assert_eq!(
            p.refs(&p.pyn),
            p.refs(&p.git),
            "{what}: refs (pyn, then git)"
        );
        assert_eq!(
            logs_dump(&p.pyn.join(".git")),
            logs_dump(&p.git.join(".git")),
            "{what}: reflogs (pyn, then git)"
        );
        cmp_all = false;
    }
}

fn with<'a>(extra: &[Step<'a>]) -> Vec<Step<'a>> {
    let mut all = MIXED.to_vec();
    all.extend_from_slice(extra);
    all
}

#[test]
fn push_of_named_paths_matches_git() {
    for args in [
        &["a.txt"][..],
        &["a.txt", "d"],
        &["d"],
        &["d/e.txt", "d/new.txt"],
        &["b.txt", "c.txt"],
        &["d/f.txt"],
        &["d/new.txt"],
        &["."],
        &["l", "bin", "s.sh"],
        &["a.txt", "a.txt"],
        &["d/", "./a.txt"],
    ] {
        let mut cmd: Vec<&str> = vec!["stash", "push", "--"];
        cmd.extend(args.iter().copied());
        run(&with(&[&cmd]));
    }
}

#[test]
fn push_options_with_paths_match_git() {
    for cmd in [
        &["stash", "push", "-m", "named", "--", "a.txt"][..],
        &["stash", "push", "-u", "--", "u.txt"],
        &["stash", "push", "-u", "--", "ud"],
        &["stash", "push", "-u", "--", "ud", "a.txt"],
        &["stash", "push", "-u", "-m", "both", "--", "."],
        &["stash", "push", "-k", "--", "b.txt"],
        &["stash", "push", "-k", "--", "b.txt", "d"],
        &["stash", "push", "-k", "-u", "--", "d", "u.txt"],
        &["stash", "push", "--keep-index", "--", "."],
        &["stash", "push", "-k"],
        &["stash", "push", "-k", "-u"],
        &["stash", "push", "-k", "-m", "kept"],
        &["stash", "-k"],
    ] {
        run(&with(&[cmd]));
    }
}

#[test]
fn push_refusals_and_nothing_to_save_match_git() {
    for cmd in [
        &["stash", "push", "--", "nope"][..],
        &["stash", "push", "--", "a.txt", "nope", "also"],
        &["stash", "push", "--", "u.txt"],
        &["stash", "push", "--", "d/f.txt"],
        &["stash", "push", "--", ""],
        &["stash", "push", "--", "../outside"],
        &["stash", "push", "--", "nope/"],
    ] {
        run(&with(&[cmd]));
    }
    run(&[
        &["stash", "push", "--", "a.txt"],
        &["stash", "push", "--", "b.txt"],
        &["stash", "push", "-u", "--", "nope"],
    ]);
}

#[test]
fn push_from_a_folder_matches_git() {
    for cmd in [
        &["@in", "d", "stash", "push", "--", "e.txt"][..],
        &["@in", "d", "stash", "push", "--", "."],
        &["@in", "d", "stash", "push", "--", "../a.txt", "new.txt"],
        &["@in", "d", "stash", "push", "--", "nope"],
        &["@in", "d", "stash", "push", "--", "../nope"],
        &["@in", "d", "stash", "push", "-k", "--", "../b.txt"],
    ] {
        run(&with(&[cmd]));
    }
}

#[test]
fn staged_state_survives_pop_and_apply_with_index() {
    for tail in [
        &[&["stash", "push"][..], &["stash", "pop", "--index"]][..],
        &[&["stash", "push"], &["stash", "apply", "--index"]],
        &[&["stash", "push", "-u"], &["stash", "pop", "--index"]],
        &[&["stash", "push"], &["stash", "pop"]],
        &[
            &["stash", "push", "--", "b.txt", "d"],
            &["stash", "pop", "--index"],
        ],
        &[
            &["stash", "push", "--", "d"],
            &["stash", "push", "--", "b.txt"],
            &["stash", "apply", "--index", "stash@{1}"],
            &["stash", "apply", "--index", "1"],
            &["stash", "pop", "--index", "stash@{1}"],
        ],
        &[
            &["stash", "push", "-k"],
            &["@git", "reset", "-q", "--hard"],
            &["stash", "pop", "--index"],
        ],
        &[
            &["stash", "push"],
            &["stash", "pop", "--index", "stash@{7}"],
        ],
        &[&["stash", "push"], &["stash", "apply", "--index", "nope"]],
    ] {
        let mut steps = with(&[]);
        steps.extend_from_slice(tail);
        run(&steps);
    }
}

const BIG: &str = "l1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9\nl10\nl11\nl12\n";

#[test]
fn index_conflicts_and_work_tree_conflicts_match_git() {
    let staged = BIG.replace("l1\n", "S1\n");
    let worked = staged.replace("l12\n", "W12\n");
    let rival = BIG.replace("l1\n", "X1\n");
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let (staged, worked, rival) = (leak(staged), leak(worked), leak(rival));
    run(&[
        &["@w", "big.txt", staged],
        &["@git", "add", "big.txt"],
        &["@w", "big.txt", worked],
        &["stash", "push"],
        &["@w", "big.txt", rival],
        &["@git", "add", "big.txt"],
        &["stash", "apply", "--index"],
        &["stash", "pop", "--index"],
        &["stash", "apply"],
        &["@git", "reset", "-q", "--hard"],
        &["stash", "pop"],
        &["stash", "list"],
    ]);
    run(&[
        &["@w", "c.txt", "C1\n"],
        &["@git", "add", "c.txt"],
        &["@w", "big.txt", staged],
        &["stash", "push"],
        &["@w", "big.txt", rival],
        &["@git", "commit", "-q", "-am", "z"],
        &["stash", "apply", "--index"],
        &["stash", "pop", "--index"],
        &["stash", "list"],
    ]);
}

#[test]
fn show_matches_git() {
    for show in [
        &["@show", "stash", "show"][..],
        &["@show", "stash", "show", "-p"],
        &["@show", "stash", "show", "--stat"],
        &["@show", "stash", "show", "--patch", "--stat"],
        &["@show", "stash", "show", "--shortstat"],
        &["@show", "stash", "show", "--name-only"],
        &["@show", "stash", "show", "--name-status"],
        &["@show", "stash", "show", "-u"],
        &["@show", "stash", "show", "--include-untracked", "-p"],
        &["@show", "stash", "show", "--only-untracked"],
        &["@show", "stash", "show", "--only-untracked", "-p"],
        &["@show", "stash", "show", "-U1", "-p"],
        &["@show", "stash", "show", "stash@{1}"],
        &["@show", "stash", "show", "1", "-p"],
        &["@show", "stash", "show", "stash@{5}"],
        &["@show", "stash", "show", "nope"],
        &["@show", "stash", "show", "stash@{0}", "stash@{1}"],
        &["@show", "stash", "show", "HEAD"],
    ] {
        run(&with(&[
            &["stash", "push", "-u", "-m", "first", "--", "a.txt", "ud"],
            &["stash", "push", "-u"],
            show,
        ]));
    }
    run(&[&["stash", "show"], &["stash", "list"]]);
}

#[test]
fn show_honours_the_stash_settings() {
    for (key, value) in [
        ("stash.showPatch", "true"),
        ("stash.showStat", "false"),
        ("stash.showIncludeUntracked", "true"),
    ] {
        run(&with(&[
            &["@git", "config", key, value],
            &["stash", "push", "-u"],
            &["@show", "stash", "show"],
        ]));
    }
}

#[test]
fn branch_matches_git() {
    run(&with(&[
        &["stash", "push", "-u"],
        &["@git", "commit", "-q", "--allow-empty", "-m", "later"],
        &["stash", "branch", "topic"],
    ]));
    run(&with(&[
        &["stash", "push", "-u", "-m", "one"],
        &["@w", "a.txt", "other\n"],
        &["@git", "commit", "-q", "-am", "later"],
        &["stash", "push", "-m", "two", "--", "b.txt"],
        &["stash", "branch", "topic", "stash@{1}"],
        &["stash", "list"],
    ]));
    run(&with(&[
        &["stash", "push"],
        &["stash", "branch", "main"],
        &["stash", "branch", "bad name"],
        &["stash", "branch", "topic", "stash@{4}"],
    ]));
    run(&[&["stash", "branch", "topic"]]);
}

#[test]
fn branch_with_a_conflict_keeps_the_stash() {
    run(&[
        &["@w", "b.txt", "b1\nB2\nb3\n"],
        &["@git", "add", "b.txt"],
        &["@w", "b.txt", "b1\nB2\nB3\n"],
        &["stash", "push"],
        &["stash", "branch", "topic"],
        &["stash", "list"],
    ]);
}

#[test]
fn create_and_store_match_git() {
    run(&with(&[
        &["stash", "create"],
        &["stash", "create", "my", "message"],
        &["stash", "list"],
    ]));
    run(&[&["stash", "create"], &["stash", "list"]]);
    for store in [
        &["stash", "store", "$ID"][..],
        &["stash", "store", "-m", "stored", "$ID"],
        &["stash", "store", "--message=stored", "-q", "$ID"],
        &["stash", "store"],
        &["stash", "store", "nope"],
        &["stash", "store", "HEAD"],
    ] {
        store_run(store);
    }
}

/// `stash create` on both, then `store` with the id it printed (`$ID`), twice to see a repeat do nothing.
fn store_run(store: &[&str]) {
    let p = pair();
    write(&p.git, "a.txt", b"changed\n");
    write(&p.git, "n.txt", b"n\n");
    assert!(p.git_in(&p.git, &["add", "n.txt"]).status.success());
    copy_tree(&p.git, &p.pyn);
    let id = String::from_utf8(p.git_in(&p.git, &["stash", "create", "x"]).stdout).unwrap();
    let mine = String::from_utf8(p.pyn_in(&p.pyn, &["stash", "create", "x"]).stdout).unwrap();
    assert_eq!(mine, id, "the created stash commit");
    assert_eq!(id.trim().len(), 40);
    let args: Vec<&str> = store
        .iter()
        .map(|a| if *a == "$ID" { id.trim() } else { *a })
        .collect();
    for round in 0..2 {
        let g = p.git_in(&p.git, &args);
        let y = p.pyn_in(&p.pyn, &args);
        let what = format!("round {round} {args:?}");
        assert_eq!(
            oracle::status_class(y.status.code().unwrap()),
            oracle::status_class(g.status.code().unwrap()),
            "{what}: exit"
        );
        assert_eq!(
            keep(&y.stderr, false),
            keep(&g.stderr, false),
            "{what}: stderr"
        );
        assert_eq!(
            logs_dump(&p.pyn.join(".git")),
            logs_dump(&p.git.join(".git")),
            "{what}: reflogs"
        );
        assert_eq!(p.refs(&p.pyn), p.refs(&p.git), "{what}: refs");
        assert_eq!(tree_dump(&p.pyn), tree_dump(&p.git), "{what}: work tree");
    }
}

#[test]
fn list_drop_and_pop_name_stashes_as_git_does() {
    run(&with(&[
        &["stash", "push", "-m", "one", "--", "a.txt"],
        &["stash", "push", "-m", "two", "--", "b.txt"],
        &["stash", "list"],
        &["stash", "drop", "1"],
        &["stash", "drop", "stash@{4}"],
        &["stash", "drop", "9"],
        &["stash", "apply", "nope"],
        &["stash", "pop", "HEAD"],
        &["stash", "pop", "stash@{0}"],
        &["stash", "pop"],
        &["stash", "pop"],
        &["stash", "apply", "0"],
    ]));
}

#[test]
fn a_sha256_repository_round_trips_through_the_new_options() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let home = dir.join("home");
    fs::create_dir_all(&home).unwrap();
    let work = dir.join("w");
    fs::create_dir_all(&work).unwrap();
    let p = Pair {
        home,
        git: work.clone(),
        pyn: work.clone(),
        _tmp: tempfile::tempdir().unwrap(),
    };
    for args in [
        &["init", "-q", "-b", "main", "--object-format=sha256"][..],
        &["commit", "-q", "--allow-empty", "-m", "empty"],
    ] {
        assert!(p.git_in(&work, args).status.success());
    }
    write(&work, "a.txt", b"a\n");
    write(&work, "b.txt", b"b\n");
    assert!(p.git_in(&work, &["add", "-A"]).status.success());
    assert!(
        p.git_in(&work, &["commit", "-q", "-m", "files"])
            .status
            .success()
    );
    write(&work, "a.txt", b"A\n");
    write(&work, "b.txt", b"B\n");
    assert!(p.git_in(&work, &["add", "b.txt"]).status.success());
    for args in [
        &["stash", "push", "--", "a.txt"][..],
        &["stash", "push", "-k"],
        &["stash", "pop", "--index"],
        &["stash", "show", "-p"],
        &["stash", "pop", "--index"],
    ] {
        let out = p.pyn_in(&work, args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(p.git_in(&work, &["fsck", "--strict"]).status.success());
    assert_eq!(fs::read(work.join("a.txt")).unwrap(), b"A\n");
    assert_eq!(fs::read(work.join("b.txt")).unwrap(), b"B\n");
}

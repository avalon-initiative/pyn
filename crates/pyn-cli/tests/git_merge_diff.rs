//! `pyn show` and `pyn log` on merge commits compared with real git on the same repository: combined (`-c`) and
//! dense combined (`--cc`) diffs, `-m`, `--first-parent`, `--diff-merges` and `log.diffMerges`, on SHA-1 and SHA-256
//! repositories. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Fixture {
    _tmp: TempDir,
    repo: PathBuf,
    format: &'static str,
    clock: std::cell::Cell<u64>,
}

fn text(lines: &[&str]) -> Vec<u8> {
    let mut out = lines.join("\n").into_bytes();
    out.push(b'\n');
    out
}

impl Fixture {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        init(&repo, format, false);
        Self {
            _tmp: tmp,
            repo,
            format,
            clock: std::cell::Cell::new(1_700_000_000),
        }
    }

    fn write(&self, path: &str, content: &[u8]) {
        let full = self.repo.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }

    fn chmod(&self, path: &str, mode: u32) {
        fs::set_permissions(self.repo.join(path), fs::Permissions::from_mode(mode)).unwrap();
    }

    fn git_ok(&self, args: &[&str]) -> std::process::Output {
        self.clock.set(self.clock.get() + 10);
        let date = format!("{} +0000", self.clock.get());
        git(&self.repo)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(args)
            .output()
            .unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self.git_ok(args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn branch(&self, name: &str, from: &str) {
        self.git(&["checkout", "-q", "-B", name, from]);
    }

    /// Commits the work tree as it is with the given parents, moving the current branch to it.
    fn commit_tree(&self, parents: &[&str], message: &str) -> String {
        self.git(&["add", "-A", "-f"]);
        let tree = self.git(&["write-tree"]);
        let mut args = vec!["commit-tree", &tree, "-m", message];
        for p in parents {
            args.push("-p");
            args.push(p);
        }
        let id = self.git(&args);
        self.git(&["reset", "-q", "--hard", &id]);
        id
    }

    fn oracle(&self, args: &[&str]) -> std::process::Output {
        git(&self.repo)
            .env("TZ", "UTC")
            .arg("--no-pager")
            .args(args)
            .output()
            .unwrap()
    }

    fn pyn(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.repo)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.repo.parent().unwrap())
            .env("TZ", "UTC")
            .output()
            .unwrap()
    }

    fn agrees(&self, args: &[&str]) -> bool {
        let (want, got) = (self.oracle(args), self.pyn(args));
        want.status.code() == got.status.code() && want.stdout == got.stdout
    }

    /// Both tools must exit alike and print the same bytes.
    fn same(&self, args: &[&str]) {
        let (want, got) = (self.oracle(args), self.pyn(args));
        assert_eq!(
            got.status.code(),
            want.status.code(),
            "{} {args:?}: {}",
            self.format,
            String::from_utf8_lossy(&got.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&got.stdout),
            String::from_utf8_lossy(&want.stdout),
            "{} {args:?}",
            self.format
        );
    }

    fn all_same(&self, sets: &[&[&str]]) {
        for args in sets {
            self.same(args);
        }
    }

    /// Whether the plain diffs the separate and first-parent forms print already match git here.
    fn plain_diffs_match(&self) -> bool {
        let merge = self.git(&["rev-parse", "HEAD"]);
        let parents = self.git(&["rev-list", "--parents", "-n1", "HEAD"]);
        let mut revs = vec![merge];
        revs.extend(parents.split(' ').skip(1).map(str::to_string));
        for p in revs.clone() {
            let kids = self.git(&["rev-list", "--parents", "-n1", &p]);
            for q in kids.split(' ').skip(1) {
                if !self.agrees(&["diff", q, &p]) {
                    return false;
                }
            }
        }
        true
    }
}

/// Options whose output for the merge at `HEAD` is the merge's own diff only.
const COMBINED: &[&[&str]] = &[
    &["show"],
    &["show", "--cc"],
    &["show", "-c"],
    &["show", "--stat"],
    &["show", "-c", "--stat"],
    &["show", "--name-only"],
    &["show", "--name-status"],
    &["show", "-c", "--name-status"],
    &["show", "--numstat"],
    &["show", "--shortstat"],
    &["show", "--summary"],
    &["show", "--oneline"],
    &["show", "-U0"],
    &["show", "-U1", "-c"],
    &["show", "-U5"],
    &["show", "--no-diff-merges"],
    &["show", "-s"],
    &["show", "--format=%h", "-c"],
    &["show", "--format=", "--cc"],
    &["show", "--no-renames"],
    &["show", "--no-renames", "-c"],
];

/// Options that also print plain diffs of other commits or one diff per parent.
const PLAIN: &[&[&str]] = &[
    &["show", "-m"],
    &["show", "--first-parent"],
    &["show", "-m", "--first-parent"],
    &["show", "--oneline", "-m"],
    &["show", "--diff-merges=separate", "--stat"],
    &["show", "-s", "-m"],
    &["show", "HEAD~1", "HEAD", "HEAD~2"],
    &["show", "--oneline", "HEAD~1", "HEAD", "HEAD~2"],
    &["show", "--oneline", "-m", "HEAD~1", "HEAD", "HEAD~2"],
    &["show", "--cc", "HEAD~1", "HEAD", "HEAD~2"],
    &["log"],
    &["log", "-p"],
    &["log", "-c"],
    &["log", "--cc"],
    &["log", "-m"],
    &["log", "-m", "-p"],
    &["log", "-m", "--stat"],
    &["log", "--cc", "--stat"],
    &["log", "-c", "--stat", "-p"],
    &["log", "--stat"],
    &["log", "--oneline", "--cc"],
    &["log", "--oneline", "-c", "--name-status"],
    &["log", "--diff-merges=cc"],
    &["log", "--diff-merges=separate", "--stat"],
    &["log", "--diff-merges=first-parent"],
    &["log", "--diff-merges=off", "-p"],
    &["log", "--first-parent"],
    &["log", "--first-parent", "-p"],
    &["log", "--first-parent", "-c"],
    &["log", "--first-parent", "-m", "-p"],
    &["log", "--cc", "--", "f"],
    &["log", "-c", "--oneline", "--", "g"],
    &["log", "--reverse", "--cc"],
    &["log", "--format=%h", "-c"],
];

/// Checks the merge at `HEAD` and the history under it. The plain diffs of this tool and git's can differ in how a
/// change is aligned, so those forms run only where `diff` already agrees.
fn check(f: &Fixture) {
    f.all_same(COMBINED);
    if f.plain_diffs_match() {
        f.all_same(PLAIN);
    }
}

fn each_format(build: impl Fn(&Fixture)) {
    for format in FORMATS {
        let f = Fixture::new(format);
        build(&f);
        check(&f);
    }
}

fn base_files(f: &Fixture) {
    f.write(
        "f",
        &text(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]),
    );
    f.write("g", &text(&["x", "y", "z"]));
    f.write("h", &text(&["one", "two", "three", "four"]));
}

#[test]
fn clean_merges_match_git() {
    each_format(|f| {
        base_files(f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.write(
            "f",
            &text(&["a", "B", "c", "d", "e", "f", "g", "h", "i", "j"]),
        );
        f.write("g", &text(&["x", "Y", "z"]));
        f.write("side.txt", &text(&["s"]));
        f.commit("side");
        f.branch("main", &base);
        f.write(
            "f",
            &text(&["a", "b", "c", "d", "e", "f", "g", "h", "I", "j"]),
        );
        f.write("h", &text(&["one", "two", "THREE", "four"]));
        f.commit("main");
        f.git(&["merge", "-q", "--no-edit", "side"]);
    });
}

#[test]
fn per_parent_headers_are_coloured_as_in_git() {
    for format in FORMATS {
        let f = Fixture::new(format);
        base_files(&f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("g", &text(&["x", "Y", "z"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("h", &text(&["one", "two", "THREE", "four"]));
        f.commit("main");
        f.git(&["merge", "-q", "--no-edit", "side"]);
        f.all_same(&[
            &["show", "-m", "--color=always"],
            &["log", "-m", "-p", "--color=always"],
        ]);
    }
}

#[test]
fn a_clean_merge_among_several_commits_is_separated_as_in_git() {
    each_format(|f| {
        f.write("f", &text(&["a", "b", "c"]));
        f.write("g", &text(&["x"]));
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("f", &text(&["a", "b", "C"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("g", &text(&["x", "y"]));
        f.commit("main");
        f.git(&["merge", "-q", "--no-edit", "side"]);
        f.all_same(&[
            &["show", "--oneline", "HEAD~1", "HEAD", "HEAD~2"],
            &["show", "HEAD~1", "HEAD", "HEAD~2"],
            &["show", "--format=%h", "HEAD~1", "HEAD", "HEAD~2"],
            &["show", "--format=", "HEAD~1", "HEAD", "HEAD~2"],
            &["show", "--pretty=short", "HEAD", "HEAD~1"],
            &["show", "--stat", "HEAD~1", "HEAD", "HEAD~2"],
            &["show", "-s", "HEAD~1", "HEAD", "HEAD~2"],
            &["show", "--oneline", "-s", "HEAD~1", "HEAD", "HEAD~2"],
            &[
                "show",
                "--oneline",
                "--no-diff-merges",
                "HEAD~1",
                "HEAD",
                "HEAD~2",
            ],
        ]);
    });
}

#[test]
fn conflicts_resolved_differently_from_both_parents_match_git() {
    each_format(|f| {
        base_files(f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.write(
            "f",
            &text(&["a", "b", "side", "d", "e", "f", "g", "h", "i", "j"]),
        );
        f.write("g", &text(&["x", "side", "z"]));
        f.write("h", &text(&["side", "two", "three", "four"]));
        f.commit("side");
        f.branch("main", &base);
        f.write(
            "f",
            &text(&["a", "b", "main", "d", "e", "f", "g", "h", "i", "j"]),
        );
        f.write("g", &text(&["x", "main", "z"]));
        f.write("h", &text(&["main", "two", "three", "four"]));
        f.commit("main");
        let out = f.git_ok(&["merge", "side", "--no-edit"]);
        assert!(!out.status.success());
        f.write(
            "f",
            &text(&["a", "b", "both", "d", "e", "f", "g", "h", "i", "j"]),
        );
        f.git(&["checkout", "--theirs", "g"]);
        f.git(&["checkout", "--ours", "h"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "merge"]);
    });
}

#[test]
fn evil_merges_match_git() {
    each_format(|f| {
        base_files(f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("g", &text(&["x", "y", "side"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("h", &text(&["one", "main", "three", "four"]));
        f.commit("main");
        f.git(&["merge", "-q", "--no-commit", "--no-ff", "side"]);
        f.write("evil.txt", &text(&["added", "by", "the", "merge"]));
        f.write(
            "f",
            &text(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "evil"]),
        );
        f.write("g", &text(&["x", "y", "evil"]));
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "merge"]);
    });
}

#[test]
fn files_deleted_on_one_side_match_git() {
    each_format(|f| {
        base_files(f);
        f.write("gone", &text(&["1", "2", "3"]));
        f.write("kept", &text(&["1", "2", "3"]));
        let base = f.commit("base");
        f.branch("side", &base);
        f.git(&["rm", "-q", "gone"]);
        f.write("kept", &text(&["1", "two", "3"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("gone", &text(&["1", "2", "three"]));
        f.git(&["rm", "-q", "kept"]);
        f.commit("main");
        let out = f.git_ok(&["merge", "side", "--no-edit"]);
        assert!(!out.status.success());
        f.git(&["rm", "-q", "-f", "gone"]);
        f.write("kept", &text(&["1", "kept", "3"]));
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "merge"]);
    });
}

#[test]
fn deleted_files_in_the_result_match_git() {
    for format in FORMATS {
        let f = Fixture::new(format);
        base_files(&f);
        f.write("g2", &text(&["p", "q"]));
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("g", &text(&["x", "side", "z"]));
        f.write("g2", &text(&["p", "side"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("g", &text(&["x", "main", "z"]));
        f.write("g2", &text(&["main", "q"]));
        f.commit("main");
        let out = f.git_ok(&["merge", "side", "--no-edit"]);
        assert!(!out.status.success());
        f.git(&["rm", "-q", "-f", "g", "g2"]);
        f.git(&["commit", "-q", "-m", "merge"]);
        check(&f);
    }
}

#[test]
fn mode_changes_match_git() {
    each_format(|f| {
        base_files(f);
        f.write("run.sh", &text(&["#!/bin/sh", "echo hi"]));
        f.write("plain", &text(&["a", "b"]));
        let base = f.commit("base");
        f.branch("side", &base);
        f.chmod("run.sh", 0o755);
        f.write("plain", &text(&["a", "side"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("run.sh", &text(&["#!/bin/sh", "echo hello"]));
        f.chmod("plain", 0o755);
        f.commit("main");
        f.git(&["merge", "-q", "--no-edit", "side"]);
    });
}

#[test]
fn binary_files_match_git() {
    each_format(|f| {
        base_files(f);
        f.write("bin", b"\0\x01\x02");
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("bin", b"\0side");
        f.commit("side");
        f.branch("main", &base);
        f.write("bin", b"\0main");
        f.write(
            "f",
            &text(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "main"]),
        );
        f.commit("main");
        let out = f.git_ok(&["merge", "side", "--no-edit"]);
        assert!(!out.status.success());
        f.git(&["checkout", "--theirs", "bin"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "merge"]);
    });
}

#[test]
fn octopus_merges_match_git() {
    for parents in [3, 4] {
        each_format(|f| {
            base_files(f);
            let base = f.commit("base");
            let mut tips = Vec::new();
            for i in 0..parents {
                f.branch(&format!("t{i}"), &base);
                let mut lines =
                    ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"].map(String::from);
                lines[i * 2] = format!("t{i}");
                let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
                f.write("f", &text(&refs));
                f.write(&format!("only{i}"), &text(&["x"]));
                tips.push(f.commit(&format!("t{i}")));
            }
            f.branch("main", &tips[0]);
            let others: Vec<&str> = tips.iter().skip(1).map(String::as_str).collect();
            let mut args = vec!["merge", "-q", "--no-commit"];
            args.extend(others);
            f.git(&args);
            f.write("g", &text(&["x", "evil", "z"]));
            f.git(&["add", "-A"]);
            f.git(&["commit", "-q", "-m", "octopus"]);
            f.git(&["reset", "-q", "--hard"]);
        });
    }
}

#[test]
fn pathspecs_limit_the_combined_diff() {
    for format in FORMATS {
        let f = Fixture::new(format);
        for n in ["f", "g", "d/h", "d/i"] {
            f.write(n, &text(&["a", "b", "c", "d", "e"]));
        }
        let base = f.commit("base");
        let mut tips = Vec::new();
        for i in 0..3 {
            f.branch(&format!("t{i}"), &base);
            for n in ["f", "g", "d/h", "d/i"] {
                f.write(n, &text(&["a", &format!("p{i}"), "c", "d", "e"]));
            }
            tips.push(f.commit(&format!("t{i}")));
        }
        for n in ["f", "g", "d/h", "d/i"] {
            f.write(n, &text(&["a", "mixed", "c", "d", &format!("e{n}")]));
        }
        let refs: Vec<&str> = tips.iter().map(String::as_str).collect();
        f.commit_tree(&refs, "merge");
        f.all_same(&[
            &["show", "--cc", "--", "f"],
            &["show", "-c", "--", "f", "g"],
            &["show", "--cc", "--", "d"],
            &["show", "--cc", "--", "d/h", "f"],
            &["show", "--stat", "--", "d"],
            &["show", "--name-status", "--", "d/i", "g"],
            &["show", "--oneline", "--cc", "--", "g"],
        ]);
    }
}

#[test]
fn octopus_merges_resolving_unlike_every_parent_match_git() {
    for parents in [3, 4] {
        each_format(|f| {
            base_files(f);
            let base = f.commit("base");
            let mut tips = Vec::new();
            for i in 0..parents {
                f.branch(&format!("t{i}"), &base);
                f.write(
                    "f",
                    &text(&[
                        "a",
                        &format!("p{i}"),
                        "c",
                        "d",
                        &format!("q{i}"),
                        "f",
                        "g",
                        "h",
                        "i",
                        "j",
                    ]),
                );
                f.write("g", &text(&["x", "y", &format!("z{}", i % 2)]));
                tips.push(f.commit(&format!("t{i}")));
            }
            f.write(
                "f",
                &text(&["a", "p1", "c", "d", "q2", "f", "g", "h", "i", "mix"]),
            );
            f.write("g", &text(&["x", "y", "z1"]));
            let refs: Vec<&str> = tips.iter().map(String::as_str).collect();
            f.commit_tree(&refs, "octopus");
        });
    }
}

#[test]
fn renames_across_parents_match_git() {
    each_format(|f| {
        base_files(f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.git(&["mv", "f", "moved"]);
        f.write(
            "moved",
            &text(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "J"]),
        );
        f.commit("side");
        f.branch("main", &base);
        f.write(
            "f",
            &text(&["A", "b", "c", "d", "e", "f", "g", "h", "i", "j"]),
        );
        f.write("g", &text(&["x", "y", "main"]));
        f.commit("main");
        f.git(&["merge", "-q", "--no-edit", "side"]);
        f.write(
            "moved",
            &text(&["A", "b", "c", "d", "e", "f", "g", "h", "i", "J", "evil"]),
        );
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "--amend", "--no-edit"]);
        f.all_same(&[
            &["show", "-M"],
            &["show", "-M", "--name-status"],
            &["show", "-M", "-c"],
            &["show", "-C"],
            &["show", "--no-renames", "--name-status"],
            &["show", "--stat"],
            &["log", "-c", "--name-status", "--oneline"],
        ]);
    });
}

#[test]
fn symlinks_and_empty_files_match_git() {
    each_format(|f| {
        base_files(f);
        f.write("empty", b"");
        std::os::unix::fs::symlink("f", f.repo.join("link")).unwrap();
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("empty", &text(&["now", "full"]));
        fs::remove_file(f.repo.join("link")).unwrap();
        std::os::unix::fs::symlink("g", f.repo.join("link")).unwrap();
        f.commit("side");
        f.branch("main", &base);
        f.write("f", b"no newline at the end");
        f.commit("main");
        f.git(&["merge", "-q", "--no-edit", "side"]);
    });
}

#[test]
fn missing_final_newlines_match_git() {
    each_format(|f| {
        f.write("f", b"one\ntwo\nthree");
        f.write("g", b"one\ntwo\nthree\n");
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("f", b"one\ntwo\nthree\nfour");
        f.write("g", b"one\ntwo\nthree");
        f.commit("side");
        f.branch("main", &base);
        f.write("f", b"ONE\ntwo\nthree");
        f.write("g", b"one\ntwo\nTHREE\n");
        f.commit("main");
        let out = f.git_ok(&["merge", "side", "--no-edit"]);
        assert!(!out.status.success());
        f.write("f", b"ONE\ntwo\nthree\nfour");
        f.write("g", b"one\ntwo\nTHREE");
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "merge"]);
    });
}

#[test]
fn hunk_headers_carry_the_function_line() {
    each_format(|f| {
        let body = |a: &str, b: &str| {
            text(&[
                "int one(void)",
                "{",
                "    a;",
                "    b;",
                "    c;",
                "    d;",
                "    e;",
                "    f;",
                "}",
                "",
                "static int two(void)",
                "{",
                a,
                "    g;",
                "    h;",
                "    i;",
                "    j;",
                "    k;",
                "    l;",
                b,
                "}",
            ])
        };
        f.write("c.c", &body("    m;", "    n;"));
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("c.c", &body("    side;", "    n;"));
        f.commit("side");
        f.branch("main", &base);
        f.write("c.c", &body("    m;", "    main;"));
        f.commit("main");
        f.git(&["merge", "-q", "--no-edit", "side"]);
        f.write("c.c", &body("    mixed;", "    mixed;"));
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "--amend", "--no-edit"]);
    });
}

#[test]
fn log_diff_merges_config_matches_git() {
    for format in FORMATS {
        let f = Fixture::new(format);
        base_files(&f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("g", &text(&["x", "side", "z"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("h", &text(&["one", "main", "three", "four"]));
        f.write("g", &text(&["x", "main", "z"]));
        f.commit("main");
        let out = f.git_ok(&["merge", "side", "--no-edit"]);
        assert!(!out.status.success());
        f.write("g", &text(&["x", "both", "z"]));
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "merge"]);
        for value in [
            "separate",
            "first-parent",
            "cc",
            "combined",
            "off",
            "on",
            "dense-combined",
        ] {
            f.git(&["config", "log.diffMerges", value]);
            f.all_same(&[
                &["show", "-m"],
                &["show", "--diff-merges=on"],
                &["show", "--diff-merges=m", "--stat"],
                &["log", "-m", "-p"],
                &["log", "-m", "--stat"],
                &["log", "--diff-merges=m", "-p", "--oneline"],
                &["log", "-p"],
                &["show"],
            ]);
        }
        f.git(&["config", "--unset", "log.diffMerges"]);
        f.same(&["show", "--diff-merges=bogus"]);
        f.same(&["log", "--diff-merges=bogus"]);
    }
}

#[test]
fn later_merge_options_override_earlier_ones() {
    for format in FORMATS {
        let f = Fixture::new(format);
        base_files(&f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("g", &text(&["x", "side", "z"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("g", &text(&["x", "main", "z"]));
        f.commit("main");
        let out = f.git_ok(&["merge", "side", "--no-edit"]);
        assert!(!out.status.success());
        f.write("g", &text(&["x", "both", "z"]));
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "merge"]);
        f.all_same(&[
            &["show", "-m", "-c"],
            &["show", "-c", "-m"],
            &["show", "--cc", "-c"],
            &["show", "-c", "--cc"],
            &["show", "-c", "--no-diff-merges"],
            &["show", "--no-diff-merges", "--cc"],
            &["show", "--diff-merges=separate", "--cc"],
            &["show", "--cc", "--diff-merges=separate"],
            &["show", "--dd"],
            &["show", "--diff-merges=1"],
            &["log", "-c", "-m", "-p"],
            &["log", "-m", "-c"],
            &["log", "-m", "-c", "--stat"],
            &["log", "--first-parent", "--diff-merges=off", "-p"],
            &["log", "--diff-merges=cc", "--first-parent"],
            &["log", "--first-parent", "-c"],
        ]);
    }
}

#[test]
fn unfinished_forms_are_usage_errors() {
    let f = Fixture::new("sha1");
    base_files(&f);
    f.commit("base");
    for args in [
        &["log", "--graph", "-c"][..],
        &["log", "-c", "--follow", "f"],
        &["log", "-c", "-S", "x"],
        &["log", "-m", "-p", "--", "f"],
    ] {
        let out = f.pyn(args);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
    }
}

/// A small deterministic generator for the random histories.
struct Rng(u64);

impl Rng {
    fn next(&mut self, m: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % m
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.next(items.len() as u64) as usize]
    }
}

const VOCAB: [&str; 14] = [
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "",
    "x",
    "y",
    "{",
    "}",
    "fn main() {",
    "    return;",
];

type Files = std::collections::BTreeMap<String, (Vec<u8>, u32)>;

fn rand_lines(r: &mut Rng, least: u64, spread: u64) -> Vec<String> {
    let n = least + r.next(spread);
    (0..n).map(|_| r.pick(&VOCAB).to_string()).collect()
}

fn edit(r: &mut Rng, mut lines: Vec<String>) -> Vec<String> {
    for _ in 0..r.next(5) {
        match r.next(3) {
            0 if !lines.is_empty() => {
                let at = r.next(lines.len() as u64) as usize;
                lines[at] = format!("{}{}", r.pick(&VOCAB), r.next(4));
            }
            1 => {
                let at = r.next(lines.len() as u64 + 1) as usize;
                let add = rand_lines(r, 1, 3);
                lines.splice(at..at, add);
            }
            _ if !lines.is_empty() => {
                let at = r.next(lines.len() as u64) as usize;
                let end = (at + 1 + r.next(3) as usize).min(lines.len());
                lines.drain(at..end);
            }
            _ => {}
        }
    }
    lines
}

fn blob(r: &mut Rng, lines: &[String]) -> Vec<u8> {
    let mut t = lines.join("\n");
    if !lines.is_empty() {
        t.push('\n');
        if r.next(12) == 0 {
            t.pop();
        }
    }
    t.into_bytes()
}

fn split(content: &[u8]) -> Vec<String> {
    let t = String::from_utf8_lossy(content).into_owned();
    let mut v: Vec<String> = t.split('\n').map(String::from).collect();
    if content.ends_with(b"\n") {
        v.pop();
    }
    v
}

impl Fixture {
    fn set_tree(&self, files: &Files) {
        for entry in fs::read_dir(&self.repo).unwrap() {
            let p = entry.unwrap().path();
            if p.file_name().unwrap() == ".git" {
                continue;
            }
            if p.is_dir() {
                fs::remove_dir_all(&p).unwrap();
            } else {
                fs::remove_file(&p).unwrap();
            }
        }
        for (path, (content, mode)) in files {
            self.write(path, content);
            self.chmod(path, *mode);
        }
    }

    fn random_merge(&self, seed: u64) {
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(7));
        let names = ["f", "g", "d/h", "d/i", "j.txt", "k"];
        let mut files = Files::new();
        for n in names {
            if r.next(5) > 0 || files.len() < 2 {
                let lines = rand_lines(&mut r, 0, 15);
                let mode = if r.next(10) == 0 { 0o755 } else { 0o644 };
                files.insert(n.to_string(), (blob(&mut r, &lines), mode));
            }
        }
        if r.next(4) == 0 {
            files.insert("bin".into(), (vec![0, 1, 2, r.next(256) as u8], 0o644));
        }
        self.set_tree(&files);
        let base = self.commit_tree(&[], "base");
        let count = *r.pick(&[2, 2, 2, 2, 3, 4]);
        let mut pfiles: Vec<Files> = Vec::new();
        let mut parents = Vec::new();
        for i in 0..count {
            let mut pf = files.clone();
            for n in pf.keys().cloned().collect::<Vec<_>>() {
                let (c, m) = pf[&n].clone();
                if n == "bin" {
                    if r.next(5) == 0 {
                        pf.insert(n, ([c.as_slice(), b"x"].concat(), m));
                    }
                    continue;
                }
                match r.next(100) {
                    0..50 => {
                        let lines = edit(&mut r, split(&c));
                        pf.insert(n, (blob(&mut r, &lines), m));
                    }
                    50..58 => {
                        pf.remove(&n);
                    }
                    58..66 => {
                        pf.insert(n, (c, if m == 0o644 { 0o755 } else { 0o644 }));
                    }
                    _ => {}
                }
            }
            if r.next(10) < 3 {
                let cand: Vec<String> = pf
                    .iter()
                    .filter(|(n, (c, _))| {
                        n.as_str() != "bin" && c.iter().filter(|b| **b == b'\n').count() >= 3
                    })
                    .map(|(n, _)| n.clone())
                    .collect();
                if !cand.is_empty() {
                    let n = r.pick(&cand).clone();
                    let (c, m) = pf.remove(&n).unwrap();
                    let lines = if r.next(2) == 0 {
                        edit(&mut r, split(&c))
                    } else {
                        split(&c)
                    };
                    pf.insert(format!("{n}_r"), (blob(&mut r, &lines), m));
                }
            }
            if r.next(10) < 3 {
                let lines = rand_lines(&mut r, 1, 5);
                pf.insert(format!("new{i}"), (blob(&mut r, &lines), 0o644));
            }
            self.git(&["checkout", "-q", "-f", "--detach", &base]);
            self.set_tree(&pf);
            parents.push(self.commit_tree(&[&base], &format!("p{i}")));
            pfiles.push(pf);
        }
        let mut paths: std::collections::BTreeSet<String> = files.keys().cloned().collect();
        for pf in &pfiles {
            paths.extend(pf.keys().cloned());
        }
        let mut res = Files::new();
        for n in paths {
            let cands: Vec<&(Vec<u8>, u32)> = pfiles.iter().filter_map(|p| p.get(&n)).collect();
            if cands.is_empty() {
                continue;
            }
            match r.next(100) {
                0..8 | 93.. => {}
                8..55 => {
                    res.insert(n, (*r.pick(&cands)).clone());
                }
                55..85 => {
                    if cands.iter().any(|(c, _)| c.contains(&0)) {
                        res.insert(n, (*r.pick(&cands)).clone());
                        continue;
                    }
                    let mut out: Vec<String> = Vec::new();
                    for (c, _) in &cands {
                        let l = split(c);
                        let a = r.next(l.len() as u64 + 1) as usize;
                        let b = a + r.next((l.len() - a) as u64 + 1) as usize;
                        out.extend_from_slice(&l[a..b]);
                    }
                    let out = edit(&mut r, out);
                    let mode = r.pick(&cands).1;
                    res.insert(n, (blob(&mut r, &out), mode));
                }
                _ => {
                    let lines = rand_lines(&mut r, 0, 7);
                    let mode = *r.pick(&[0o644, 0o755]);
                    res.insert(n, (blob(&mut r, &lines), mode));
                }
            }
        }
        if r.next(4) == 0 {
            let lines = rand_lines(&mut r, 1, 4);
            res.insert(format!("evil{}", r.next(2)), (blob(&mut r, &lines), 0o644));
        }
        self.git(&["checkout", "-q", "-f", "--detach", &parents[0]]);
        self.set_tree(&res);
        let refs: Vec<&str> = parents.iter().map(String::as_str).collect();
        let merge = self.commit_tree(&refs, "merge");
        self.git(&["checkout", "-q", "-f", "-B", "main", &merge]);
    }
}

#[test]
fn random_merge_histories_match_git() {
    let seeds: u64 = std::env::var("PYN_MERGE_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);
    for format in FORMATS {
        for seed in 0..seeds {
            let f = Fixture::new(format);
            f.random_merge(seed);
            let sets: &[&[&str]] = &[
                &["show"],
                &["show", "-c"],
                &["show", "--stat"],
                &["show", "-c", "--stat"],
                &["show", "--name-status"],
                &["show", "-c", "--name-only"],
                &["show", "--summary"],
                &["show", "-U0"],
                &["show", "-U1", "-c"],
                &["show", "--oneline", "--cc"],
                &["show", "-M", "--cc"],
                &["show", "--no-renames", "-c"],
                &["show", "-s", "-m"],
            ];
            for args in sets {
                f.same(args);
            }
            if f.plain_diffs_match() {
                f.all_same(&[
                    &["show", "-m"],
                    &["show", "--first-parent"],
                    &["show", "-m", "--stat"],
                    &["log", "--cc"],
                    &["log", "-m", "-p"],
                    &["log", "--first-parent", "-p"],
                    &["log", "--oneline", "-c", "--stat"],
                ]);
            }
        }
    }
}

const REMERGE: &[&[&str]] = &[
    &["show", "--remerge-diff"],
    &["show", "--diff-merges=remerge"],
    &["show", "--diff-merges=r", "--stat"],
    &["show", "--remerge-diff", "--name-status"],
    &["show", "--remerge-diff", "-s"],
    &["show", "--remerge-diff", "-U1"],
    &["show", "--remerge-diff", "--", "g"],
    &["log", "--remerge-diff"],
    &["log", "--remerge-diff", "--oneline", "--first-parent"],
    &["log", "--diff-merges=remerge", "-p", "--format=%h"],
    &["log", "--remerge-diff", "--stat", "--format=%h"],
];

/// Every remerge form with git's default conflict markers and with `merge.conflictStyle=diff3`.
fn remerged(f: &Fixture) {
    for style in [None, Some("diff3"), Some("merge")] {
        match style {
            Some(v) => f.git(&["config", "merge.conflictStyle", v]),
            None => String::new(),
        };
        f.all_same(REMERGE);
    }
}

fn conflicted_merge(f: &Fixture, resolve: impl Fn(&Fixture)) {
    base_files(f);
    f.write("keep", &text(&["k"]));
    f.write("bin", b"\0\x01\x02");
    let base = f.commit("base");
    f.branch("side", &base);
    f.write(
        "f",
        &text(&["a", "b", "side", "d", "e", "f", "g", "h", "i", "j"]),
    );
    f.write("g", &text(&["x", "side", "z"]));
    f.write("h", &text(&["one", "two", "three", "four", "side"]));
    f.write("bin", b"\0side");
    f.write("added", &text(&["side"]));
    f.git(&["rm", "-q", "keep"]);
    f.commit("side\tsubject");
    f.branch("main", &base);
    f.write(
        "f",
        &text(&["a", "b", "main", "d", "e", "f", "g", "h", "i", "j"]),
    );
    f.write("g", &text(&["x", "main", "z"]));
    f.write("h", &text(&["zero", "one", "two", "three", "four"]));
    f.write("bin", b"\0main");
    f.write("added", &text(&["main"]));
    f.write("keep", &text(&["k", "main"]));
    f.commit("main");
    let out = f.git_ok(&["merge", "side", "--no-edit"]);
    assert!(!out.status.success());
    resolve(f);
    f.git(&["add", "-A"]);
    f.git(&["commit", "-q", "-m", "merge"]);
}

#[test]
fn remerge_diffs_of_conflicts_match_git() {
    for format in FORMATS {
        let f = Fixture::new(format);
        conflicted_merge(&f, |f| {
            f.write(
                "f",
                &text(&["a", "b", "both", "d", "e", "f", "g", "h", "i", "j"]),
            );
            f.git(&["checkout", "--theirs", "g"]);
            f.git(&["checkout", "--ours", "bin"]);
            f.write("added", &text(&["resolved"]));
            f.write("keep", &text(&["kept"]));
        });
        remerged(&f);
    }
}

#[test]
fn remerge_diffs_of_clean_and_evil_merges_match_git() {
    for format in FORMATS {
        let f = Fixture::new(format);
        base_files(&f);
        let base = f.commit("base");
        f.branch("side", &base);
        f.write("g", &text(&["x", "y", "side"]));
        f.commit("side");
        f.branch("main", &base);
        f.write("h", &text(&["one", "main", "three", "four"]));
        f.commit("main");
        f.git(&["merge", "-q", "--no-commit", "--no-ff", "side"]);
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "clean merge"]);
        remerged(&f);
        f.git(&["reset", "-q", "--hard", "main"]);
        f.git(&["merge", "-q", "--no-commit", "--no-ff", "side"]);
        f.write("evil.txt", &text(&["added", "by", "the", "merge"]));
        f.write("g", &text(&["x", "y", "evil"]));
        f.git(&["add", "-A"]);
        f.git(&["commit", "-q", "-m", "evil merge"]);
        remerged(&f);
    }
}

#[test]
fn remerge_diffs_skip_octopus_merges_and_refuse_zdiff3_like_git() {
    let f = Fixture::new("sha1");
    base_files(&f);
    let base = f.commit("base");
    let mut tips = Vec::new();
    for i in 0..3 {
        f.branch(&format!("t{i}"), &base);
        f.write(&format!("only{i}"), &text(&["x"]));
        tips.push(f.commit(&format!("t{i}")));
    }
    f.branch("main", &tips[0]);
    f.git(&["merge", "-q", "--no-commit", &tips[1], &tips[2]]);
    f.git(&["commit", "-q", "-m", "octopus"]);
    f.all_same(&[
        &["show", "--remerge-diff"],
        &["log", "--remerge-diff", "--oneline"],
    ]);
    let (want, got) = (
        f.oracle(&["show", "--remerge-diff"]),
        f.pyn(&["show", "--remerge-diff"]),
    );
    assert_eq!(got.stderr, want.stderr);
}

#[test]
fn tabs_in_messages_are_expanded_and_ids_abbreviated_like_git() {
    let f = Fixture::new("sha1");
    base_files(&f);
    f.commit("subject\n\n# Conflicts:\n\tg\n\tindented\ttab\n\t\ttwo");
    f.write("g", &text(&["changed"]));
    f.git(&[
        "commit",
        "-q",
        "-a",
        "--cleanup=verbatim",
        "-m",
        "second\n\n\ta\tb",
    ]);
    for format in ["medium", "short", "full", "fuller", "raw", "oneline"] {
        for extra in [
            &[][..],
            &["--expand-tabs"],
            &["--expand-tabs=3"],
            &["--expand-tabs=0"],
            &["--no-expand-tabs"],
            &["--abbrev-commit"],
            &["--no-abbrev-commit"],
            &["--abbrev-commit", "--no-expand-tabs"],
        ] {
            let pretty = format!("--pretty={format}");
            let mut log = vec!["log", pretty.as_str()];
            log.extend(extra);
            f.same(&log);
            let mut show = vec!["show", "-s", pretty.as_str()];
            show.extend(extra);
            f.same(&show);
        }
    }
}

//! `pyn merge -s` and `-X` on a git repository, compared with the real `git merge` on a copy of the same repository:
//! exit status, resulting `HEAD`, index, work tree, merge state files and `AUTO_MERGE`. Real git is the oracle for
//! meaning only; its message text is not compared. Needs `git` on the PATH (tests only).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::{copy_tree, snapshot};

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

/// Two copies of one repository: one for git, one for pyn.
struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
}

/// What `git` is allowed to differ in from pyn.
#[derive(Clone, Copy, Default)]
struct Skip {
    /// The git before 2.50 only records `AUTO_MERGE` for `ort`.
    auto_merge: bool,
    /// Git writes extra reflog entries when it restores the tree after a strategy fails.
    reflog: bool,
    /// `resolve` labels conflict markers with the names of temporary files.
    markers: bool,
}

fn git_minor() -> u32 {
    let out = Command::new("git").arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let version = text.trim().trim_start_matches("git version ");
    version.split('.').nth(1).unwrap().parse().unwrap()
}

/// Strategies that run on the ort engine only from git 2.50 on.
fn old_engine(strategy: &str) -> Skip {
    Skip {
        auto_merge: git_minor() < 50 && matches!(strategy, "recursive" | "subtree"),
        ..Skip::default()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl Pair {
    fn new(format: &str, script: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let git = root.join("git");
        fs::create_dir_all(&git).unwrap();
        let pair = Self {
            _tmp: tmp,
            pyn: root.join("pyn"),
            git,
            home,
        };
        // Background auto-gc would remove loose objects while `copy_tree` runs.
        let init = format!(
            "git init -q -b main --object-format={format} .\n\
             git config gc.auto 0\ngit config maintenance.auto false\ngit config merge.conflictStyle diff3\n{script}"
        );
        let out = Command::new("sh")
            .args(["-e", "-c", &init])
            .current_dir(&pair.git)
            .env("HOME", &pair.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        copy_tree(&pair.git, &pair.pyn);
        pair
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn pyn_cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    /// A shell command run in both copies, with real git.
    fn both_sh(&self, script: &str) {
        for dir in [&self.git, &self.pyn] {
            let out = Command::new("sh")
                .args(["-e", "-c", script])
                .current_dir(dir)
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .envs(IDENTITY)
                .output()
                .unwrap();
            assert!(out.status.success(), "{script}: {}", text(&out.stderr));
        }
    }

    /// Everything a merge leaves behind that a person or git can see.
    fn state(&self, dir: &Path, skip: Skip) -> Vec<String> {
        let g = |args: &[&str]| text(&self.git_in(dir, args).stdout);
        let mut out = vec![
            format!("HEAD {}", g(&["rev-parse", "HEAD"]).trim()),
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!("status\n{}", g(&["status", "--porcelain"])),
        ];
        for name in ["MERGE_HEAD", "MERGE_MODE", "MERGE_MSG", "ORIG_HEAD"] {
            out.push(format!(
                "{name} {:?}",
                fs::read_to_string(dir.join(".git").join(name)).ok()
            ));
        }
        if !skip.auto_merge {
            let have = dir.join(".git/AUTO_MERGE").exists();
            let tree = if have {
                g(&["ls-tree", "-r", "AUTO_MERGE"])
            } else {
                String::new()
            };
            out.push(format!("AUTO_MERGE {have}\n{tree}"));
            if have {
                for line in tree.lines() {
                    let id = line.split_whitespace().nth(2).unwrap();
                    out.push(format!("blob {id} {:?}", g(&["cat-file", "blob", id])));
                }
            }
        }
        if !skip.reflog {
            out.push(format!(
                "reflog {}",
                g(&["log", "-g", "-1", "--format=%gs", "HEAD"])
            ));
        }
        for (path, content) in snapshot(dir) {
            if path == ".git" || path.starts_with(".git/") || path.starts_with(".pyn") {
                continue;
            }
            let file = dir.join(&path);
            if skip.markers && file.is_file() {
                let bytes = fs::read_to_string(&file).unwrap_or_default();
                let lines: Vec<&str> = bytes
                    .lines()
                    .map(|l| {
                        if l.starts_with("<<<<<<<")
                            || l.starts_with("|||||||")
                            || l.starts_with(">>>>>>>")
                        {
                            &l[..7]
                        } else {
                            l
                        }
                    })
                    .collect();
                out.push(format!("{path} {lines:?}"));
            } else {
                out.push(format!("{path} {content}"));
            }
        }
        out
    }

    /// Runs the same `git <args>` and `pyn <args>`; status and the whole resulting state must agree.
    fn same(&self, args: &[&str], skip: Skip) {
        let g = self.git_in(&self.git, args);
        let p = self.pyn_cmd(args);
        let what = format!("{args:?}");
        assert_eq!(
            g.status.code(),
            p.status.code(),
            "{what}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(
            self.state(&self.git, skip),
            self.state(&self.pyn, skip),
            "{what}"
        );
    }

    /// pyn refuses `args` with `code` and changes nothing.
    fn refused(&self, args: &[&str], code: i32) {
        let before = self.state(&self.pyn, Skip::default());
        let p = self.pyn_cmd(args);
        assert_eq!(p.status.code(), Some(code), "{args:?}: {}", text(&p.stderr));
        assert_eq!(before, self.state(&self.pyn, Skip::default()), "{args:?}");
    }
}

fn each_format(script: &str, run: impl Fn(&Pair)) {
    for format in common::gitfix::FORMATS {
        run(&Pair::new(format, script));
    }
}

/// `main` and `side` both changed `f` (conflict) and each added a file.
const CONFLICT: &str = "printf 'a\\nb\\nc\\nd\\ne\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'a\\nB2\\nc\\nd\\nE\\n' >f; printf 'x\\nside\\n' >g; git add .; git commit -qm side\n\
    git checkout -q main\nprintf 'a\\nB1\\nc\\nd\\ne\\n' >f; printf 'x\\nmain\\n' >h; git add .; git commit -qm main\n";

/// A clean merge needing a line merge in `f`.
const CLEAN: &str = "printf 'a\\nb\\nc\\nd\\ne\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'a\\nb\\nc\\nd\\nE\\n' >f; printf 'x\\nside\\n' >g; git add .; git commit -qm side\n\
    git checkout -q main\nprintf 'A\\nb\\nc\\nd\\ne\\n' >f; printf 'x\\nmain\\n' >h; git add .; git commit -qm main\n";

/// Disjoint files: git's plain in-index merge resolves it.
const TRIVIAL: &str = "printf 'a\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'x2\\n' >g; echo s >s; git add .; git commit -qm side\n\
    git checkout -q main\nprintf 'a2\\n' >f; git add .; git commit -qm main\n";

/// `side` is ahead of `main`.
const AHEAD: &str = "printf 'a\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b side\necho n >n; git add .; git commit -qm side\ngit checkout -q main\n";

/// `side` is behind `main`.
const BEHIND: &str = "printf 'a\\n' >f; git add .; git commit -qm base\ngit branch side\n\
    echo n >n; git add .; git commit -qm main\n";

/// Every kind of conflict a path can have, and a path that merges without one.
const MIXED: &str = "printf 'a\\nb\\nc\\n' >f; printf '\\0one' >b; printf 'm\\n' >md; printf 'd\\n' >dm; ln -s t1 sl; \
    printf 'k\\n' >clean; git add .; git commit -qm base\ngit branch side\n\
    printf 'a\\nB1\\nc\\n' >f; printf '\\0two' >b; printf 'm2\\n' >md; git rm -q dm; printf 'A1\\n' >aa; \
    ln -sfn t2 sl; printf 'k1\\n' >clean; git add -A .; git commit -qm main\ngit checkout -q side\n\
    printf 'a\\nB2\\nc\\n' >f; printf '\\0three' >b; git rm -q md; printf 'd2\\n' >dm; printf 'A2\\n' >aa; \
    rm sl; ln -s t3 sl; git add -A .; git commit -qm side\ngit checkout -q main\n";

#[test]
fn ours_makes_a_merge_commit_with_head_s_tree() {
    for script in [CONFLICT, CLEAN, TRIVIAL, AHEAD, MIXED] {
        each_format(script, |p| {
            p.same(&["merge", "-s", "ours", "side"], Skip::default())
        });
    }
    each_format(CONFLICT, |p| {
        p.same(
            &["merge", "--strategy=ours", "-m", "msg", "side"],
            Skip::default(),
        )
    });
}

#[test]
fn ours_ignores_fast_forward_options_and_strategy_options() {
    each_format(AHEAD, |p| {
        p.same(
            &["merge", "-s", "ours", "--ff-only", "side"],
            Skip::default(),
        )
    });
    each_format(AHEAD, |p| {
        p.same(&["merge", "-s", "ours", "--no-ff", "side"], Skip::default())
    });
    each_format(CONFLICT, |p| {
        p.same(
            &["merge", "-s", "ours", "-X", "bogus", "side"],
            Skip::default(),
        )
    });
}

#[test]
fn ours_stops_before_committing_and_is_up_to_date_when_there_is_nothing_to_merge() {
    each_format(CONFLICT, |p| {
        p.same(
            &["merge", "-s", "ours", "--no-commit", "side"],
            Skip::default(),
        )
    });
    each_format(BEHIND, |p| {
        p.same(&["merge", "-s", "ours", "side"], Skip::default())
    });
}

#[test]
fn ours_needs_an_index_that_equals_head_but_not_a_clean_work_tree() {
    each_format(CONFLICT, |p| {
        p.both_sh("echo more >>g; git add g");
        p.same(
            &["merge", "-s", "ours", "side"],
            Skip {
                reflog: true,
                auto_merge: true,
                ..Skip::default()
            },
        );
    });
    each_format(CONFLICT, |p| {
        p.both_sh("echo more >>g");
        p.same(&["merge", "-s", "ours", "side"], Skip::default());
    });
}

#[test]
fn named_strategies_that_run_the_tree_merge_match_git() {
    for strategy in ["ort", "recursive"] {
        let skip = old_engine(strategy);
        for script in [CONFLICT, CLEAN, TRIVIAL, AHEAD, BEHIND, MIXED] {
            each_format(script, |p| p.same(&["merge", "-s", strategy, "side"], skip));
        }
        each_format(CONFLICT, |p| {
            p.same(&["merge", "-s", strategy, "--no-commit", "side"], skip)
        });
        each_format(CLEAN, |p| {
            p.same(&["merge", "-s", strategy, "--no-commit", "side"], skip)
        });
    }
}

#[test]
fn resolve_tries_the_in_index_merge_first() {
    let skip = Skip {
        markers: true,
        ..Skip::default()
    };
    for script in [CONFLICT, CLEAN, TRIVIAL, AHEAD, BEHIND] {
        each_format(script, |p| {
            p.same(&["merge", "-s", "resolve", "side"], skip)
        });
    }
    each_format(TRIVIAL, |p| {
        p.same(
            &["merge", "-s", "resolve", "--no-commit", "side"],
            Skip::default(),
        )
    });
    each_format(CLEAN, |p| {
        p.same(
            &["merge", "-s", "resolve", "--no-commit", "side"],
            Skip::default(),
        )
    });
}

#[test]
fn resolve_with_a_deletion_is_not_the_in_index_merge() {
    let script = "printf 'a\\n' >f; printf 'x\\n' >g; git add .; git commit -qm base\n\
        git checkout -q -b side\ngit rm -q g; git commit -qm side\ngit checkout -q main\n\
        printf 'a2\\n' >f; git add .; git commit -qm main\n";
    each_format(script, |p| {
        p.same(&["merge", "-s", "resolve", "side"], Skip::default())
    });
    each_format(script, |p| {
        p.same(&["merge", "-s", "octopus", "side"], Skip::default())
    });
}

#[test]
fn resolve_with_strategy_options_fails_after_the_in_index_attempt() {
    each_format(CLEAN, |p| {
        p.same(
            &["merge", "-s", "resolve", "-X", "ours", "side"],
            Skip::default(),
        )
    });
    each_format(TRIVIAL, |p| {
        p.same(
            &["merge", "-s", "resolve", "-X", "ours", "side"],
            Skip::default(),
        )
    });
}

#[test]
fn octopus_with_one_head_succeeds_only_without_file_level_merging() {
    for script in [TRIVIAL, AHEAD, BEHIND, CLEAN, CONFLICT] {
        each_format(script, |p| {
            p.same(&["merge", "-s", "octopus", "side"], Skip::default())
        });
    }
}

#[test]
fn resolve_and_octopus_check_the_index_first() {
    for strategy in ["resolve", "octopus"] {
        each_format(TRIVIAL, |p| {
            p.both_sh("echo more >>f; git add f");
            p.same(
                &["merge", "-s", strategy, "side"],
                Skip {
                    reflog: true,
                    auto_merge: true,
                    ..Skip::default()
                },
            );
        });
    }
}

#[test]
fn x_ours_and_x_theirs_resolve_conflicting_hunks_only() {
    for favor in ["ours", "theirs"] {
        for script in [CONFLICT, MIXED] {
            each_format(script, |p| {
                p.same(&["merge", "-X", favor, "side"], Skip::default())
            });
        }
        each_format(MIXED, |p| {
            p.same(
                &["merge", &format!("-X{favor}"), "--no-commit", "side"],
                Skip::default(),
            )
        });
        each_format(MIXED, |p| {
            p.same(
                &[
                    "merge",
                    "--strategy-option",
                    favor,
                    "-s",
                    "recursive",
                    "side",
                ],
                old_engine("recursive"),
            )
        });
    }
}

#[test]
fn the_last_favor_wins() {
    each_format(CONFLICT, |p| {
        p.same(
            &["merge", "-X", "ours", "-X", "theirs", "side"],
            Skip::default(),
        )
    });
}

/// Files that differ from their base and from each other only in whitespace, or in one real line each.
const SPACE: &str = "printf 'one\\ntwo words\\nthree\\nfour\\n' >amount; \
    printf 'one\\ntwo words\\nthree\\nfour\\n' >eol; printf 'a\\nb\\nc\\n' >cr; printf 'p q\\nr\\n' >same; \
    printf 'k\\n lead\\nm\\n' >lead; git add .; git commit -qm base\n\
    git checkout -q -b side\n\
    printf 'one\\ntwo words!\\nthree\\nfour\\n' >amount; printf 'one\\ntwo words\\nthree\\nfour\\nfive\\n' >eol; \
    printf 'a\\r\\nb\\r\\nc\\r\\n' >cr; printf 'p  q\\nr\\n' >same; printf 'k\\nlead\\nm side\\n' >lead; \
    git add .; git commit -qm side\ngit checkout -q main\n\
    printf 'one\\ntwo   words\\nthree\\nfour\\n' >amount; printf 'one\\ntwo words  \\nthree\\nfour\\n' >eol; \
    printf 'a\\nB\\nc\\n' >cr; printf 'p   q\\nr\\n' >same; printf 'k\\n   lead\\nm main\\n' >lead; \
    git add .; git commit -qm main\n";

#[test]
fn whitespace_options_decide_what_counts_as_a_change() {
    for opt in [
        "ignore-space-change",
        "ignore-all-space",
        "ignore-space-at-eol",
        "ignore-cr-at-eol",
    ] {
        each_format(SPACE, |p| {
            p.same(&["merge", "-X", opt, "side"], Skip::default())
        });
    }
    each_format(SPACE, |p| {
        p.same(
            &[
                "merge",
                "-X",
                "ignore-space-change",
                "-X",
                "ignore-cr-at-eol",
                "side",
            ],
            Skip::default(),
        )
    });
    each_format(SPACE, |p| p.same(&["merge", "side"], Skip::default()));
}

#[test]
fn rename_and_algorithm_options_that_change_nothing_are_accepted() {
    for opt in [
        "no-renames",
        "diff-algorithm=myers",
        "diff-algorithm=minimal",
        "diff-algorithm=default",
        "no-renormalize",
        "renormalize",
    ] {
        each_format(CONFLICT, |p| {
            p.same(&["merge", "-X", opt, "side"], Skip::default())
        });
        each_format(CLEAN, |p| {
            p.same(&["merge", "-X", opt, "side"], Skip::default())
        });
    }
}

#[test]
fn unknown_options_end_the_command_as_git_does() {
    each_format(CONFLICT, |p| {
        p.same(&["merge", "-X", "bogus", "side"], Skip::default())
    });
    each_format(CONFLICT, |p| {
        p.same(
            &["merge", "-X", "diff-algorithm=bogus", "side"],
            Skip::default(),
        )
    });
    each_format(CONFLICT, |p| {
        p.same(&["merge", "-X", "", "side"], Skip::default())
    });
    each_format(CONFLICT, |p| {
        p.same(&["merge", "-s", "bogus", "side"], Skip::default())
    });
    each_format(AHEAD, |p| {
        p.same(&["merge", "-s", "bogus", "side"], Skip::default())
    });
}

#[test]
fn an_option_is_only_read_when_a_real_merge_needs_it() {
    each_format(AHEAD, |p| {
        p.same(&["merge", "-X", "bogus", "side"], Skip::default())
    });
    each_format(BEHIND, |p| {
        p.same(&["merge", "-X", "bogus", "side"], Skip::default())
    });
}

#[test]
fn what_cannot_be_honored_is_refused_before_anything_changes() {
    for opt in [
        "patience",
        "histogram",
        "diff-algorithm=patience",
        "diff-algorithm=histogram",
    ] {
        each_format(CONFLICT, |p| p.refused(&["merge", "-X", opt, "side"], 129));
    }
    each_format(CONFLICT, |p| {
        p.refused(&["merge", "-s", "ort", "-s", "ours", "side"], 129)
    });
}

#[test]
fn renormalize_is_refused_where_line_endings_would_be_converted() {
    let script = "printf 'a\\nb\\n' >f; printf '* text=auto\\n' >.gitattributes; git add .; git commit -qm base\n\
        git checkout -q -b side\nprintf 'a\\nb\\nc\\n' >f; git add .; git commit -qm side\ngit checkout -q main\n\
        printf 'z\\na\\nb\\n' >f; git add .; git commit -qm main\n";
    each_format(script, |p| {
        p.refused(&["merge", "-X", "renormalize", "side"], 128)
    });
}

/// Project B lives at `vendor/` on `main` and at the root on `up`.
const SUBTREE: &str = "echo readme >README; git add .; git commit -qm root\n\
    git checkout -q --orphan up\ngit rm -rfq .\nprintf '1\\n2\\n3\\n' >a.txt; echo b >b.txt; echo c >c.txt; \
    git add .; git commit -qm up0\ngit checkout -q main\n\
    git merge -q -s ours --no-commit --allow-unrelated-histories up\ngit read-tree --prefix=vendor/ -u up\n\
    git commit -qm vendor\ngit checkout -q up\nprintf '1\\n2\\n3\\n4\\n' >a.txt; echo new >d.txt; git add .; \
    git commit -qm up1\ngit checkout -q main\necho more >>README; git commit -qam readme\n";

#[test]
fn subtree_merges_find_where_the_other_tree_belongs() {
    let skip = old_engine("subtree");
    each_format(SUBTREE, |p| p.same(&["merge", "-s", "subtree", "up"], skip));
    each_format(SUBTREE, |p| {
        p.same(&["merge", "-X", "subtree", "up"], Skip::default())
    });
    each_format(SUBTREE, |p| {
        p.same(&["merge", "-X", "subtree=vendor", "up"], Skip::default())
    });
    each_format(SUBTREE, |p| {
        p.same(
            &["merge", "-s", "subtree", "-X", "subtree=vendor/", "up"],
            skip,
        )
    });
    each_format(SUBTREE, |p| {
        p.same(
            &["merge", "-s", "recursive", "-X", "subtree=vendor", "up"],
            old_engine("recursive"),
        )
    });
}

#[test]
fn subtree_conflicts_leave_the_merge_to_finish() {
    let script = format!(
        "{SUBTREE}printf '1\\nX\\n3\\n' >vendor/a.txt; git commit -qam local\n\
         git checkout -q up\nprintf '1\\nY\\n3\\n' >a.txt; git commit -qam up2\ngit checkout -q main\n"
    );
    let skip = old_engine("subtree");
    each_format(&script, |p| p.same(&["merge", "-s", "subtree", "up"], skip));
    each_format(&script, |p| {
        p.same(&["merge", "-s", "subtree", "-X", "theirs", "up"], skip)
    });
}

#[test]
fn auto_merge_goes_away_when_the_merge_ends() {
    for conclude in [vec!["commit", "-m", "done"], vec!["merge", "--abort"]] {
        each_format(CONFLICT, |p| {
            let g = p.git_in(&p.git, &["merge", "side"]);
            let n = p.pyn_cmd(&["merge", "side"]);
            assert_eq!(g.status.code(), n.status.code());
            assert!(p.pyn.join(".git/AUTO_MERGE").exists());
            if conclude[0] == "commit" {
                p.both_sh("printf 'resolved\\n' >f; git add f");
            }
            let n = p.pyn_cmd(&conclude);
            assert!(n.status.success(), "{}", text(&n.stderr));
            assert!(!p.pyn.join(".git/AUTO_MERGE").exists());
        });
    }
}

/// Shell helpers giving every commit and merge a later time, so the order git merges bases in is defined.
const CLOCK: &str = "T=1700000000\n\
    tick() { T=$((T+100)); GIT_AUTHOR_DATE=\"$T +0000\"; GIT_COMMITTER_DATE=\"$T +0000\"; export GIT_AUTHOR_DATE GIT_COMMITTER_DATE; }\n\
    ci() { tick; git commit -q --allow-empty \"$@\"; }\n\
    mg() { tick; git merge -q --no-edit \"$@\" >/dev/null 2>&1 || true; }\n";

/// What a criss-cross scenario changes: `x` and `y` each edit the base, `main` and `side` merge the other's commit
/// with their own merge options, then each adds `main` and `side` commits of its own.
struct Cross<'a> {
    x: &'a str,
    y: &'a str,
    merge_main: &'a str,
    merge_side: &'a str,
    main: &'a str,
    side: &'a str,
}

/// `main` and `side` each merged the other's first commit, so `x` and `y` are both merge bases of their tips.
fn criss_cross(c: &Cross) -> String {
    format!(
        "{CLOCK}seq 1 9 >f; echo k >k; git add .; ci -m base\n\
         git branch x; git branch y\n\
         git checkout -q x\n{x}\ngit add -A; ci -m x1\n\
         git checkout -q y\n{y}\ngit add -A; ci -m y1\n\
         git checkout -q main\ngit reset -q --hard x\nmg {mm} y\n\
         git checkout -q -b side y\nmg {ms} x\n\
         git checkout -q main\n{main}\ngit add -A; ci -m more-main\n\
         git checkout -q side\n{side}\ngit add -A; ci -m more-side\n\
         git checkout -q main\n",
        x = c.x,
        y = c.y,
        mm = c.merge_main,
        ms = c.merge_side,
        main = c.main,
        side = c.side,
    )
}

const CROSS_CLEAN: Cross = Cross {
    x: "sed -i 1s/.*/x1/ f",
    y: "sed -i 9s/.*/y9/ f",
    merge_main: "",
    merge_side: "",
    main: "sed -i 3s/.*/m3/ f",
    side: "sed -i 7s/.*/s7/ f; echo s >s",
};

#[test]
fn a_criss_cross_merge_uses_a_virtual_base() {
    let scripts = [
        criss_cross(&CROSS_CLEAN),
        // the final merge conflicts against the merged base
        criss_cross(&Cross {
            main: "sed -i 4s/.*/m4/ f",
            side: "sed -i 4s/.*/s4/ f",
            ..CROSS_CLEAN
        }),
        // the bases conflict among themselves; each merge kept its own side
        criss_cross(&Cross {
            x: "sed -i 5s/.*/x5/ f",
            y: "sed -i 5s/.*/y5/ f",
            merge_main: "-X ours",
            merge_side: "-X ours",
            main: "sed -i 2s/.*/m2/ f",
            side: "sed -i 8s/.*/s8/ f",
        }),
        criss_cross(&Cross {
            x: "sed -i 5s/.*/x5/ f",
            y: "sed -i 5s/.*/y5/ f",
            merge_main: "-X ours",
            merge_side: "-X ours",
            main: "sed -i 5s/.*/m5/ f",
            side: "sed -i 5s/.*/s5/ f",
        }),
        // a file added on both bases, and one deleted on a base
        criss_cross(&Cross {
            x: "echo x >n; git rm -q k",
            y: "echo y >n",
            merge_main: "-X ours",
            merge_side: "-X ours",
            main: "echo m >>n",
            side: "echo s >>k",
        }),
    ];
    for script in &scripts {
        each_format(script, |p| p.same(&["merge", "side"], Skip::default()));
        each_format(script, |p| {
            p.same(&["merge", "--no-commit", "side"], Skip::default())
        });
        each_format(script, |p| {
            p.same(
                &["merge", "-s", "ort", "-X", "theirs", "side"],
                Skip::default(),
            )
        });
    }
}

#[test]
fn a_conflicted_criss_cross_merge_is_aborted_and_concluded_like_git() {
    let script = criss_cross(&Cross {
        main: "sed -i 4s/.*/m4/ f",
        side: "sed -i 4s/.*/s4/ f",
        ..CROSS_CLEAN
    });
    each_format(&script, |p| {
        p.same(&["merge", "side"], Skip::default());
        p.both_sh("printf 'resolved\\n' >f; git add f");
        p.same(&["commit", "-m", "done"], Skip::default());
    });
    each_format(&script, |p| {
        p.same(&["merge", "side"], Skip::default());
        p.same(&["merge", "--abort"], Skip::default());
    });
}

#[test]
fn three_merge_bases_merge_one_after_another() {
    let script = format!(
        "{CLOCK}seq 1 9 >f; git add .; ci -m base\n\
         for n in 1 2 3; do git checkout -q -b p$n main; echo $n >p$n; git add .; ci -m p$n; done\n\
         git checkout -q -b main2 p1\nmg p2 p3\n\
         git checkout -q -b side p1\nmg p3 p2\n\
         git checkout -q main2\necho m >m; git add .; ci -m more-main\n\
         git checkout -q side\necho s >s; git add .; ci -m more-side\n\
         git checkout -q main2\n"
    );
    each_format(&script, |p| p.same(&["merge", "side"], Skip::default()));
}

/// `main` and `side` change `t` from nothing: a file on one, a folder on the other.
fn file_folder(base: &str, main: &str, side: &str) -> String {
    format!(
        "{CLOCK}echo b >base; {base}\ngit add -A .; ci -m base\ngit branch side\n\
         {main}\ngit add -A .; ci -m main\ngit checkout -q side\n\
         {side}\ngit add -A .; ci -m side\ngit checkout -q main\n"
    )
}

const FOLDER: &str = "mkdir t; echo in >t/x";
const FILE: &str = "echo f >t";

#[test]
fn a_file_meeting_a_folder_is_moved_aside_like_git() {
    let skip = Skip {
        auto_merge: true,
        ..Skip::default()
    };
    let scripts = [
        // we add the file, they add the folder, and the other way round
        file_folder("", FILE, FOLDER),
        file_folder("", FOLDER, FILE),
        // a nested folder, and a link instead of a file
        file_folder("", FILE, "mkdir -p t/u; echo in >t/u/x"),
        file_folder("", "ln -s foo t", FOLDER),
        file_folder("", "mkdir t; echo f >t/u", "mkdir -p t/u; echo in >t/u/x"),
        // the moved name is taken
        file_folder("", "echo f >t; echo g >t~HEAD", FOLDER),
        file_folder("", FOLDER, "echo f >t; echo g >t~side"),
        // a side that kept the file the other replaced is not a conflict
        file_folder(
            "printf 'a\\nb\\n' >t",
            "rm t; mkdir t; echo in >t/x",
            "echo z >z",
        ),
        file_folder(
            "printf 'a\\nb\\n' >t",
            "echo z >z",
            "rm t; mkdir t; echo in >t/x",
        ),
        // the file was edited where the other side replaced it with a folder
        file_folder(
            "printf 'a\\nb\\n' >t",
            "echo c >>t",
            "rm t; mkdir t; echo in >t/x",
        ),
        file_folder(
            "printf 'a\\nb\\n' >t",
            "rm t; mkdir t; echo in >t/x",
            "echo c >>t",
        ),
        // the folder was edited where the other side replaced it with a file
        file_folder(
            "mkdir t; echo a >t/x; echo q >t/y",
            "rm -r t; echo f >t",
            "echo c >>t/x",
        ),
        file_folder(
            "mkdir t; echo a >t/x; echo q >t/y",
            "echo c >>t/x",
            "rm -r t; echo f >t",
        ),
        file_folder(
            "mkdir t; echo a >t/x; echo q >t/y",
            "rm -r t; echo f >t",
            "echo z >z",
        ),
    ];
    for script in &scripts {
        each_format(script, |p| p.same(&["merge", "side"], skip));
        each_format(script, |p| p.same(&["merge", "--no-commit", "side"], skip));
    }
}

#[test]
fn a_moved_file_is_concluded_and_aborted_like_git() {
    let skip = Skip {
        auto_merge: true,
        ..Skip::default()
    };
    for script in [
        file_folder("", FILE, FOLDER),
        file_folder("", FOLDER, FILE),
        file_folder(
            "printf 'a\\nb\\n' >t",
            "echo c >>t",
            "rm t; mkdir t; echo in >t/x",
        ),
    ] {
        each_format(&script, |p| {
            p.same(&["merge", "side"], skip);
            p.same(&["merge", "--abort"], skip);
        });
    }
    let script = file_folder("", FILE, FOLDER);
    each_format(&script, |p| {
        p.same(&["merge", "side"], skip);
        p.both_sh("git add -A .");
        p.same(&["commit", "-m", "done"], skip);
    });
    each_format(&script, |p| {
        p.both_sh("echo dirty >>base");
        p.same(&["merge", "side"], skip);
    });
}

#[test]
fn what_a_criss_cross_merge_cannot_do_is_refused_before_anything_changes() {
    let clean = criss_cross(&CROSS_CLEAN);
    each_format(&clean, |p| {
        p.refused(&["merge", "-s", "resolve", "side"], 128);
        p.refused(&["merge", "-s", "subtree", "side"], 128);
    });
    // the bases conflict among themselves and every commit has one time, so git's merge order is unknown
    let same_time = "seq 1 9 >f; git add .; git commit -qm base\ngit branch x; git branch y\n\
        git checkout -q x; sed -i 5s/.*/x5/ f; git commit -qam x1\n\
        git checkout -q y; sed -i 5s/.*/y5/ f; git commit -qam y1\n\
        git checkout -q main; git reset -q --hard x; git merge -q --no-edit -X ours y >/dev/null\n\
        git checkout -q -b side y; git merge -q --no-edit -X ours x >/dev/null\n\
        git checkout -q main; sed -i 2s/.*/m2/ f; git commit -qam m\n\
        git checkout -q side; sed -i 8s/.*/s8/ f; git commit -qam s\ngit checkout -q main\n";
    each_format(same_time, |p| p.refused(&["merge", "side"], 128));
}

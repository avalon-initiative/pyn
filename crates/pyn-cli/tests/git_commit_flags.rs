//! `pyn commit -F`, `--allow-empty`, `--author`, `--date`, `-s` and `--amend` on a git repository compared with the
//! real `git commit` on an identical repository: the commit object, reflogs, refs, index and exit statuses, never
//! git's wording. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const BASE_ENV: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "Ann Author"),
    ("GIT_AUTHOR_EMAIL", "ann@example.com"),
    ("GIT_COMMITTER_NAME", "Cy Committer"),
    ("GIT_COMMITTER_EMAIL", "cy@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0100"),
    ("GIT_COMMITTER_DATE", "1700000100 -0200"),
];

/// Setup steps run with real git in both repositories before the command under test.
#[derive(Clone, Copy)]
enum Step {
    Write(&'static str, &'static str),
    Git(&'static [&'static str]),
}
use Step::{Git, Write as W};

struct Case {
    name: &'static str,
    setup: Vec<Step>,
    args: Vec<String>,
    stdin: Option<&'static str>,
}

fn case(name: &'static str, setup: &[Step], args: &[&str], stdin: Option<&'static str>) -> Case {
    Case {
        name,
        setup: setup.to_vec(),
        args: args.iter().map(|a| a.to_string()).collect(),
        stdin,
    }
}

/// A history of two commits on `main`, a side branch, and a staged change to `b.txt`.
const BASE: &[Step] = &[
    W("a.txt", "one\n"),
    Git(&["add", "."]),
    Git(&["commit", "-q", "-m", "one"]),
    W("b.txt", "two\n"),
    Git(&["add", "."]),
    Git(&["commit", "-q", "-m", "two\n\nbody text"]),
    Git(&["branch", "side"]),
];

struct Pair {
    _tmp: TempDir,
    root: PathBuf,
    oracle: PathBuf,
    ours: PathBuf,
}

fn dir_env(c: &mut Command, root: &Path) {
    c.env("HOME", root.join("home"))
        .env("TZ", "UTC")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true");
    for (k, v) in BASE_ENV {
        c.env(k, v);
    }
}

impl Pair {
    fn new(format: &str, setup: &[&Step]) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let (oracle, ours) = (root.join("oracle"), root.join("ours"));
        for dir in [&oracle, &ours] {
            init(dir, format, false);
            fs::write(dir.join(".gitignore"), "").unwrap();
            for step in setup {
                match step {
                    Step::Write(p, t) => fs::write(dir.join(p), t).unwrap(),
                    Step::Git(args) => {
                        let mut c = git(dir);
                        dir_env(&mut c, &root);
                        let out = c.args(*args).output().unwrap();
                        let ok = out.status.success() || args.first() == Some(&"merge");
                        assert!(ok, "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
                    }
                }
            }
        }
        Self {
            _tmp: tmp,
            root,
            oracle,
            ours,
        }
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> String {
        let mut c = git(dir);
        dir_env(&mut c, &self.root);
        let out = c.args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn command(&self, pyn: bool, args: &[String], stdin: Option<&str>) -> i32 {
        let dir = if pyn { &self.ours } else { &self.oracle };
        let mut c = if pyn {
            let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
            c.env_clear().env("PATH", path_env()).current_dir(dir);
            c.arg("commit");
            c
        } else {
            let mut c = git(dir);
            c.arg("commit");
            c
        };
        dir_env(&mut c, &self.root);
        c.env("GIT_COMMITTER_DATE", "1700009999 +0000")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = c.spawn().unwrap();
        let mut pipe = child.stdin.take().unwrap();
        let _ = pipe.write_all(stdin.unwrap_or("").as_bytes());
        drop(pipe);
        child.wait().unwrap().code().unwrap()
    }

    /// Everything a commit may change, as text.
    fn state(&self, dir: &Path) -> String {
        let mut out = String::new();
        for args in [
            &["cat-file", "-p", "HEAD"][..],
            &["for-each-ref", "--format=%(refname) %(objectname)"],
            &["ls-files", "-s"],
            &["reflog", "show", "--format=%gs", "HEAD"],
            &["reflog", "show", "--format=%gs", "refs/heads/main"],
            &["status", "--porcelain"],
        ] {
            out += &format!("== {args:?}\n{}", self.git_in(dir, args));
        }
        out
    }

    /// Both tools must exit alike and leave the same repository; a success must also pass `git fsck`.
    fn same(&self, case: &Case, label: &str) {
        let want = self.command(false, &case.args, case.stdin);
        let got = self.command(true, &case.args, case.stdin);
        assert_eq!(got, want, "{label}: exit status of commit {:?}", case.args);
        assert_eq!(
            self.state(&self.ours),
            self.state(&self.oracle),
            "{label}: repository after commit {:?}",
            case.args
        );
        if got == 0 {
            fsck(&self.ours);
        }
    }
}

fn run_cases(cases: &[Case]) {
    for format in FORMATS {
        for case in cases {
            let setup: Vec<&Step> = BASE.iter().chain(&case.setup).collect();
            let pair = Pair::new(format, &setup);
            pair.same(case, &format!("{format} {}", case.name));
        }
    }
}

const STAGE_B: &[Step] = &[W("b.txt", "three\n"), Git(&["add", "b.txt"])];

#[test]
fn message_files_and_standard_input_give_the_message() {
    run_cases(&[
        case(
            "file",
            &[
                W("m.txt", "from file\n\nbody  \n\n\n"),
                STAGE_B[0],
                STAGE_B[1],
            ],
            &["-F", "m.txt"],
            None,
        ),
        case(
            "long file flag",
            &[W("m.txt", "  \nfrom file\n"), STAGE_B[0], STAGE_B[1]],
            &["--file=m.txt"],
            None,
        ),
        case("stdin", STAGE_B, &["-F", "-"], Some("from stdin\n\nmore\n")),
        case(
            "all with file",
            &[W("m.txt", "all\n"), W("a.txt", "changed\n")],
            &["-a", "-F", "m.txt"],
            None,
        ),
    ]);
}

#[test]
fn message_errors_match_git() {
    run_cases(&[
        case(
            "m with F",
            &[W("m.txt", "x\n"), STAGE_B[0], STAGE_B[1]],
            &["-m", "x", "-F", "m.txt"],
            None,
        ),
        case("missing file", STAGE_B, &["-F", "nope.txt"], None),
        case(
            "empty file",
            &[W("m.txt", "\n  \n"), STAGE_B[0], STAGE_B[1]],
            &["-F", "m.txt"],
            None,
        ),
        case("nothing staged", &[], &["-m", "x"], None),
    ]);
}

#[test]
fn allow_empty_commits_without_changes() {
    run_cases(&[
        case("empty", &[], &["--allow-empty", "-m", "empty one"], None),
        case(
            "empty with file",
            &[W("m.txt", "from file\n")],
            &["--allow-empty", "-F", "m.txt"],
            None,
        ),
        case(
            "allow-empty with changes",
            STAGE_B,
            &["--allow-empty", "-m", "with change"],
            None,
        ),
    ]);
}

#[test]
fn author_and_date_go_to_the_author_line_only() {
    let cases = [
        ("--author=Zed Zee <zed@example.org>", "-m"),
        ("--date=1710000000 +0530", "-m"),
        ("--date=@1710000000", "-m"),
        ("--date=2024-03-09T16:00:00+02:00", "-m"),
        ("--date=2024-03-09 16:00:00 -0700", "-m"),
        ("--date=Sat, 9 Mar 2024 16:00:00 +0100", "-m"),
        ("--date=2024-03-09 16:00:00", "-m"),
    ];
    let mut all = Vec::new();
    for (flag, m) in cases {
        all.push(case("flag", STAGE_B, &[flag, m, "authored"], None));
    }
    all.push(case(
        "author and date",
        STAGE_B,
        &[
            "--author",
            "Q <q@x>",
            "--date",
            "1710000000 -0100",
            "-m",
            "both",
        ],
        None,
    ));
    all.push(case(
        "empty email",
        STAGE_B,
        &["--author", "Q <>", "-m", "no email"],
        None,
    ));
    for bad in ["nobody", "<q@x>", " <q@x>"] {
        all.push(case(
            "bad author",
            STAGE_B,
            &["--author", bad, "-m", "x"],
            None,
        ));
    }
    all.push(case(
        "bad date",
        STAGE_B,
        &["--date", "not a date", "-m", "x"],
        None,
    ));
    run_cases(&all);
}

#[test]
fn signoff_follows_the_trailer_rules() {
    let messages = [
        "subject only",
        "subject\n\nbody paragraph",
        "subject\n\nFoo: bar",
        "subject\n\nSigned-off-by: Other <o@example.com>",
        "subject\n\ntext\nSigned-off-by: Other <o@example.com>",
        "subject\n\nSigned-off-by: Cy Committer <cy@example.com>",
        "subject\n\nSigned-off-by: Cy Committer <cy@example.com>\nSigned-off-by: Other <o@example.com>",
        "subject\n\na\nb\nc\nd\nFoo: bar",
        "subject\nFoo: bar",
        "subject\n\n(cherry picked from commit abc)",
    ];
    let mut cases = Vec::new();
    for m in messages {
        for flag in ["-s", "--signoff"] {
            cases.push(case("signoff", STAGE_B, &[flag, "-m", m], None));
        }
    }
    cases.push(case(
        "signoff from file",
        &[W("m.txt", "from file\n"), STAGE_B[0], STAGE_B[1]],
        &["-s", "-F", "m.txt"],
        None,
    ));
    run_cases(&cases);
}

#[test]
fn amend_rewrites_the_tip_as_git_does() {
    run_cases(&[
        case("message only", &[], &["--amend", "-m", "reworded"], None),
        case(
            "no-edit with staged change",
            STAGE_B,
            &["--amend", "--no-edit"],
            None,
        ),
        case(
            "no-edit without changes",
            &[],
            &["--amend", "--no-edit"],
            None,
        ),
        case(
            "message from file",
            &[W("m.txt", "new words\n\nmore\n")],
            &["--amend", "-F", "m.txt"],
            None,
        ),
        case(
            "author",
            &[],
            &["--amend", "--no-edit", "--author", "Zed <zed@example.org>"],
            None,
        ),
        case(
            "author with message and staged change",
            STAGE_B,
            &["--amend", "--author=Zed <zed@example.org>", "-m", "both"],
            None,
        ),
        case(
            "date",
            &[],
            &["--amend", "--no-edit", "--date", "1710000000 +0900"],
            None,
        ),
        case("signoff", &[], &["--amend", "--no-edit", "-s"], None),
        case(
            "signoff twice",
            &[Git(&[
                "commit",
                "-q",
                "--amend",
                "--no-edit",
                "-s",
                "--allow-empty",
            ])],
            &["--amend", "--no-edit", "-s"],
            None,
        ),
        case(
            "all",
            &[W("a.txt", "edited\n")],
            &["--amend", "-a", "--no-edit"],
            None,
        ),
        case(
            "root commit",
            &[
                Git(&["checkout", "-q", "--orphan", "root"]),
                W("r.txt", "r\n"),
                Git(&["add", "r.txt"]),
                Git(&["commit", "-q", "-m", "root"]),
                W("r.txt", "r2\n"),
                Git(&["add", "r.txt"]),
            ],
            &["--amend", "--no-edit"],
            None,
        ),
        case(
            "empties the commit",
            &[Git(&["commit", "-q", "--allow-empty", "-m", "nothing"])],
            &["--amend", "--no-edit"],
            None,
        ),
        case(
            "empties the commit but allowed",
            &[Git(&["commit", "-q", "--allow-empty", "-m", "nothing"])],
            &["--amend", "--no-edit", "--allow-empty"],
            None,
        ),
        case(
            "merge commit",
            &[
                Git(&["checkout", "-q", "side"]),
                W("c.txt", "c\n"),
                Git(&["add", "c.txt"]),
                Git(&["commit", "-q", "-m", "side work"]),
                Git(&["checkout", "-q", "main"]),
                Git(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]),
            ],
            &["--amend", "-m", "merge reworded"],
            None,
        ),
        case(
            "unborn branch",
            &[
                Git(&["checkout", "-q", "--orphan", "fresh"]),
                Git(&["rm", "-rfq", "."]),
            ],
            &["--amend", "-m", "x"],
            None,
        ),
        case(
            "during a merge",
            &[
                Git(&["checkout", "-q", "side"]),
                W("c.txt", "c\n"),
                Git(&["add", "c.txt"]),
                Git(&["commit", "-q", "-m", "side work"]),
                Git(&["checkout", "-q", "main"]),
                Git(&["merge", "-q", "--no-ff", "--no-commit", "side"]),
            ],
            &["--amend", "-m", "x"],
            None,
        ),
    ]);
}

/// A merge of `side` into `main` that conflicts on `a.txt`, with the conflict resolved and staged.
const CONFLICTED_MERGE: &[Step] = &[
    Git(&["checkout", "-q", "side"]),
    W("a.txt", "side\n"),
    Git(&["commit", "-q", "-am", "side change"]),
    Git(&["checkout", "-q", "main"]),
    W("a.txt", "main\n"),
    Git(&["commit", "-q", "-am", "main change"]),
    Git(&["merge", "side"]),
    W("a.txt", "resolved\n"),
    Git(&["add", "a.txt"]),
];

#[test]
fn concluding_a_conflicted_merge_matches_git() {
    // --no-edit keeps the `# Conflicts:` comment lines; an editor (or none) strips them.
    run_cases(&[
        case("no-edit", CONFLICTED_MERGE, &["--no-edit"], None),
        case("no editor", CONFLICTED_MERGE, &[], None),
        case("explicit message", CONFLICTED_MERGE, &["-m", "done"], None),
    ]);
}

#[test]
fn combinations_match_git() {
    run_cases(&[
        case(
            "everything",
            &[W("m.txt", "combined\n\nbody\n"), STAGE_B[0], STAGE_B[1]],
            &[
                "-F",
                "m.txt",
                "-s",
                "--author",
                "Zed <zed@example.org>",
                "--date",
                "1710000000 +0000",
            ],
            None,
        ),
        case(
            "empty with signoff and author",
            &[],
            &[
                "--allow-empty",
                "-s",
                "--author",
                "Zed <zed@example.org>",
                "-m",
                "x",
            ],
            None,
        ),
        case(
            "amend everything",
            STAGE_B,
            &[
                "--amend",
                "-s",
                "--author",
                "Zed <zed@example.org>",
                "--date",
                "1710000000 +0000",
                "-m",
                "all of it",
            ],
            None,
        ),
        case(
            "amend with stdin",
            &[],
            &["--amend", "-F", "-"],
            Some("replaced on stdin\n"),
        ),
    ]);
}

#[test]
fn a_pyn_server_home_still_refuses_the_git_only_flags() {
    let tmp = tempdir().unwrap();
    for flags in [
        &["--author", "A <a@x>"][..],
        &["--date", "1700000000"],
        &["-s"],
        &["--signoff"],
        &["--amend"],
        &["--no-edit"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(tmp.path())
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", tmp.path())
            .args(["commit", "-m", "x"])
            .args(flags)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(129), "{flags:?}: {err}");
        assert!(
            err.contains("applies to a git repository, not a pyn server"),
            "{flags:?}: {err}"
        );
    }
}

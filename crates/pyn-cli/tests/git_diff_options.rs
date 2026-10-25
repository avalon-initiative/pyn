//! `pyn diff`, `show`, `log` and `stash show` options on a git repository, each compared with real git on the same
//! repository: whitespace modes, hunk layout, `--raw`, prefixes, `--word-diff`, `--color`, `--no-index`, the `--stat`
//! family and `--dirstat`. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const C_ONE: &str = "#include <stdio.h>\n\nint add(int a, int b)\n{\n    return a + b;\n}\n\nint sub(int a, int b)\n{\n    return a - b;\n}\n\nint main(void)\n{\n    printf(\"%d\\n\", add(1, 2));\n    printf(\"%d\\n\", sub(3, 1));\n    return 0;\n}\n";
const C_TWO: &str = "#include <stdio.h>\n\nint add(int a, int b)\n{\n    return a + b;\n}\n\nint sub(int a, int b)\n{\n    int d = a - b;\n    return d;\n}\n\nint mul(int a, int b)\n{\n    return a * b;\n}\n\nint main(void)\n{\n    printf(\"%d\\n\", add(1, 2));\n    printf(\"%d\\n\", sub(3, 1));\n    printf(\"%d\\n\", mul(2, 3));\n    return 0;\n}\n";

struct Fixture {
    _tmp: TempDir,
    repo: PathBuf,
    format: &'static str,
}

fn lines(prefix: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{prefix} {i}\n")).collect()
}

impl Fixture {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let repo = tmp.path().canonicalize().unwrap().join("repo");
        init(&repo, format, false);
        let f = Self {
            _tmp: tmp,
            repo,
            format,
        };
        f.build();
        f
    }

    fn write(&self, path: &str, content: &[u8]) {
        if let Some(dir) = self.repo.join(path).parent() {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(self.repo.join(path), content).unwrap();
    }

    fn git(&self, args: &[&str]) {
        let out = git(&self.repo).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(&self, message: &str) {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// Two commits, then staged and unstaged edits over a mix of text, whitespace, a binary file and a rename.
    fn build(&self) {
        self.write("src/main.c", C_ONE.as_bytes());
        self.write(
            "src/util.c",
            b"static int one(void)\n{\n\treturn 1;\n}\n\nstatic int two(void)\n{\n\treturn 2;\n}\n",
        );
        self.write(
            "docs/readme.txt",
            b"The quick brown fox jumps over the lazy dog\nsecond line here\nthird line is plain\n",
        );
        self.write(
            "docs/notes.txt",
            b"alpha  beta\ngamma\t\ndelta\r\nepsilon\n\nzeta\n",
        );
        self.write("data/blob.bin", b"\0\x01\x02\x03\x04\x05\x06\x07\n");
        self.write("tools/run.sh", b"#!/bin/sh\necho hi\n");
        self.git(&["add", "-A"]);
        self.git(&["update-index", "--chmod=+x", "tools/run.sh"]);
        self.write("old_name.txt", lines("line", 20).as_bytes());
        self.write("gone.txt", b"bye\nbye\n");
        self.commit("one");

        self.write("src/main.c", C_TWO.as_bytes());
        self.write("src/util.c", b"static int one(void)\n{\n    return 1;  \n}\n\n\nstatic int two(void)\n{\n\treturn 2;\n}\n");
        self.write(
            "docs/readme.txt",
            b"The quick red fox leaps over the lazy dog\nsecond line here\nthird line is plain, really\n",
        );
        self.write(
            "docs/notes.txt",
            b"alpha beta\ngamma \r\ndelta\nepsilon\n\n\nzeta\n",
        );
        self.write("data/blob.bin", b"\0\x01\x02\x03\x04\x05\x06\x08\x09\n");
        self.git(&["update-index", "--chmod=-x", "tools/run.sh"]);
        self.git(&["rm", "-q", "gone.txt"]);
        self.git(&["mv", "old_name.txt", "new_name.txt"]);
        self.write(
            "new_name.txt",
            lines("line", 20).replace("line 7", "LINE 7").as_bytes(),
        );
        self.write("docs/added.txt", b"first\nsecond\n  \n");
        self.commit("two");

        self.write(
            "src/main.c",
            C_TWO.replace("add(1, 2)", "add(4, 5)").as_bytes(),
        );
        self.git(&["add", "src/main.c"]);
        self.write(
            "src/util.c",
            b"static int one(void)\n{\n\treturn 1;\n}\n\n\nstatic int two(void)\n{\n  return 2;\n}\n\nstatic int three(void)\n{\n\treturn 3;\n}\n",
        );
        self.write(
            "docs/readme.txt",
            b"The quick red fox leaps over the lazy dog\nsecond line is here\nthird line is plain, really\nfourth\n",
        );
        self.write(
            "docs/notes.txt",
            b"alpha beta\ngamma\ndelta\nepsilon\nzeta\n",
        );
    }

    fn pyn(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(dir)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.repo.parent().unwrap())
            .env("TZ", "UTC")
            .output()
            .unwrap()
    }

    fn same(&self, args: &[&str]) {
        self.same_in(&self.repo, args);
    }

    fn same_in(&self, dir: &Path, args: &[&str]) {
        let want = git(dir).env("TZ", "UTC").args(args).output().unwrap();
        let got = self.pyn(dir, args);
        let err = String::from_utf8_lossy(&got.stderr);
        assert_eq!(
            got.status.code(),
            want.status.code(),
            "{} {args:?}: {err}",
            self.format
        );
        assert_eq!(
            String::from_utf8_lossy(&got.stdout),
            String::from_utf8_lossy(&want.stdout),
            "{} {args:?}",
            self.format
        );
    }

    fn all(&self, cases: &[&[&str]]) {
        for args in cases {
            self.same(args);
        }
    }

    fn config(&self, key: &str, value: &str) {
        self.git(&["config", key, value]);
    }

    fn unconfig(&self, key: &str) {
        self.git(&["config", "--unset", key]);
    }
}

fn each_format(check: impl Fn(&Fixture)) {
    for format in FORMATS {
        check(&Fixture::new(format));
    }
}

fn usage_error(repo: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
        .current_dir(repo)
        .args(args)
        .env_clear()
        .env("PATH", path_env())
        .env("HOME", repo.parent().unwrap())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(129), "{args:?}");
    assert!(out.stdout.is_empty(), "{args:?}");
}

#[test]
fn whitespace_modes_match_git() {
    each_format(|f| {
        for flag in [
            "-b",
            "--ignore-space-change",
            "--ignore-space-at-eol",
            "--ignore-cr-at-eol",
            "--ignore-blank-lines",
            "-w",
        ] {
            f.all(&[
                &["diff", flag],
                &["diff", flag, "--cached"],
                &["diff", flag, "HEAD~2", "HEAD"],
                &["diff", flag, "HEAD~1", "HEAD", "-U0"],
                &["diff", flag, "HEAD~1", "HEAD", "--stat"],
                &["diff", flag, "HEAD~1", "HEAD", "--numstat"],
                &["diff", flag, "HEAD~1", "HEAD", "--shortstat"],
                &["diff", flag, "HEAD~1", "HEAD", "--name-status"],
                &["diff", flag, "HEAD~1", "HEAD", "--exit-code"],
                &["show", flag],
                &["show", flag, "HEAD~1", "--stat"],
                &["log", "-p", flag],
                &["log", "--stat", flag],
            ]);
        }
        f.all(&[
            &["diff", "-b", "--ignore-blank-lines", "HEAD~1", "HEAD"],
            &[
                "diff",
                "--ignore-space-at-eol",
                "-U1",
                "--ignore-blank-lines",
                "HEAD~1",
                "HEAD",
            ],
            &[
                "diff",
                "--ignore-blank-lines",
                "--exit-code",
                "HEAD~1",
                "HEAD",
                "--",
                "docs/notes.txt",
            ],
            &[
                "diff",
                "--ignore-blank-lines",
                "--exit-code",
                "HEAD~1",
                "HEAD",
                "--",
                "src/util.c",
            ],
            &["diff", "--ignore-cr-at-eol", "--check", "HEAD~1", "HEAD"],
        ]);
    });
}

#[test]
fn function_context_and_hunk_distance_match_git() {
    each_format(|f| {
        f.all(&[
            &["diff", "-W"],
            &["diff", "--function-context", "--cached"],
            &["diff", "-W", "HEAD~2", "HEAD"],
            &["diff", "-W", "-U0", "HEAD~2", "HEAD"],
            &["diff", "-W", "-U1", "HEAD~1", "HEAD", "--", "src"],
            &["diff", "-W", "--stat", "HEAD~1", "HEAD"],
            &["show", "-W", "HEAD~1"],
            &["log", "-p", "-W", "-2"],
            &["diff", "--inter-hunk-context=3", "-U0", "HEAD~2", "HEAD"],
            &["diff", "--inter-hunk-context=1", "-U1", "HEAD~1", "HEAD"],
            &["diff", "--inter-hunk-context=8", "-U0", "--", "src/main.c"],
            &["show", "--inter-hunk-context=2", "-U1"],
            &["log", "-p", "-U1", "--inter-hunk-context=5"],
        ]);
    });
}

#[test]
fn raw_and_object_id_options_match_git() {
    each_format(|f| {
        f.all(&[
            &["diff", "--raw"],
            &["diff", "--raw", "--cached"],
            &["diff", "--raw", "HEAD"],
            &["diff", "--raw", "HEAD~2", "HEAD"],
            &["diff", "--raw", "-z", "HEAD~2", "HEAD"],
            &["diff", "--raw", "--abbrev=12", "HEAD~2", "HEAD"],
            &["diff", "--raw", "--abbrev", "HEAD~2", "HEAD"],
            &["diff", "--raw", "--abbrev=2", "HEAD~1", "HEAD"],
            &["diff", "--raw", "--no-abbrev", "HEAD~1", "HEAD"],
            &["diff", "--raw", "--no-renames", "HEAD~1", "HEAD"],
            &[
                "diff",
                "--raw",
                "-M",
                "HEAD~1",
                "HEAD",
                "--",
                "new_name.txt",
                "old_name.txt",
            ],
            &["diff", "--name-only", "-z", "HEAD~2", "HEAD"],
            &["diff", "--name-status", "-z", "HEAD~2", "HEAD"],
            &["diff", "--numstat", "-z", "HEAD~2", "HEAD"],
            &["diff", "--stat", "-z", "HEAD~1", "HEAD"],
            &["diff", "-z", "HEAD~1", "HEAD"],
            &["diff", "--full-index", "HEAD~2", "HEAD"],
            &["diff", "--abbrev=10", "HEAD~1", "HEAD"],
            &["diff", "--abbrev=0", "HEAD~1", "HEAD"],
            &["show", "--raw"],
            &["show", "--raw", "-p", "HEAD~1"],
            &["show", "--full-index", "HEAD~1"],
            &["log", "--raw", "-2"],
            &["log", "--raw", "-p", "-2", "-M"],
            &["diff", "--raw", "-p", "HEAD~1", "HEAD"],
            &["diff", "--patch-with-raw", "HEAD~1", "HEAD"],
        ]);
    });
}

#[test]
fn prefixes_text_and_reverse_match_git() {
    each_format(|f| {
        f.all(&[
            &["diff", "--no-prefix", "HEAD~2", "HEAD"],
            &[
                "diff",
                "--src-prefix=old/",
                "--dst-prefix=new/",
                "HEAD~1",
                "HEAD",
            ],
            &["diff", "--src-prefix=x/", "HEAD~1", "HEAD"],
            &["diff", "--dst-prefix=", "HEAD~1", "HEAD"],
            &["diff", "--src-prefix", "left/", "HEAD~1", "HEAD"],
            &["diff", "--no-prefix", "--stat", "HEAD~1", "HEAD"],
            &["show", "--no-prefix"],
            &["log", "-p", "--src-prefix=a:", "--dst-prefix=b:", "-2"],
            &["diff", "-a", "HEAD~2", "HEAD"],
            &["diff", "--text", "--stat", "HEAD~2", "HEAD"],
            &["diff", "-a", "--numstat", "HEAD~2", "HEAD"],
            &["diff", "-R"],
            &["diff", "-R", "--cached"],
            &["diff", "-R", "HEAD~2", "HEAD"],
            &["diff", "-R", "--raw", "HEAD~1", "HEAD"],
            &["diff", "-R", "--stat", "HEAD~1", "HEAD"],
            &["diff", "-R", "--no-prefix", "HEAD~1", "HEAD"],
            &["show", "-R"],
            &["log", "-R", "-p", "-2"],
        ]);
    });
}

#[test]
fn diff_filter_and_silent_diff_match_git() {
    each_format(|f| {
        f.all(&[
            &["diff", "--diff-filter=A", "HEAD~2", "HEAD"],
            &["diff", "--diff-filter=D", "--name-status", "HEAD~2", "HEAD"],
            &[
                "diff",
                "--diff-filter=MR",
                "--name-status",
                "HEAD~2",
                "HEAD",
            ],
            &[
                "diff",
                "--diff-filter=ad",
                "--name-status",
                "HEAD~2",
                "HEAD",
            ],
            &["diff", "--diff-filter=R", "--stat", "HEAD~1", "HEAD"],
            &["show", "--diff-filter=A", "--stat", "HEAD~1"],
            &["diff", "-s"],
            &["diff", "-s", "--exit-code"],
            &["diff", "--no-patch", "--exit-code", "HEAD", "HEAD"],
            &["diff", "--minimal", "HEAD~2", "HEAD"],
            &["diff", "--diff-algorithm=minimal", "HEAD~2", "HEAD"],
            &["diff", "--diff-algorithm=myers", "HEAD~2", "HEAD"],
            &["diff", "--diff-algorithm=default", "HEAD~2", "HEAD"],
            &["diff", "--no-indent-heuristic", "HEAD~2", "HEAD"],
            &["diff", "--indent-heuristic", "HEAD~2", "HEAD"],
            &["diff", "--no-ext-diff", "--no-textconv", "HEAD~2", "HEAD"],
            &["log", "-p", "--minimal", "-3"],
        ]);
    });
}

#[test]
fn word_diff_matches_git() {
    each_format(|f| {
        for mode in ["", "=plain", "=color", "=porcelain", "=none"] {
            let flag = format!("--word-diff{mode}");
            f.all(&[
                &["diff", &flag],
                &["diff", &flag, "--cached"],
                &["diff", &flag, "HEAD~2", "HEAD"],
                &["diff", &flag, "-U0", "HEAD~1", "HEAD"],
                &["diff", &flag, "HEAD~1", "HEAD", "--", "docs"],
                &["diff", &flag, "-w", "HEAD~1", "HEAD"],
                &["diff", &flag, "--stat", "HEAD~1", "HEAD"],
                &["show", &flag],
                &["show", &flag, "HEAD~1"],
                &["log", "-p", &flag, "-2"],
            ]);
        }
        f.all(&[
            &["diff", "--word-diff-regex=[a-z]+", "HEAD~2", "HEAD"],
            &["diff", "--word-diff-regex=.", "HEAD~2", "HEAD"],
            &[
                "diff",
                "--word-diff=porcelain",
                "--word-diff-regex=[A-Za-z]+",
                "HEAD~2",
                "HEAD",
            ],
            &[
                "diff",
                "--word-diff=color",
                "--word-diff-regex=[^ ]+",
                "HEAD~2",
                "HEAD",
            ],
            &["diff", "--color-words", "HEAD~2", "HEAD"],
            &["diff", "--color-words=[a-z]+", "HEAD~2", "HEAD"],
            &["diff", "--word-diff", "--color=always", "HEAD~2", "HEAD"],
            &[
                "diff",
                "--word-diff=plain",
                "--color=never",
                "HEAD~2",
                "HEAD",
            ],
            &["diff", "--word-diff=color", "--no-color", "HEAD~2", "HEAD"],
            &["diff", "--word-diff", "--no-prefix", "HEAD~2", "HEAD"],
            &["diff", "--word-diff", "-W", "HEAD~2", "HEAD"],
        ]);
        f.config("diff.wordRegex", "[a-z]+");
        f.all(&[&["diff", "--word-diff", "HEAD~2", "HEAD"]]);
        f.unconfig("diff.wordRegex");
    });
}

#[test]
fn color_matches_git() {
    each_format(|f| {
        f.all(&[
            &["diff", "--color=always"],
            &["diff", "--color=always", "--cached"],
            &["diff", "--color=always", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "-U0", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "-W", "HEAD~2", "HEAD"],
            &["diff", "--color", "HEAD~2", "HEAD"],
            &["diff", "--color=never", "HEAD~2", "HEAD"],
            &["diff", "--color=auto", "HEAD~2", "HEAD"],
            &["diff", "--no-color", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "--no-color", "HEAD~2", "HEAD"],
            &["diff", "--no-color", "--color=always", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "--stat", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "--check", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "--summary", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "--numstat", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "--name-status", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "--raw", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "-R", "HEAD~1", "HEAD"],
            &["diff", "--color=always", "-M", "HEAD~1", "HEAD"],
            &["diff", "--color=always", "-p", "--stat", "HEAD~1", "HEAD"],
            &[
                "diff",
                "--color=always",
                "--ws-error-highlight=all",
                "HEAD~2",
                "HEAD",
            ],
            &[
                "diff",
                "--color=always",
                "--ws-error-highlight=old,context",
                "HEAD~2",
                "HEAD",
            ],
            &[
                "diff",
                "--color=always",
                "--ws-error-highlight=none",
                "HEAD~2",
                "HEAD",
            ],
            &["show", "--color=always"],
            &["show", "--color=always", "HEAD~1", "--stat"],
            &["show", "--color=always", "--oneline"],
            &["show", "--color=always", "--format=%h %s"],
            &["show", "--color=always", "--format=raw", "-s"],
            &["log", "--color=always", "-p", "-2"],
            &["log", "--color=always", "--oneline", "--stat"],
            &["log", "--color=always", "--raw", "-1"],
        ]);
    });
}

#[test]
fn color_settings_match_git() {
    each_format(|f| {
        f.config("color.ui", "always");
        f.all(&[
            &["diff", "HEAD~2", "HEAD"],
            &["diff", "--no-color", "HEAD~2", "HEAD"],
            &["show", "--stat"],
        ]);
        f.config("color.diff", "never");
        f.all(&[
            &["diff", "HEAD~2", "HEAD"],
            &["diff", "--color=always", "-U0", "HEAD~1", "HEAD"],
        ]);
        f.config("color.diff", "always");
        f.config("color.diff.meta", "yellow bold");
        f.config("color.diff.frag", "magenta");
        f.config("color.diff.func", "green dim");
        f.config("color.diff.context", "blue");
        f.config("color.diff.old", "#ff0000 ul");
        f.config("color.diff.new", "bold 202 brightcyan");
        f.config("color.diff.whitespace", "reverse");
        f.config("color.diff.commit", "cyan");
        f.all(&[
            &["diff", "HEAD~2", "HEAD"],
            &["diff", "--stat", "HEAD~2", "HEAD"],
            &["diff", "--check", "HEAD~2", "HEAD"],
            &["diff", "--word-diff", "HEAD~2", "HEAD"],
            &["diff", "--word-diff=porcelain", "HEAD~2", "HEAD"],
            &["show"],
            &["log", "-p", "-2"],
        ]);
        f.config("color.diff.new", "normal");
        f.config("color.diff.old", "nonsense");
        f.all(&[&["diff", "HEAD~2", "HEAD"]]);
        f.config("color.diff.whitespace", "normal");
        f.all(&[&["diff", "HEAD~2", "HEAD"]]);
        f.config("diff.wsErrorHighlight", "all");
        f.config("color.diff.whitespace", "red");
        f.all(&[&["diff", "HEAD~2", "HEAD"]]);
        f.config("diff.wsErrorHighlight", "none");
        f.all(&[&["diff", "HEAD~2", "HEAD"]]);
    });
}

#[test]
fn whitespace_rules_match_git() {
    each_format(|f| {
        f.write(
            "ws.txt",
            b"clean\ntrail  \n \tspace-before-tab\n        eight spaces\n\ttab\n  \n",
        );
        f.git(&["add", "-N", "ws.txt"]);
        for rule in [
            "trailing-space",
            "-trailing-space",
            "space-before-tab,tab-in-indent",
            "indent-with-non-tab",
            "indent-with-non-tab,tabwidth=4",
            "blank-at-eol,-blank-at-eof",
            "cr-at-eol",
        ] {
            f.config("core.whitespace", rule);
            f.all(&[
                &["diff", "--check"],
                &["diff", "--check", "HEAD~2", "HEAD"],
                &["diff", "--color=always", "HEAD~2", "HEAD"],
                &[
                    "diff",
                    "--color=always",
                    "--ws-error-highlight=all",
                    "HEAD~1",
                    "HEAD",
                ],
                &["diff", "--color=always", "ws.txt"],
            ]);
        }
        f.unconfig("core.whitespace");
        f.write(
            "conflict.txt",
            b"a\n<<<<<<< HEAD\nx\n=======\ny\n>>>>>>> other\nb\n",
        );
        f.git(&["add", "-N", "conflict.txt"]);
        f.all(&[
            &["diff", "--check"],
            &["diff", "--check", "--", "conflict.txt"],
        ]);
    });
}

#[test]
fn stat_family_matches_git() {
    each_format(|f| {
        f.all(&[
            &["diff", "--stat=40", "HEAD~2", "HEAD"],
            &["diff", "--stat=40,12", "HEAD~2", "HEAD"],
            &["diff", "--stat=60,20,3", "HEAD~2", "HEAD"],
            &["diff", "--stat=,8", "HEAD~2", "HEAD"],
            &["diff", "--stat=200", "HEAD~2", "HEAD"],
            &["diff", "--stat-width=50", "HEAD~2", "HEAD"],
            &["diff", "--stat-width", "50", "HEAD~2", "HEAD"],
            &["diff", "--stat-name-width=10", "HEAD~2", "HEAD"],
            &[
                "diff",
                "--stat",
                "--stat-name-width=10",
                "--stat-width=48",
                "HEAD~2",
                "HEAD",
            ],
            &["diff", "--stat-graph-width=4", "HEAD~2", "HEAD"],
            &["diff", "--stat-count=2", "HEAD~2", "HEAD"],
            &["diff", "--stat", "--stat-count=0", "HEAD~2", "HEAD"],
            &["diff", "--stat=30", "--stat-width=70", "HEAD~2", "HEAD"],
            &["diff", "--compact-summary", "HEAD~2", "HEAD"],
            &[
                "diff",
                "--stat",
                "--compact-summary",
                "-M",
                "HEAD~1",
                "HEAD",
            ],
            &["diff", "--compact-summary", "--summary", "HEAD~1", "HEAD"],
            &["diff", "--compact-summary", "--cached"],
            &["diff", "--stat-width=44", "-w", "HEAD~1", "HEAD"],
            &["diff", "--stat", "-p", "HEAD~1", "HEAD"],
            &["diff", "--patch-with-stat", "HEAD~1", "HEAD"],
            &["diff", "--numstat", "-p", "HEAD~1", "HEAD"],
            &["diff", "--shortstat", "-p", "HEAD~1", "HEAD"],
            &["diff", "--summary", "-p", "--stat", "HEAD~1", "HEAD"],
            &["show", "--stat=50"],
            &["show", "--stat", "-p", "HEAD~1"],
            &["show", "--compact-summary", "HEAD~1"],
            &["log", "--stat=50,20", "-2"],
            &["log", "--stat-count=1", "--stat", "-2"],
            &["log", "--compact-summary", "-2"],
            &["log", "--stat", "-p", "-2"],
        ]);
        f.config("diff.statGraphWidth", "6");
        f.all(&[
            &["diff", "--stat", "HEAD~2", "HEAD"],
            &["diff", "--stat-graph-width=3", "HEAD~2", "HEAD"],
        ]);
        f.unconfig("diff.statGraphWidth");
        // The width git takes from the terminal.
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&f.repo)
            .args(["diff", "--stat", "HEAD~2", "HEAD"])
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", f.repo.parent().unwrap())
            .env("COLUMNS", "55")
            .output()
            .unwrap();
        let want = git(&f.repo)
            .env("COLUMNS", "55")
            .args(["diff", "--stat", "HEAD~2", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(out.stdout, want.stdout, "{}", f.format);
    });
}

#[test]
fn dirstat_matches_git() {
    each_format(|f| {
        for params in [
            "",
            "=lines",
            "=files",
            "=cumulative",
            "=lines,cumulative",
            "=changes,5",
            "=0",
            "=0,files",
            "=1.5",
            "=100",
            "=files,noncumulative",
        ] {
            let flag = format!("--dirstat{params}");
            f.all(&[
                &["diff", &flag],
                &["diff", &flag, "--cached"],
                &["diff", &flag, "HEAD~2", "HEAD"],
                &["diff", &flag, "HEAD~1", "HEAD"],
                &["diff", &flag, "-M", "HEAD~1", "HEAD"],
                &["diff", &flag, "-w", "HEAD~1", "HEAD"],
                &["show", &flag],
                &["show", &flag, "HEAD~1", "-p"],
            ]);
        }
        f.all(&[
            &["diff", "--dirstat-by-file", "HEAD~2", "HEAD"],
            &["diff", "--dirstat-by-file=10", "HEAD~2", "HEAD"],
            &["diff", "--cumulative", "HEAD~2", "HEAD"],
            &["diff", "-p", "--dirstat", "HEAD~2", "HEAD"],
            &["log", "--dirstat=0", "-2"],
            &["log", "--dirstat=0,lines", "-p", "-2"],
        ]);
    });
}

#[test]
fn stash_show_takes_the_same_options() {
    // git 2.43 cannot stash in a sha256 repository.
    let f = Fixture::new("sha1");
    f.git(&["stash", "push", "-q"]);
    f.all(&[
        &["stash", "show", "-b"],
        &["stash", "show", "--color=always"],
        &["stash", "show", "--raw"],
        &["stash", "show", "-p", "-W"],
        &["stash", "show", "--stat=40", "--compact-summary"],
        &["stash", "show", "--dirstat=0"],
        &["stash", "show", "-p", "--word-diff"],
        &["stash", "show", "-R", "-p", "--no-prefix"],
        &["stash", "show", "--patch-with-stat"],
    ]);
}

/// A folder tree with files that match, differ, exist on one side only and differ in mode.
fn no_index_tree(root: &Path) {
    let put = |path: &str, content: &[u8]| {
        let p = root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    };
    put("d1/same.txt", b"same\n");
    put("d2/same.txt", b"same\n");
    put("d1/only1.txt", b"x\ny\n");
    put("d2/only2.txt", b"z\n");
    put("d1/sub/mod.txt", b"one\ntwo\nthree\n");
    put("d2/sub/mod.txt", b"one\n2\nthree\n");
    put("d1/a.txt", b"first\n");
    put("d2/a.txt", b"second\n");
    put("d1/a/x", b"1\n");
    put("d2/a/x", b"2\n");
    put("d1/bin", b"\0one\n");
    put("d2/bin", b"\0two\n");
    put("d1/mode.sh", b"echo\n");
    put("d2/mode.sh", b"echo\n");
    put("d1/moved_a", lines("same line", 12).as_bytes());
    put("d2/moved_b", lines("same line", 12).as_bytes());
    put("f1", b"a\nb\nc\n");
    put("f2", b"a\nB\nc\n");
    put("empty", b"");
    put("nonl", b"no newline");
    use std::os::unix::fs::{PermissionsExt, symlink};
    fs::set_permissions(root.join("d2/mode.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("target-one", root.join("d1/link")).unwrap();
    symlink("target-two", root.join("d2/link")).unwrap();
    symlink("f1", root.join("sl1")).unwrap();
    symlink("f2", root.join("sl2")).unwrap();
}

#[test]
fn no_index_outside_a_repository_matches_git() {
    let f = Fixture::new("sha1");
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("plain");
    fs::create_dir_all(&root).unwrap();
    no_index_tree(&root);
    let cases: &[&[&str]] = &[
        &["diff", "--no-index", "f1", "f2"],
        &["diff", "--no-index", "--", "f1", "f2"],
        &["diff", "f1", "f2"],
        &["diff", "--no-index", "f1", "f1"],
        &["diff", "--no-index", "./f1", "./f2"],
        &["diff", "--no-index", "d1", "d2"],
        &["diff", "--no-index", "d1/", "d2/"],
        &["diff", "d1", "d2"],
        &["diff", "--no-index", "d1", "d2", "-M"],
        &["diff", "--no-index", "d1", "d2", "--no-renames"],
        &["diff", "--no-index", "--stat", "d1", "d2"],
        &["diff", "--no-index", "--numstat", "d1", "d2"],
        &["diff", "--no-index", "--shortstat", "d1", "d2"],
        &["diff", "--no-index", "--name-only", "d1", "d2"],
        &["diff", "--no-index", "--name-status", "d1", "d2"],
        &["diff", "--no-index", "--raw", "d1", "d2"],
        &["diff", "--no-index", "--raw", "-z", "d1", "d2"],
        &["diff", "--no-index", "--summary", "d1", "d2"],
        &["diff", "--no-index", "--stat", "--summary", "d1", "d2"],
        &["diff", "--no-index", "--check", "d1", "d2"],
        &["diff", "--no-index", "--color=always", "d1", "d2"],
        &["diff", "--no-index", "--word-diff", "d1", "d2"],
        &["diff", "--no-index", "-U0", "d1", "d2"],
        &["diff", "--no-index", "--no-prefix", "f1", "f2"],
        &["diff", "--no-index", "-R", "f1", "f2"],
        &["diff", "--no-index", "-w", "d1", "d2"],
        &["diff", "--no-index", "--quiet", "f1", "f2"],
        &["diff", "--no-index", "--exit-code", "f1", "f2"],
        &["diff", "--no-index", "-s", "f1", "f2"],
        &["diff", "--no-index", "f1", "d2/a.txt"],
        &["diff", "--no-index", "d1/a.txt", "f1"],
        &["diff", "--no-index", "empty", "nonl"],
        &["diff", "--no-index", "nonl", "empty"],
        &["diff", "--no-index", "/dev/null", "f1"],
        &["diff", "--no-index", "f1", "/dev/null"],
        &["diff", "--no-index", "/dev/null", "/dev/null"],
        &["diff", "--no-index", "sl1", "sl2"],
        &["diff", "--no-index", "f1", "d1/missing"],
        &["diff", "--no-index", "f1", "d2"],
        &["diff", "--no-index", "missing", "f1"],
    ];
    for args in cases {
        f.same_in(&root, args);
    }
    // Links are left out of the dirstat: git 2.43 and later versions count their short text differently.
    fs::remove_file(root.join("d1/link")).unwrap();
    fs::remove_file(root.join("d2/link")).unwrap();
    f.same_in(&root, &["diff", "--no-index", "--dirstat=0", "d1", "d2"]);
    f.same_in(
        &root,
        &["diff", "--no-index", "--dirstat=0,lines", "d1", "d2"],
    );
    let abs: Vec<String> = ["f1", "f2"]
        .iter()
        .map(|n| root.join(n).to_string_lossy().into_owned())
        .collect();
    f.same_in(&root, &["diff", "--no-index", &abs[0], &abs[1]]);
}

#[test]
fn no_index_inside_a_repository_matches_git() {
    each_format(|f| {
        let root = f.repo.join("scratch");
        fs::create_dir_all(&root).unwrap();
        no_index_tree(&root);
        let cases: &[&[&str]] = &[
            &["diff", "--no-index", "scratch/f1", "scratch/f2"],
            &["diff", "--no-index", "scratch/d1", "scratch/d2"],
            &["diff", "--no-index", "scratch/d1", "scratch/d2", "--stat"],
            &[
                "diff",
                "--no-index",
                "scratch/d1",
                "scratch/d2",
                "--summary",
            ],
            &["diff", "--no-index", "src/main.c", "scratch/f1"],
            &["diff", "--no-index", "--name-status", "docs", "scratch/d1"],
        ];
        for args in cases {
            f.same(args);
        }
        f.same_in(&root, &["diff", "--no-index", "f1", "f2"]);
        f.same_in(&root, &["diff", "--no-index", "d1", "d2", "--stat"]);
        f.same_in(&root, &["diff", "--no-index", "--raw", "d1", "d2"]);
    });
}

#[test]
fn diff_configuration_matches_git() {
    each_format(|f| {
        f.config("diff.noprefix", "true");
        f.all(&[
            &["diff", "HEAD~1", "HEAD"],
            &["diff", "--src-prefix=q/", "HEAD~1", "HEAD"],
            &["diff", "-R", "--dst-prefix=z/", "HEAD~1", "HEAD"],
            &["show"],
            &["log", "-p", "-1"],
        ]);
        f.unconfig("diff.noprefix");
        f.config("diff.indentHeuristic", "false");
        f.all(&[
            &["diff", "HEAD~2", "HEAD"],
            &["diff", "--indent-heuristic", "HEAD~2", "HEAD"],
        ]);
        f.unconfig("diff.indentHeuristic");
        f.config("diff.algorithm", "minimal");
        f.all(&[
            &["diff", "HEAD~2", "HEAD"],
            &["diff", "--diff-algorithm=default", "HEAD~2", "HEAD"],
        ]);
        f.unconfig("diff.algorithm");
    });
}

#[test]
fn unfinished_options_are_usage_errors() {
    let f = Fixture::new("sha1");
    for args in [
        &["diff", "--patience"][..],
        &["diff", "--histogram"],
        &["diff", "--diff-algorithm=patience"],
        &["diff", "--binary"],
        &["diff", "--color=sometimes"],
        &["diff", "--word-diff=wide"],
        &["diff", "--ws-error-highlight=sideways"],
        &["diff", "--stat=abc"],
        &["diff", "--stat-width=wide"],
        &["diff", "--dirstat=abc"],
        &["diff", "--diff-filter=Q"],
        &["diff", "--no-index", "f1"],
        &["diff", "--no-index", "-", "f1"],
        &["diff", "--raw", "--stat"],
        &["diff", "--stat-width=40", "--numstat"],
        &["diff", "--dirstat", "--stat"],
        &["diff", "-p", "--name-only"],
        &["diff", "-p", "--check"],
        &["diff", "-s", "--stat"],
        &["diff", "--no-prefix", "--src-prefix=x/"],
        &["log", "--diff-filter=A"],
    ] {
        usage_error(&f.repo, args);
    }
}

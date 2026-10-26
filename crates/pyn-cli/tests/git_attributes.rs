//! Line-ending conversion and `.gitattributes` on a git repository, compared with real git on identical repositories:
//! what `add` stores, what checkout writes, and what `status` and `diff` report. Real git is the oracle in these tests
//! only, and only blob ids, work tree bytes and report lines are compared, never wording.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Pair {
    _tmp: TempDir,
    root: PathBuf,
    oracle: PathBuf,
    ours: PathBuf,
}

struct Case {
    config: &'static str,
    attributes: &'static [(&'static str, &'static str)],
    info: &'static str,
    files: &'static [(&'static str, &'static [u8])],
}

const SAMPLES: &[(&str, &[u8])] = &[
    ("lf.txt", b"a\nb\n"),
    ("crlf.txt", b"a\r\nb\r\n"),
    ("mixed.txt", b"a\r\nb\nc\r\n"),
    ("nul.txt", b"a\0\r\nb\n"),
    ("lone.txt", b"a\rb\n"),
    ("none.txt", b"no newline"),
    ("sub/deep.txt", b"x\r\ny\r\n"),
    ("a.bat", b"x\ny\n"),
    ("a.sh", b"x\r\ny\r\n"),
    ("a.dat", b"x\r\ny\n"),
    ("a.c", b"x\r\ny\r\n"),
    ("a.bin", b"x\r\ny\r\n"),
    ("id.txt", b"$Id$ and $Id: foo $\r\nend\n"),
    ("eof-id.txt", b"$Id$"),
    ("ctrl-z.txt", b"a\nb\n\x1a"),
    ("cr-end.txt", b"a\nb\r"),
    ("empty.txt", b""),
    ("lf-only-lone-cr.txt", b"a\r\nb\rc\nd\n"),
];

impl Pair {
    fn new(format: &str, case: &Case) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (oracle, ours) = (root.join("oracle"), root.join("ours"));
        for repo in [&oracle, &ours] {
            init(repo, format, false);
            run(repo, &["config", "user.name", "t"]);
            run(repo, &["config", "user.email", "t@example.com"]);
            run(repo, &["config", "core.filemode", "true"]);
            let mut config = fs::read_to_string(repo.join(".git/config")).unwrap();
            config.push_str(case.config);
            fs::write(repo.join(".git/config"), config).unwrap();
            for (path, text) in case.attributes {
                write(repo, path, text.as_bytes());
            }
            fs::write(repo.join(".git/info/attributes"), case.info).unwrap();
            for (path, bytes) in case.files {
                write(repo, path, bytes);
            }
        }
        Self {
            _tmp: tmp,
            root,
            oracle,
            ours,
        }
    }

    fn pyn(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.ours)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git(&self, args: &[&str]) -> (i32, String, String) {
        let out = git(&self.oracle).args(args).output().unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// Runs `args` with both tools and requires the same exit status and the same conversion warnings.
    fn both(&self, args: &[&str]) -> (i32, String) {
        let (want, got) = (self.git(args), self.pyn(args));
        assert_eq!(got.0, want.0, "{args:?}: pyn said {}", got.2);
        let warned = |s: &str| {
            let mut v: Vec<String> = s
                .lines()
                .filter(|l| l.starts_with("warning: in the working copy"))
                .map(str::to_owned)
                .collect();
            v.sort();
            v
        };
        assert_eq!(warned(&got.2), warned(&want.2), "{args:?} warnings");
        (want.0, want.1)
    }

    /// The staged blob ids and paths, read by real git from each repository.
    fn index(&self) -> (String, String) {
        let ls = |dir: &Path| run(dir, &["ls-files", "-s"]);
        (ls(&self.ours), ls(&self.oracle))
    }

    fn same_index(&self, what: &str) {
        let (got, want) = self.index();
        assert_eq!(got, want, "{what}: index");
    }

    fn same_tree(&self, what: &str) {
        let (got, want) = (tree(&self.ours), tree(&self.oracle));
        assert_eq!(got, want, "{what}: work tree");
    }

    fn same_status(&self, what: &str) {
        let (got, want) = (
            self.pyn(&["status", "--porcelain"]),
            self.git(&["status", "--porcelain"]),
        );
        assert_eq!(got.1, want.1, "{what}: status");
    }

    fn same_diff(&self, what: &str, args: &[&str]) {
        let mut a = vec!["diff"];
        a.extend_from_slice(args);
        let (got, want) = (self.pyn(&a), self.git(&a));
        assert_eq!(got.0, want.0, "{what}: diff status");
        assert_eq!(got.1, want.1, "{what}: diff");
    }
}

fn write(repo: &Path, path: &str, bytes: &[u8]) {
    let file = repo.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, bytes).unwrap();
}

fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
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
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Adds, commits, wipes the work tree and brings it back, comparing each step with git.
fn round_trip(case: &Case) {
    for format in FORMATS {
        let p = Pair::new(format, case);
        p.same_tree("setup");
        let (code, _) = p.both(&["add", "-A"]);
        p.same_index("add");
        if code != 0 {
            continue;
        }
        p.same_status("after add");
        p.both(&["commit", "-q", "-m", "one"]);
        p.same_status("after commit");

        for repo in [&p.oracle, &p.ours] {
            for path in tree(repo).keys() {
                fs::remove_file(repo.join(path)).unwrap();
            }
        }
        p.same_status("deleted");
        p.both(&["restore", "--", "."]);
        p.same_tree("restore");
        p.same_status("after restore");

        // Edit every file the way a Windows editor would: append a CRLF line.
        for repo in [&p.oracle, &p.ours] {
            for (path, bytes) in tree(repo) {
                let mut more = bytes.clone();
                more.extend_from_slice(b"tail\r\n");
                fs::write(repo.join(path), more).unwrap();
            }
        }
        p.same_status("edited");
        p.same_diff("edited", &[]);
        p.same_diff("edited stat", &["--stat"]);
        p.both(&["add", "-A"]);
        p.same_index("add edits");
        p.same_diff("cached", &["--cached"]);

        p.both(&["commit", "-q", "-m", "two"]);
        for repo in [&p.oracle, &p.ours] {
            run(repo, &["branch", "other", "HEAD~1"]);
        }
        p.both(&["switch", "other"]);
        p.same_tree("switch");
        p.same_status("after switch");
        p.both(&["reset", "-q", "--hard", "main"]);
        p.same_tree("reset --hard");
        p.same_status("after reset");
    }
}

const NO_ATTRIBUTES: &[(&str, &str)] = &[];

#[test]
fn autocrlf_true_converts_both_ways() {
    round_trip(&Case {
        config: "[core]\n\tautocrlf = true\n",
        attributes: NO_ATTRIBUTES,
        info: "",
        files: SAMPLES,
    });
}

#[test]
fn autocrlf_input_converts_on_the_way_in_only() {
    round_trip(&Case {
        config: "[core]\n\tautocrlf = input\n",
        attributes: NO_ATTRIBUTES,
        info: "",
        files: SAMPLES,
    });
}

#[test]
fn text_and_eol_attributes_decide_per_path() {
    round_trip(&Case {
        config: "",
        attributes: &[(
            ".gitattributes",
            "*.txt text\n*.bat eol=crlf\n*.sh text eol=lf\n*.dat -text\n*.c crlf\n*.bin binary\nid.txt ident\n",
        )],
        info: "",
        files: SAMPLES,
    });
}

#[test]
fn text_auto_guesses_from_the_content() {
    round_trip(&Case {
        config: "",
        attributes: &[(
            ".gitattributes",
            "* text=auto\n*.bat text=auto eol=crlf\n*.sh text=auto eol=lf\n",
        )],
        info: "",
        files: SAMPLES,
    });
}

#[test]
fn core_eol_applies_to_text_paths() {
    round_trip(&Case {
        config: "[core]\n\teol = crlf\n",
        attributes: &[(".gitattributes", "* text\n*.bin -text\n*.sh eol=lf\n")],
        info: "",
        files: SAMPLES,
    });
}

#[test]
fn autocrlf_gives_way_to_attributes() {
    round_trip(&Case {
        config: "[core]\n\tautocrlf = true\n",
        attributes: &[(
            ".gitattributes",
            "*.bin binary\n*.dat -text\n*.sh eol=lf\n*.c text eol=lf\n",
        )],
        info: "",
        files: SAMPLES,
    });
}

#[test]
fn nested_files_info_macros_and_precedence() {
    round_trip(&Case {
        config: "",
        attributes: &[
            (
                ".gitattributes",
                "[attr]win text eol=crlf\n*.txt win\n*.bin -text\n",
            ),
            ("sub/.gitattributes", "*.txt -text\n/deep.txt text eol=lf\n"),
        ],
        info: "*.bat text eol=crlf\na.bin win\n",
        files: SAMPLES,
    });
}

#[test]
fn safecrlf_true_refuses_a_lossy_add() {
    round_trip(&Case {
        config: "[core]\n\tautocrlf = true\n\tsafecrlf = true\n",
        attributes: NO_ATTRIBUTES,
        info: "",
        files: &[("crlf.txt", b"a\r\nb\r\n"), ("lf.txt", b"a\nb\n")],
    });
    round_trip(&Case {
        config: "[core]\n\tautocrlf = input\n\tsafecrlf = true\n",
        attributes: NO_ATTRIBUTES,
        info: "",
        files: &[("crlf.txt", b"a\r\nb\r\n")],
    });
    round_trip(&Case {
        config: "[core]\n\tautocrlf = true\n\tsafecrlf = false\n",
        attributes: NO_ATTRIBUTES,
        info: "",
        files: SAMPLES,
    });
}

#[test]
fn an_index_with_carriage_returns_keeps_them() {
    let blobs: &[&[u8]] = &[
        b"one\r\ntwo\r\n",
        b"one\rtwo\n",
        b"one\r\n",
        b"\r",
        b"one\ntwo\r",
    ];
    let edits: &[&[u8]] = &[b"one\r\ntwo\r\nthree\r\n", b"one\r\nx\ny\n", b"a\r\n"];
    for format in FORMATS {
        for blob in blobs {
            for edit in edits {
                let case = Case {
                    config: "",
                    attributes: NO_ATTRIBUTES,
                    info: "",
                    files: &[],
                };
                let p = Pair::new(format, &case);
                for repo in [&p.oracle, &p.ours] {
                    write(repo, "a.txt", blob);
                }
                p.both(&["add", "a.txt"]);
                p.both(&["commit", "-q", "-m", "crlf in the repository"]);
                for repo in [&p.oracle, &p.ours] {
                    let mut config = fs::read_to_string(repo.join(".git/config")).unwrap();
                    config.push_str("[core]\n\tautocrlf = true\n");
                    fs::write(repo.join(".git/config"), config).unwrap();
                }
                let what = format!("{blob:?} then {edit:?}");
                p.same_status(&what);
                write(&p.oracle, "a.txt", edit);
                write(&p.ours, "a.txt", edit);
                p.same_status(&what);
                p.both(&["add", "a.txt"]);
                p.same_index(&what);
            }
        }
    }
}

#[test]
fn a_gitattributes_change_applies_to_the_files_written_after_it() {
    for format in FORMATS {
        let case = Case {
            config: "",
            attributes: NO_ATTRIBUTES,
            info: "",
            files: &[("a.txt", b"x\ny\n"), ("sub/b.txt", b"x\ny\n")],
        };
        let p = Pair::new(format, &case);
        p.both(&["add", "-A"]);
        p.both(&["commit", "-q", "-m", "plain"]);
        for repo in [&p.oracle, &p.ours] {
            run(repo, &["checkout", "-q", "-b", "crlf"]);
            write(repo, ".gitattributes", b"*.txt text eol=crlf\n");
            write(repo, "sub/.gitattributes", b"b.txt -text\n");
            run(repo, &["add", "-A"]);
            run(repo, &["commit", "-q", "-m", "attributes"]);
            run(repo, &["checkout", "-q", "main"]);
        }
        p.same_tree("main");
        p.both(&["switch", "crlf"]);
        p.same_tree("switch to crlf");
        p.same_status("status on crlf");
        p.both(&["switch", "main"]);
        p.same_tree("switch back");
        p.same_status("status on main");
    }
}

#[test]
fn the_diff_attribute_controls_binary_detection() {
    for format in FORMATS {
        let case = Case {
            config: "",
            attributes: &[(".gitattributes", "*.nodiff -diff\n*.forced diff\n")],
            info: "",
            files: &[
                ("a.nodiff", b"one\ntwo\n"),
                ("b.forced", b"one\0\ntwo\n"),
                ("c.bin", b"one\0\ntwo\n"),
            ],
        };
        let p = Pair::new(format, &case);
        p.both(&["add", "-A"]);
        p.both(&["commit", "-q", "-m", "one"]);
        for repo in [&p.oracle, &p.ours] {
            write(repo, "a.nodiff", b"one\nthree\n");
            write(repo, "b.forced", b"one\0\nthree\n");
            write(repo, "c.bin", b"one\0\nthree\n");
        }
        p.same_diff("attribute", &[]);
        p.same_diff("attribute stat", &["--numstat"]);
    }
}

#[test]
fn merge_stash_and_cherry_pick_convert_what_they_write() {
    for format in FORMATS {
        let case = Case {
            config: "[core]\n\tautocrlf = true\n[merge]\n\tconflictStyle = diff3\n",
            attributes: &[(".gitattributes", "*.bin -text\n")],
            info: "",
            files: &[
                ("a.txt", b"1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n"),
                ("b.txt", b"1\r\n2\r\n3\r\n"),
                ("c.bin", b"1\r\n2\r\n"),
            ],
        };
        let p = Pair::new(format, &case);
        p.both(&["add", "-A"]);
        p.both(&["commit", "-q", "-m", "base"]);
        for repo in [&p.oracle, &p.ours] {
            run(repo, &["checkout", "-q", "-b", "side"]);
            write(repo, "a.txt", b"1\r\n2\r\n3\r\n4\r\n5\r\nside\r\n");
            write(repo, "b.txt", b"side\r\n2\r\n3\r\n");
            write(repo, "c.bin", b"1\r\n2\r\nside\r\n");
            run(repo, &["add", "-A"]);
            run(repo, &["commit", "-q", "-m", "side"]);
            run(repo, &["checkout", "-q", "main"]);
            write(repo, "a.txt", b"main\r\n2\r\n3\r\n4\r\n5\r\n6\r\n");
            write(repo, "b.txt", b"other\r\n2\r\n3\r\n");
            run(repo, &["add", "-A"]);
            run(repo, &["commit", "-q", "-m", "main"]);
        }
        p.same_status("diverged");
        p.both(&["stash", "push"]);
        p.same_tree("stashed");
        p.both(&["stash", "pop"]);
        p.same_tree("popped");
        p.same_status("after pop");
        let (code, _) = p.both(&["merge", "side"]);
        p.same_tree("merge");
        p.same_status("after merge");
        p.same_index("merge index");
        assert_eq!(code, 1, "b.txt conflicts");
        p.both(&["merge", "--abort"]);
        p.same_tree("abort");
        p.both(&["cherry-pick", "side"]);
        p.same_tree("cherry-pick");
        p.same_status("after cherry-pick");
    }
}

#[test]
fn commit_all_ls_files_rm_and_diff_head_see_converted_content() {
    for format in FORMATS {
        let case = Case {
            config: "[core]\n\tautocrlf = true\n",
            attributes: NO_ATTRIBUTES,
            info: "",
            files: &[
                ("a.txt", b"1\r\n2\r\n"),
                ("b.txt", b"1\r\n2\r\n"),
                ("c.txt", b"1\r\n"),
            ],
        };
        let p = Pair::new(format, &case);
        p.both(&["add", "-A"]);
        p.both(&["commit", "-q", "-m", "one"]);
        for repo in [&p.oracle, &p.ours] {
            write(repo, "a.txt", b"1\r\n2\r\n3\r\n");
            write(repo, "b.txt", b"changed\r\n");
        }
        let (got, want) = (p.pyn(&["ls-files", "-m"]), p.git(&["ls-files", "-m"]));
        assert_eq!(got.1, want.1, "ls-files -m");
        p.same_diff("against HEAD", &["HEAD"]);
        p.both(&["rm", "c.txt"]);
        p.both(&["commit", "-q", "-a", "-m", "all"]);
        p.same_index("commit -a");
        p.same_status("after commit -a");
    }
}

#[test]
fn the_global_attributes_file_has_the_lowest_precedence() {
    for format in FORMATS {
        let case = Case {
            config: "",
            attributes: &[(".gitattributes", "*.dat -text\n")],
            info: "",
            files: &[("a.txt", b"x\r\ny\r\n"), ("b.dat", b"x\r\ny\r\n")],
        };
        let p = Pair::new(format, &case);
        let global = p.root.join("global-attributes");
        fs::write(&global, "* text eol=lf\n*.dat text\n").unwrap();
        for repo in [&p.oracle, &p.ours] {
            run(
                repo,
                &["config", "core.attributesFile", global.to_str().unwrap()],
            );
        }
        p.both(&["add", "-A"]);
        p.same_index("global attributes");
    }
}

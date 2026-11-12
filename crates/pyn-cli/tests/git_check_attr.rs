//! `check-attr` and `ls-files --eol` on a git repository, compared with real git on the same repository.
//! Real git is the oracle in these tests only; reports are compared, never wording.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    vars: RefCell<Vec<(String, String)>>,
}

struct Out {
    code: i32,
    stdout: Vec<u8>,
}

fn envs(c: &mut Command, root: &Path) {
    c.env_clear()
        .env("PATH", path_env())
        .env("HOME", root.join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true");
}

impl Rig {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        Self {
            _tmp: tmp,
            root,
            vars: RefCell::new(Vec::new()),
        }
    }

    fn work(&self, name: &str, format: &str, files: &[(&str, &[u8])], info: &str) -> PathBuf {
        let repo = self.root.join(name);
        init(&repo, format, false);
        run(&repo, &["config", "user.name", "t"]);
        run(&repo, &["config", "user.email", "t@example.com"]);
        for (path, bytes) in files {
            let file = repo.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, bytes).unwrap();
        }
        fs::write(repo.join(".git/info/attributes"), info).unwrap();
        repo
    }

    fn exec(&self, mut c: Command, dir: &Path, args: &[&str], stdin: &[u8]) -> Out {
        envs(&mut c, &self.root);
        c.envs(self.vars.borrow().iter().map(|(k, v)| (k, v)));
        let mut child = c
            .current_dir(dir)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).ok();
        let out = child.wait_with_output().unwrap();
        Out {
            code: out.status.code().unwrap(),
            stdout: out.stdout,
        }
    }

    fn pyn(&self, dir: &Path, args: &[&str], stdin: &[u8]) -> Out {
        self.exec(Command::new(env!("CARGO_BIN_EXE_pyn")), dir, args, stdin)
    }

    fn git(&self, dir: &Path, args: &[&str], stdin: &[u8]) -> Out {
        self.exec(Command::new("git"), dir, args, stdin)
    }

    /// Sets (or with `None` removes) an environment variable for the tools run after this.
    fn var(&self, key: &str, value: Option<&str>) {
        let mut vars = self.vars.borrow_mut();
        vars.retain(|(k, _)| k != key);
        if let Some(v) = value {
            vars.push((key.to_owned(), v.to_owned()));
        }
    }

    /// Runs both tools; the exit class must match, and the output too (`-a` lines compared as a set).
    fn same(&self, dir: &Path, args: &[&str], stdin: &[u8]) -> Out {
        let (got, want) = (self.pyn(dir, args, stdin), self.git(dir, args, stdin));
        assert_eq!(
            oracle::status_class(got.code),
            oracle::status_class(want.code),
            "{args:?}"
        );
        if want.code == 0 {
            assert_eq!(
                canon(&got.stdout, args),
                canon(&want.stdout, args),
                "{args:?}"
            );
        }
        want
    }
}

/// Records sorted within a path, because git's order for `-a` is not part of the contract.
fn canon(bytes: &[u8], args: &[&str]) -> Vec<Vec<u8>> {
    let nul = args.contains(&"-z") && args.contains(&"check-attr");
    let sep = if nul { 0 } else { b'\n' };
    let mut records: Vec<Vec<u8>> = bytes.split(|b| *b == sep).map(<[u8]>::to_vec).collect();
    if args.contains(&"--all") || args.contains(&"-a") {
        records.sort();
    }
    records
}

const FORMATS: [&str; 2] = ["sha1", "sha256"];

const ROOT_ATTRS: &[u8] = b"[attr]mine -text eol=crlf\n\
*.txt text eol=crlf\n*.bin binary\nx.txt -text foo=bar\n*.m mine\nq* !text\n*.a text=auto\n*.b -text\n\
*.c text eol=lf\n*.d text=auto eol=lf\n*.e text=input\n*.f crlf\n*.g -crlf\n*.h crlf=input\n*.i eol=crlf\n\
*.j text=auto eol=crlf\n\"sp ace.txt\" special\n";

fn sample_files() -> Vec<(&'static str, &'static [u8])> {
    vec![
        (".gitattributes", ROOT_ATTRS),
        ("sub/.gitattributes", b"*.txt eol=lf\n*.zz zed=1\n"),
        ("a.txt", b"one\n"),
        ("sub/a.txt", b"two\n"),
        ("sp ace.txt", b"x\n"),
    ]
}

const PATHS: &[&str] = &[
    "a.txt",
    "x.txt",
    "a.bin",
    "f.m",
    "q.txt",
    "sub/a.txt",
    "sub/deep/b.zz",
    "sp ace.txt",
    "none.xyz",
    "k.a",
    "k.i",
];

#[test]
fn check_attr_reports_match_git() {
    for format in FORMATS {
        let rig = Rig::new();
        let repo = rig.work("r", format, &sample_files(), "sub/info.txt info=yes\n");
        let sub = repo.join("sub");
        let mut named = vec!["check-attr", "text", "eol", "foo", "info", "--"];
        named.extend_from_slice(PATHS);
        rig.same(&repo, &named, b"");
        let mut all = vec!["check-attr", "-a"];
        all.extend_from_slice(PATHS);
        rig.same(&repo, &all, b"");
        let mut long = vec!["check-attr", "--all", "--"];
        long.extend_from_slice(PATHS);
        rig.same(&repo, &long, b"");
        rig.same(
            &repo,
            &["check-attr", "-z", "-a", "x.txt", "sub/info.txt"],
            b"",
        );
        rig.same(&repo, &["check-attr", "-z", "text", "--", "a.txt"], b"");
        rig.same(&repo, &["check-attr", "text", "a.txt", "x.txt"], b"");
        rig.same(
            &sub,
            &["check-attr", "-a", "a.txt", "../x.txt", "info.txt"],
            b"",
        );
        rig.same(&sub, &["check-attr", "eol", "--", "a.txt", "../a.txt"], b"");
        rig.same(&repo, &["check-attr", "text", "--", "sub/../a.txt"], b"");
        let abs = repo.join("sub/a.txt");
        rig.same(
            &repo,
            &["check-attr", "eol", "--", abs.to_str().unwrap()],
            b"",
        );
    }
}

#[test]
fn check_attr_stdin() {
    let rig = Rig::new();
    let repo = rig.work("r", "sha1", &sample_files(), "");
    let lines = "a.txt\nsub/a.txt\n\"sp ace.txt\"\nx.txt\n";
    rig.same(
        &repo,
        &["check-attr", "--stdin", "text", "eol"],
        lines.as_bytes(),
    );
    rig.same(&repo, &["check-attr", "--stdin", "-a"], lines.as_bytes());
    let nul = b"a.txt\0sub/a.txt\0sp ace.txt\0";
    rig.same(&repo, &["check-attr", "--stdin", "-z", "text"], nul);
    rig.same(&repo, &["check-attr", "--stdin", "-z", "-a"], nul);
    rig.same(&repo, &["check-attr", "--stdin", "-z", "--all"], b"");
    rig.same(
        &repo.join("sub"),
        &["check-attr", "--stdin", "eol"],
        b"a.txt\n../a.txt\n",
    );
}

#[test]
fn check_attr_usage_errors_exit_like_git() {
    let rig = Rig::new();
    let repo = rig.work("r", "sha1", &sample_files(), "");
    let cases: &[&[&str]] = &[
        &["check-attr"],
        &["check-attr", "-a"],
        &["check-attr", "text"],
        &["check-attr", "--", "a.txt"],
        &["check-attr", "text", "--"],
        &["check-attr", "text", "a.txt", "--"],
        &["check-attr", "-a", "a.txt", "--"],
        &["check-attr", "-a", "text", "--", "a.txt"],
        &["check-attr", "--stdin", "text", "--", "a.txt"],
        &["check-attr", "bad name", "a.txt"],
        &["check-attr", "-x", "a.txt"],
        &["check-attr", "--source", "nonexistent", "text", "a.txt"],
    ];
    for args in cases {
        let (got, want) = (rig.pyn(&repo, args, b""), rig.git(&repo, args, b""));
        assert_eq!(
            oracle::status_class(got.code),
            oracle::status_class(want.code),
            "{args:?}"
        );
        assert_ne!(got.code, 0, "{args:?}");
    }
}

#[test]
fn check_attr_reads_the_index_and_a_tree() {
    for format in FORMATS {
        let rig = Rig::new();
        let repo = rig.work("r", format, &sample_files(), "");
        run(&repo, &["add", "-A"]);
        run(&repo, &["commit", "-q", "-m", "one"]);
        fs::write(repo.join("sub/.gitattributes"), "*.txt eol=crlf bar\n").unwrap();
        run(&repo, &["add", "sub/.gitattributes"]);
        run(&repo, &["commit", "-q", "-m", "two"]);
        fs::write(repo.join("sub/.gitattributes"), "*.txt work\n").unwrap();
        fs::write(repo.join(".gitattributes"), "*.txt work2\n").unwrap();
        run(&repo, &["add", ".gitattributes"]);
        fs::write(repo.join(".gitattributes"), "*.txt work3\n").unwrap();
        let sub = repo.join("sub");
        for extra in [
            vec![],
            vec!["--cached"],
            vec!["--source", "HEAD"],
            vec!["--source=HEAD~1"],
            vec!["--source", "HEAD~1", "--cached"],
            vec!["--source", "HEAD^{tree}"],
            vec!["--source", "HEAD:sub"],
        ] {
            for dir in [&repo, &sub] {
                let mut args = vec!["check-attr"];
                args.extend(&extra);
                args.extend(["-a", "--", "a.txt", "x.txt"]);
                rig.same(dir, &args, b"");
            }
        }
    }
}

#[test]
fn check_attr_in_a_bare_repository_reads_head() {
    for format in FORMATS {
        let rig = Rig::new();
        let repo = rig.work("r", format, &sample_files(), "");
        run(&repo, &["add", "-A"]);
        run(&repo, &["commit", "-q", "-m", "one"]);
        let bare = rig.root.join("b.git");
        run(
            &rig.root,
            &[
                "clone",
                "-q",
                "--bare",
                repo.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        fs::write(bare.join("info/attributes"), "*.txt late=1\n").unwrap();
        // Git versions differ on reading HEAD by default in a bare repository, so compare only the explicit form.
        let mut args = vec!["check-attr", "-a", "--source", "HEAD", "--"];
        args.extend(PATHS);
        rig.same(&bare, &args, b"");
        let mut default = vec!["check-attr", "-a", "--"];
        default.extend(PATHS);
        assert_eq!(rig.pyn(&bare, &default, b"").code, 0);
        rig.same(
            &bare,
            &["check-attr", "--source", "HEAD", "eol", "--", "a.txt"],
            b"",
        );

        let empty = rig.root.join("e.git");
        init(&empty, format, true);
        rig.same(&empty, &["check-attr", "-a", "--", "a.txt"], b"");
    }
}

#[test]
fn check_attr_ignores_a_symlinked_attributes_file() {
    let rig = Rig::new();
    let repo = rig.work(
        "r",
        "sha1",
        &[("real", b"*.txt eol=crlf\n"), ("a.txt", b"x\n")],
        "",
    );
    std::os::unix::fs::symlink("real", repo.join(".gitattributes")).unwrap();
    run(&repo, &["add", "-A"]);
    run(&repo, &["commit", "-q", "-m", "one"]);
    for extra in [vec![], vec!["--cached"], vec!["--source", "HEAD"]] {
        let mut args = vec!["check-attr"];
        args.extend(&extra);
        args.extend(["-a", "--", "a.txt"]);
        let out = rig.same(&repo, &args, b"");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

const EOL_FILES: &[(&str, &[u8])] = &[
    (".gitattributes", ROOT_ATTRS),
    ("lf.txt", b"a\nb\n"),
    ("crlf.txt", b"a\r\nb\r\n"),
    ("mixed.txt", b"a\r\nb\nc\r\n"),
    ("nul.c", b"a\0\r\nb\n"),
    ("lone.a", b"a\rb\n"),
    ("none.b", b"no newline"),
    ("sub/deep.txt", b"x\r\ny\r\n"),
    ("empty.d", b""),
    ("x.e", b"x\n"),
    ("y.f", b"x\r\n"),
    ("y.g", b"x\r\n"),
    ("y.h", b"x\r\n"),
    ("y.i", b"x\r\n"),
    ("y.j", b"x\r\n"),
    ("plain.xyz", b"x\r\ny\n"),
    ("sub/plain.xyz", b"x\n"),
];

#[test]
fn ls_files_eol_matches_git() {
    for (config, format) in [
        ("", "sha1"),
        ("", "sha256"),
        ("[core]\n\tautocrlf = true\n", "sha1"),
        ("[core]\n\tautocrlf = input\n", "sha1"),
        ("[core]\n\teol = crlf\n", "sha1"),
    ] {
        let rig = Rig::new();
        let repo = rig.work("r", format, EOL_FILES, "sub/*.xyz text\n");
        let mut text = fs::read_to_string(repo.join(".git/config")).unwrap();
        text.push_str(config);
        text.push_str("[core]\n\tsafecrlf = false\n");
        fs::write(repo.join(".git/config"), text).unwrap();
        run(&repo, &["add", "-A"]);
        fs::write(repo.join("lf.txt"), b"a\r\nb\r\nc\r\n").unwrap();
        fs::write(repo.join("sub/plain.xyz"), b"x\r\nq\n").unwrap();
        fs::remove_file(repo.join("none.b")).unwrap();
        fs::write(repo.join("new.txt"), b"n\r\n").unwrap();
        std::os::unix::fs::symlink("lf.txt", repo.join("link")).unwrap();
        run(&repo, &["add", "link"]);
        let sub = repo.join("sub");
        for dir in [&repo, &sub] {
            for args in [
                vec!["ls-files", "--eol"],
                vec!["ls-files", "--eol", "-s"],
                vec!["ls-files", "--eol", "-t"],
                vec!["ls-files", "--eol", "-v"],
                vec!["ls-files", "--eol", "-o"],
                vec!["ls-files", "--eol", "-m", "-d"],
                vec!["ls-files", "--eol", "-z"],
                vec!["ls-files", "--eol", "--full-name", "-c", "-o"],
            ] {
                let (got, want) = (rig.pyn(dir, &args, b""), rig.git(dir, &args, b""));
                assert_eq!(got.code, want.code, "{args:?}");
                assert_eq!(
                    String::from_utf8_lossy(&got.stdout),
                    String::from_utf8_lossy(&want.stdout),
                    "{args:?} in {} with {config:?}",
                    dir.display()
                );
            }
        }
    }
}

#[test]
fn ls_files_eol_shows_unmerged_entries() {
    let rig = Rig::new();
    let repo = rig.work("r", "sha1", &[("f.txt", b"base\n")], "");
    run(&repo, &["add", "-A"]);
    run(&repo, &["commit", "-q", "-m", "base"]);
    run(&repo, &["checkout", "-q", "-b", "side"]);
    fs::write(repo.join("f.txt"), b"side\r\n").unwrap();
    run(&repo, &["commit", "-q", "-am", "side"]);
    run(&repo, &["checkout", "-q", "-"]);
    fs::write(repo.join("f.txt"), b"main\n").unwrap();
    run(&repo, &["commit", "-q", "-am", "main"]);
    let merge = rig.git(&repo, &["merge", "side"], b"");
    assert_ne!(merge.code, 0);
    for args in [
        vec!["ls-files", "--eol", "-u"],
        vec!["ls-files", "--eol", "-s"],
    ] {
        let (got, want) = (rig.pyn(&repo, &args, b""), rig.git(&repo, &args, b""));
        assert_eq!(got.stdout, want.stdout, "{args:?}");
    }
}

#[test]
fn blame_and_no_index_diff_convert_work_tree_files() {
    for config in ["", "[core]\n\tautocrlf = true\n"] {
        let rig = Rig::new();
        let files: &[(&str, &[u8])] = &[
            (".gitattributes", b"*.txt text eol=crlf\n*.q text\n"),
            ("f.txt", b"a\nb\n"),
            ("g.q", b"a\nb\n"),
            ("h.plain", b"a\nb\n"),
        ];
        let repo = rig.work("r", "sha1", files, "");
        let mut text = fs::read_to_string(repo.join(".git/config")).unwrap();
        text.push_str(config);
        fs::write(repo.join(".git/config"), text).unwrap();
        run(&repo, &["add", "-A"]);
        run(&repo, &["commit", "-q", "-m", "one"]);
        for (path, body) in [
            ("f.txt", &b"a\r\nb\r\nc\r\n"[..]),
            ("g.q", b"a\r\nb\r\nc\r\n"),
            ("h.plain", b"a\r\nb\r\nc\r\n"),
        ] {
            fs::write(repo.join(path), body).unwrap();
            let args = ["blame", "-s", path];
            let (got, want) = (rig.pyn(&repo, &args, b""), rig.git(&repo, &args, b""));
            assert_eq!(got.code, want.code, "{args:?}");
            assert_eq!(
                oracle::blame_lines(&got.stdout),
                oracle::blame_lines(&want.stdout),
                "{args:?} {config:?}"
            );
        }
        fs::write(repo.join("c.txt"), b"a\r\nb\r\n").unwrap();
        fs::write(repo.join("d.txt"), b"a\nb\nz\n").unwrap();
        fs::write(repo.join("c.q"), b"a\r\nb\r\n").unwrap();
        fs::write(repo.join("d.q"), b"a\nb\nz\n").unwrap();
        for args in [
            vec!["diff", "--no-index", "c.txt", "d.txt"],
            vec!["diff", "--no-index", "c.q", "d.q"],
            vec!["diff", "--no-index", "--raw", "c.txt", "d.txt"],
            vec!["diff", "--no-index", "--stat", "c.txt", "d.txt"],
            vec!["diff", "--no-index", "--exit-code", "c.txt", "f.txt"],
            vec!["diff", "--no-index", "f.txt", "h.plain"],
        ] {
            let (got, want) = (rig.pyn(&repo, &args, b""), rig.git(&repo, &args, b""));
            assert_eq!(got.code, want.code, "{args:?}");
            assert_eq!(
                String::from_utf8_lossy(&got.stdout),
                String::from_utf8_lossy(&want.stdout),
                "{args:?} {config:?}"
            );
        }
    }
}

fn two_commits(rig: &Rig, format: &str) -> PathBuf {
    let repo = rig.work("r", format, &sample_files(), "");
    run(&repo, &["add", "-A"]);
    run(&repo, &["commit", "-q", "-m", "one"]);
    fs::write(repo.join("sub/.gitattributes"), "*.txt eol=crlf bar\n").unwrap();
    run(&repo, &["add", "sub/.gitattributes"]);
    run(&repo, &["commit", "-q", "-m", "two"]);
    fs::write(repo.join("sub/.gitattributes"), "*.txt work\n").unwrap();
    repo
}

#[test]
fn git_attr_source_and_attr_tree_pick_the_tree_attributes_are_read_from() {
    for format in FORMATS {
        let rig = Rig::new();
        let repo = two_commits(&rig, format);
        let query = ["check-attr", "-a", "--", "a.txt", "sub/a.txt", "x.txt"];
        let eol = ["ls-files", "--eol", "sub/a.txt"];
        for tree in ["HEAD", "HEAD~1", "HEAD^{tree}", "HEAD~1:sub"] {
            rig.var("GIT_ATTR_SOURCE", Some(tree));
            rig.same(&repo, &query, b"");
            rig.same(&repo, &eol, b"");
            rig.same(&repo.join("sub"), &["check-attr", "-a", "a.txt"], b"");
        }
        // The option beats the variable, and the variable beats the setting.
        rig.var("GIT_ATTR_SOURCE", Some("HEAD~1"));
        rig.same(
            &repo,
            &["check-attr", "--source", "HEAD", "-a", "a.txt"],
            b"",
        );
        run(&repo, &["config", "attr.tree", "HEAD"]);
        rig.same(&repo, &query, b"");
        rig.var("GIT_ATTR_SOURCE", None);
        rig.same(&repo, &query, b"");
        rig.same(&repo, &eol, b"");
        run(&repo, &["config", "attr.tree", "HEAD~1"]);
        rig.same(&repo, &query, b"");
        rig.same(&repo, &["check-attr", "--cached", "-a", "a.txt"], b"");
        // A name that resolves to nothing is an error from the variable and ignored from the setting.
        run(&repo, &["config", "attr.tree", "nonesuch"]);
        let out = rig.same(&repo, &query, b"");
        assert!(String::from_utf8_lossy(&out.stdout).contains("work"));
        run(&repo, &["config", "--unset", "attr.tree"]);
        for bad in ["nonesuch", ""] {
            rig.var("GIT_ATTR_SOURCE", Some(bad));
            let (got, want) = (rig.pyn(&repo, &query, b""), rig.git(&repo, &query, b""));
            assert_eq!(want.code, 128, "{bad:?}");
            assert_eq!(got.code, want.code, "{bad:?}");
        }
    }
}

#[test]
fn a_bare_repository_with_an_unreadable_head_fails_like_git() {
    for format in FORMATS {
        let rig = Rig::new();
        let bare = rig.root.join("b.git");
        init(&bare, format, true);
        fs::write(bare.join("HEAD"), "not a ref\n").unwrap();
        for args in [
            vec!["check-attr", "-a", "--", "a.txt"],
            vec!["check-attr", "--source", "HEAD", "-a", "--", "a.txt"],
        ] {
            let (got, want) = (rig.pyn(&bare, &args, b""), rig.git(&bare, &args, b""));
            assert_eq!(want.code, 128, "{args:?}");
            assert_eq!(got.code, want.code, "{args:?}");
        }
    }
}

#[test]
fn no_index_diff_outside_the_work_tree_applies_the_global_autocrlf() {
    for format in FORMATS {
        for autocrlf in ["true", "input", "false"] {
            let rig = Rig::new();
            fs::write(
                rig.root.join("home/.gitconfig"),
                format!("[core]\n\tautocrlf = {autocrlf}\n\tsafecrlf = false\n"),
            )
            .unwrap();
            let outside = rig.root.join("out");
            fs::create_dir_all(&outside).unwrap();
            fs::write(outside.join("c"), b"a\r\nb\r\n").unwrap();
            fs::write(outside.join("d"), b"a\nb\nz\n").unwrap();
            fs::write(outside.join("e"), b"a\nb\r\n").unwrap();
            let repo = rig.work("r", format, &[("f", b"x\n")], "");
            for dir in [&outside, &repo] {
                for args in [
                    vec!["diff", "--no-index", "c", "d"],
                    vec!["diff", "--no-index", "--exit-code", "c", "e"],
                    vec!["diff", "--no-index", "--raw", "c", "d"],
                ] {
                    let (a, b) = if *dir == repo {
                        (
                            format!("../out/{}", args[args.len() - 2]),
                            format!("../out/{}", args[args.len() - 1]),
                        )
                    } else {
                        (
                            args[args.len() - 2].to_owned(),
                            args[args.len() - 1].to_owned(),
                        )
                    };
                    let mut args = args[..args.len() - 2].to_vec();
                    args.extend([a.as_str(), b.as_str()]);
                    let (got, want) = (rig.pyn(dir, &args, b""), rig.git(dir, &args, b""));
                    assert_eq!(got.code, want.code, "{args:?} {autocrlf}");
                    assert_eq!(
                        String::from_utf8_lossy(&got.stdout),
                        String::from_utf8_lossy(&want.stdout),
                        "{args:?} {autocrlf} in {}",
                        dir.display()
                    );
                }
            }
        }
    }
}

#[test]
fn a_bad_attr_source_is_fatal_for_diff_as_it_is_in_git() {
    for format in FORMATS {
        let rig = Rig::new();
        let repo = two_commits(&rig, format);
        fs::write(repo.join("a.txt"), b"changed\n").unwrap();
        rig.var("GIT_ATTR_SOURCE", Some("nonesuch"));
        for args in [
            vec!["diff"],
            vec!["diff", "--stat"],
            vec!["diff", "HEAD~1", "HEAD"],
            vec!["diff", "--stat", "HEAD~1", "HEAD"],
            vec!["diff", "--check", "HEAD~1", "HEAD"],
            vec!["diff", "--name-only", "HEAD~1", "HEAD"],
        ] {
            let (got, want) = (rig.pyn(&repo, &args, b""), rig.git(&repo, &args, b""));
            assert_eq!(
                oracle::status_class(got.code),
                oracle::status_class(want.code),
                "{args:?}"
            );
        }
        rig.var("GIT_ATTR_SOURCE", None);
        let ok = rig.pyn(&repo, &["diff", "HEAD~1", "HEAD"], b"");
        assert_eq!(ok.code, 0);
    }
}

#[test]
fn a_bare_repository_reads_attributes_from_head_when_git_does() {
    for format in FORMATS {
        let rig = Rig::new();
        let repo = rig.work("r", format, &sample_files(), "");
        run(&repo, &["add", "-A"]);
        run(&repo, &["commit", "-q", "-m", "one"]);
        let bare = rig.root.join("b.git");
        init(&bare, format, true);
        let branch = run(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let branch = branch.trim();
        run(&repo, &["push", "-q", bare.to_str().unwrap(), branch]);
        run(
            &bare,
            &["symbolic-ref", "HEAD", &format!("refs/heads/{branch}")],
        );
        let args = ["check-attr", "-a", "--", "a.txt", "sub/a.txt"];
        if rig.git(&bare, &args, b"").stdout.is_empty() {
            eprintln!("skipped: this git does not read HEAD's attributes in a bare repository");
            return;
        }
        rig.same(&bare, &args, b"");
    }
}

#[test]
fn no_index_diff_applies_the_global_attributes_file() {
    for format in FORMATS {
        let rig = Rig::new();
        let global = rig.root.join("home/attrs");
        fs::write(&global, "*.x -diff\n").unwrap();
        fs::write(
            rig.root.join("home/.gitconfig"),
            format!("[core]\n\tattributesFile = {}\n", global.display()),
        )
        .unwrap();
        let outside = rig.root.join("out");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("p.x"), b"a\n").unwrap();
        fs::write(outside.join("q.x"), b"b\n").unwrap();
        fs::write(outside.join("p.y"), b"a\n").unwrap();
        fs::write(outside.join("q.y"), b"b\n").unwrap();
        let repo = rig.work("r", format, &[("f", b"x\n")], "");
        for (dir, prefix) in [(&outside, ""), (&repo, "../out/")] {
            for ext in ["x", "y"] {
                let (a, b) = (format!("{prefix}p.{ext}"), format!("{prefix}q.{ext}"));
                let args = ["diff", "--no-index", a.as_str(), b.as_str()];
                let (got, want) = (rig.pyn(dir, &args, b""), rig.git(dir, &args, b""));
                assert_eq!(got.code, want.code, "{args:?}");
                let binary = |o: &Out| String::from_utf8_lossy(&o.stdout).contains("Binary files");
                assert_eq!(binary(&got), binary(&want), "{args:?} in {}", dir.display());
                assert_eq!(binary(&want), ext == "x");
            }
        }
    }
}

/// A repository with `*.t text` whose index git wrote: `size.t` is rewritten with CRLF, `same.t` with a new
/// mtime only, `edit.t` with other bytes of the same size.
fn stat_repo(rig: &Rig, format: &str, changed: &[&str]) -> PathBuf {
    let files: [(&str, &[u8]); 4] = [
        (".gitattributes", b"*.t text\n"),
        ("size.t", b"a\nb\n"),
        ("same.t", b"a\nb\n"),
        ("edit.t", b"x\ny\n"),
    ];
    let repo = rig.work("r", format, &files, "");
    // Old mtimes keep every entry from being racily clean, which makes git compare content.
    let stamp = |name: &str, age: u64| {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age);
        let file = fs::File::options()
            .write(true)
            .open(repo.join(name))
            .unwrap();
        file.set_modified(when).unwrap();
    };
    for (name, _) in files {
        stamp(name, 7200);
    }
    run(&repo, &["add", "-A"]);
    run(&repo, &["commit", "-q", "-m", "one"]);
    for name in changed {
        let bytes: &[u8] = match *name {
            "size.t" => b"a\r\nb\r\n",
            "same.t" => b"a\nb\n",
            _ => b"x\nz\n",
        };
        fs::write(repo.join(name), bytes).unwrap();
        stamp(name, 3600);
    }
    repo
}

#[test]
fn ls_files_modified_lists_a_size_change_without_comparing_converted_content() {
    for format in FORMATS {
        let rig = Rig::new();
        let repo = stat_repo(&rig, format, &["size.t", "same.t", "edit.t"]);
        let out = rig.same(&repo, &["ls-files", "-m"], b"");
        let listed = String::from_utf8_lossy(&out.stdout).into_owned();
        assert_eq!(listed, "edit.t\nsize.t\n", "{format}");
        rig.same(&repo, &["ls-files", "-m", "-t"], b"");
        rig.same(&repo, &["status", "--short"], b"");
    }
}

#[test]
fn a_bad_attr_source_is_fatal_only_where_git_reads_attributes() {
    for format in FORMATS {
        let rig = Rig::new();
        let size = stat_repo(&rig, format, &["size.t"]);
        rig.var("GIT_ATTR_SOURCE", Some("nonesuch"));
        for args in [vec!["ls-files", "-m"], vec!["status", "--short"]] {
            let out = rig.same(&size, &args, b"");
            assert_eq!(out.code, 0, "{args:?}");
        }
        let rig = Rig::new();
        let repo = stat_repo(&rig, format, &["same.t"]);
        rig.var("GIT_ATTR_SOURCE", Some("nonesuch"));
        for args in [
            vec!["ls-files", "-m"],
            vec!["status", "--short"],
            vec!["ls-files", "--eol"],
            vec!["format-patch", "-1", "--stdout"],
            vec!["grep", "-c", "a"],
        ] {
            let (got, want) = (rig.pyn(&repo, &args, b""), rig.git(&repo, &args, b""));
            assert_eq!(
                oracle::status_class(got.code),
                oracle::status_class(want.code),
                "{args:?} {format}"
            );
        }
    }
}

#[test]
fn no_index_diff_outside_the_work_tree_applies_global_line_ending_attributes() {
    for format in FORMATS {
        for attrs in [
            "*.x text",
            "*.x eol=crlf",
            "*.x text=auto",
            "*.x -text",
            "*.x crlf",
        ] {
            let rig = Rig::new();
            let global = rig.root.join("home/attrs");
            fs::write(&global, format!("{attrs}\n")).unwrap();
            fs::write(
                rig.root.join("home/.gitconfig"),
                format!(
                    "[core]\n\tattributesFile = {}\n\tsafecrlf = false\n",
                    global.display()
                ),
            )
            .unwrap();
            let outside = rig.root.join("out");
            fs::create_dir_all(&outside).unwrap();
            for (name, bytes) in [
                ("a.x", &b"p\r\nq\r\n"[..]),
                ("b.x", b"p\nq\n"),
                ("c.x", b"p\nz\n"),
            ] {
                fs::write(outside.join(name), bytes).unwrap();
            }
            let repo = rig.work("r", format, &[("f", b"x\n")], "");
            for (dir, prefix) in [(&outside, ""), (&repo, "../out/")] {
                for (a, b) in [("a.x", "b.x"), ("a.x", "c.x")] {
                    let (a, b) = (format!("{prefix}{a}"), format!("{prefix}{b}"));
                    let args = ["diff", "--no-index", a.as_str(), b.as_str()];
                    let (got, want) = (rig.pyn(dir, &args, b""), rig.git(dir, &args, b""));
                    assert_eq!(got.code, want.code, "{args:?} {attrs}");
                    assert_eq!(
                        String::from_utf8_lossy(&got.stdout),
                        String::from_utf8_lossy(&want.stdout),
                        "{args:?} {attrs} in {}",
                        dir.display()
                    );
                }
            }
        }
    }
}

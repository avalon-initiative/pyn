//! `pyn grep` on a git repository, compared with the real `git grep` on the same repository (read-only, so one copy
//! serves both): standard output byte for byte, the exit status, and standard error where git's message is matched.
//! Needs `git` on the PATH (tests only); only semantics both git 2.43 and newer releases agree on are compared.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn command(&self, program: &str, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .env("TZ", "UTC")
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .env_remove("PYN_DIR")
            .envs(IDENTITY);
        cmd
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self
            .command("git", &self.root, args)
            .output()
            .expect("git is installed");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn write(&self, name: &str, content: &[u8]) {
        let file = self.root.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, content).unwrap();
    }

    fn run(&self, program: &str, cwd: &str, args: &[String]) -> Output {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let program = if program == "pyn" {
            env!("CARGO_BIN_EXE_pyn")
        } else {
            program
        };
        self.command(program, &self.root.join(cwd), &refs)
            .output()
            .unwrap()
    }
}

const RX: &str = "a\nab\nabab\na{\na{1\n*a\n^a\na^b\na$b\n(x)\nx)\n|\na|b\n+\n?x\n{1}\n[x]\na.b\na\\b\nab+\n\tsp\nwor_d\n]\n-\na-b\nfoo bar\nFOO\n\n  indented\n";

/// A repository with committed, staged, modified, deleted, untracked and ignored files, in one object format.
fn fixture(format: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("repo");
    let home = tmp.path().canonicalize().unwrap().join("home");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&home).unwrap();
    let f = Fixture {
        _tmp: tmp,
        root,
        home,
    };
    f.git(&[
        "init",
        "-q",
        "-b",
        "main",
        &format!("--object-format={format}"),
        ".",
    ]);
    f.write(
        "a.txt",
        b"hello world\nfoo bar\nHello again\nwordy word\nfoo\n",
    );
    f.write("rx.txt", RX.as_bytes());
    f.write("sub/b.txt", b"foo in sub\nnothing\nFoo Bar\n");
    f.write("sub/deep/c.txt", b"deep foo\n");
    f.write("sub/deep/d.log", b"deep foo log\n");
    f.write("bin.dat", b"bin\0foo\nplain foo\n");
    f.write("bin2.dat", b"clean text foo\n");
    f.write("nonl.txt", b"no newline foo");
    f.write("crlf.txt", b"x\r\ncrlf foo\r\n\r\n");
    f.write("empty.txt", b"");
    f.write("blank.txt", b"\n\nfoo\n\n");
    f.write("end.txt", b"a\n\nb\n");
    f.write("end2.txt", b"b\n");
    f.write("uni.txt", "caf\u{e9} foo\nna\u{ef}ve\n".as_bytes());
    f.write("sp ace.txt", b"space foo\n");
    f.write("q\"uote.txt", b"quote foo\n");
    f.write("run.sh", b"#!/bin/sh\necho foo\n");
    f.write(".gitignore", b"ignored.txt\n");
    f.write(
        "long.txt",
        format!("{} foo\n", "x".repeat(20000)).as_bytes(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let run = f.root.join("run.sh");
        let mut perm = fs::metadata(&run).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(run, perm).unwrap();
        std::os::unix::fs::symlink("a.txt", f.root.join("link")).unwrap();
    }
    f.git(&["add", "-A"]);
    f.git(&["commit", "-qm", "one"]);
    let first = f.git(&["rev-parse", "HEAD"]);
    f.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{first},module"),
    ]);
    f.write(
        "a.txt",
        b"hello world\nfoo bar\nHello again\nwordy word\nfoo\nfoo changed\nnew line\n",
    );
    f.write("late.txt", b"late foo\n");
    f.git(&["add", "late.txt", "a.txt"]);
    f.git(&["commit", "-qm", "two"]);
    f.git(&["tag", "light"]);
    f.git(&["tag", "-a", "-m", "annotated", "annot"]);
    f.git(&["branch", "side", "HEAD~1"]);
    f.write("staged.txt", b"staged foo\n");
    f.git(&["add", "staged.txt"]);
    f.write("sub/b.txt", b"staged sub foo\n");
    f.git(&["add", "sub/b.txt"]);
    f.write("sub/b.txt", b"modified sub foo\nin work tree\n");
    f.write("a.txt", b"worktree foo only\nfoo\n");
    fs::remove_file(f.root.join("late.txt")).unwrap();
    f.write("untracked.txt", b"untracked foo\n");
    f.write("ignored.txt", b"ignored foo\n");
    f.write("added.txt", b"intent foo\n");
    f.git(&["add", "-N", "added.txt"]);
    f
}

struct Case {
    cwd: &'static str,
    args: Vec<&'static str>,
    stderr: bool,
}

fn at(cwd: &'static str, line: &'static str) -> Case {
    Case {
        cwd,
        args: line.split(' ').collect(),
        stderr: false,
    }
}

fn case(line: &'static str) -> Case {
    at("", line)
}

/// A case whose standard error must also be git's.
fn msg(line: &'static str) -> Case {
    Case {
        stderr: true,
        ..at("", line)
    }
}

fn expand(f: &Fixture, args: &[&str]) -> Vec<String> {
    if !args.iter().any(|a| a.contains('{')) {
        return args.iter().map(ToString::to_string).collect();
    }
    let head = f.git(&["rev-parse", "HEAD"]);
    args.iter()
        .map(|a| a.replace("{head}", &head).replace("{short}", &head[..8]))
        .collect()
}

fn compare(f: &Fixture, c: &Case) -> Option<String> {
    let args: Vec<String> = [vec!["grep".to_string()], expand(f, &c.args)].concat();
    let git = f.run("git", c.cwd, &args);
    let pyn = f.run("pyn", c.cwd, &args);
    let mut why = Vec::new();
    if git.status.code() != pyn.status.code() {
        why.push(format!(
            "status git {:?} pyn {:?}",
            git.status.code(),
            pyn.status.code()
        ));
    }
    if git.stdout != pyn.stdout {
        why.push(format!(
            "stdout\n  git: {:?}\n  pyn: {:?}",
            String::from_utf8_lossy(&git.stdout),
            String::from_utf8_lossy(&pyn.stdout)
        ));
    }
    if c.stderr && git.stderr != pyn.stderr {
        why.push(format!(
            "stderr\n  git: {:?}\n  pyn: {:?}",
            String::from_utf8_lossy(&git.stderr),
            String::from_utf8_lossy(&pyn.stderr)
        ));
    }
    (!why.is_empty()).then(|| format!("{} [in {:?}]: {}", c.args.join(" "), c.cwd, why.join("; ")))
}

fn check(f: &Fixture, cases: Vec<Case>) {
    let failures: Vec<String> = cases.iter().filter_map(|c| compare(f, c)).collect();
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

fn matching_cases() -> Vec<Case> {
    vec![
        case("foo"),
        case("-n foo"),
        case("-i hello"),
        case("-w word"),
        case("-w wor"),
        case("-wi HELLO"),
        case("-v foo -- a.txt"),
        case("-nv foo -- a.txt sub"),
        case("-c foo"),
        case("-cv foo -- a.txt"),
        case("-c zzzz"),
        case("-l foo"),
        case("-l zzzz"),
        case("-lc foo -- a.txt"),
        case("-ln foo -- a.txt"),
        case("-h foo -- a.txt sub"),
        case("-hn foo -- sub"),
        case("-hc foo -- a.txt sub"),
        case("-H foo a.txt"),
        case("-hH foo a.txt"),
        case("-Hh foo a.txt"),
        case("-q foo"),
        case("-q zzzz"),
        case("-qv zzzz -- a.txt"),
        case("-e foo -e hello"),
        case("-n -e foo -e hello -- a.txt"),
        case("-efoo -- a.txt"),
        case("-ne foo -- a.txt"),
        case("-ie HELLO -- a.txt"),
        case("--line-number foo -- a.txt"),
        case("--count foo -- a.txt"),
        case("--name-only foo -- a.txt"),
        case("--files-with-matches foo -- a.txt"),
        case("--invert-match foo -- a.txt"),
        case("--ignore-case HELLO -- a.txt"),
        case("--word-regexp foo -- a.txt"),
        case("--quiet foo"),
        case("-n --no-line-number foo -- a.txt"),
        case("-v -v zzz -- a.txt"),
        case("-e foo --and -e bar"),
        case("-e foo --and -e hello"),
        case("-e foo --or -e hello -- a.txt"),
        case("--not -e foo -- a.txt"),
        case("-e foo --and --not -e bar -- a.txt"),
        case("( -e foo ) -- a.txt"),
        case("( -e foo --or -e hello ) --and -e again -- a.txt"),
        case("-e foo --and ( -e bar --or -e changed ) -- a.txt"),
        case("--not ( -e foo --or -e hello ) -- a.txt"),
        case("-e foo --not --not -e bar -- a.txt"),
        case("--or -e foo -- a.txt"),
        case("-e foo --or -- a.txt"),
        case("-e foo --or --and -e bar -- a.txt"),
        case("-e hello -e foo --and -e bar -- a.txt"),
        case("-nv -e foo --and -e bar -- a.txt"),
        case("-w -e foo -e wor -- a.txt"),
        case("-i -e HELLO --and -e WORLD -- a.txt"),
        case("-F o. -- a.txt"),
        case("-F -i HELLO -- a.txt"),
        case("-F -w foo -- a.txt"),
        case("-F -w o -- a.txt"),
        case("-E foo|hello -- a.txt"),
        case("-E -w foo|foo.bar -- a.txt"),
        case("-w -E foo|foo.bar -- a.txt"),
        case("-wE (foo|wordy)(_word)? -- a.txt"),
        case("foo\\|hello -- a.txt"),
        case("-E -F foo -- a.txt"),
        case("-F -E foo -- a.txt"),
        case("-G -E foo -- a.txt"),
        case("-E -G foo|hello -- a.txt"),
        case("-e foo -E -- a.txt"),
        case("-- foo"),
        case("foo --"),
        case("-n  -- empty.txt"),
        case("-c  -- blank.txt"),
        case("-nv foo -- blank.txt"),
        case("-n ^$ -- blank.txt"),
        case("-E -n ^$ -- blank.txt"),
        case("-n foo -- crlf.txt"),
        case("-nE foo.$ -- crlf.txt"),
        case("-n ^ -- crlf.txt"),
        case("-n ^$ -- end.txt end2.txt blank.txt nonl.txt"),
        case("-c ^$ -- end.txt end2.txt"),
        case("-l ^$ -- end2.txt"),
        case("-nE ^x*$ -- end.txt end2.txt"),
        case("-nv ^$ -- end.txt end2.txt"),
        case("-n -e ^$ --and -e ^$ -- end.txt end2.txt"),
        case("-n -e ^$ --or -e zzz -- end.txt end2.txt"),
        case("-n -e ^$ -e ^zz$ -- end.txt end2.txt"),
        case("-n ( -e ^$ ) -- end.txt end2.txt"),
        case("-nw ^$ -- end.txt end2.txt"),
        case("-ni ^$ -- end.txt end2.txt"),
        case("--cached -n ^$ -- end.txt end2.txt"),
        case("-n ^$ HEAD -- end.txt end2.txt"),
        case("-n foo -- nonl.txt"),
        case("-nc foo -- nonl.txt"),
        case("-nE foo$ -- nonl.txt"),
        case("-l foo -- long.txt"),
        case("-n x*foo -- long.txt"),
        case("-F -n caf\u{e9} -- uni.txt"),
        case("-n na -- uni.txt"),
        case("-l foo -- sp*"),
        case("-n foo -- sp?ce.txt"),
        case("-n foo -- q\"uote.txt"),
        case("-l foo -- link"),
        case("-l a.txt -- link"),
        case("-l foo -- module"),
        case("-l foo -- run.sh"),
        case("-l foo -- untracked.txt"),
        case("-l foo -- ignored.txt"),
        case("-l foo -- added.txt"),
        case("-l foo -- late.txt"),
        case("-l foo -- staged.txt"),
        case("foo a.txt"),
        case("foo a.txt sub"),
        case("foo sub/b.txt sub/deep"),
        case("foo -- sub"),
        case("foo -- sub/"),
        case("-l foo -- *.txt"),
        case("-l foo -- sub/*.txt"),
        case("-l foo -- *.log"),
        case("-l foo -- sub/deep/*"),
        case("-l foo -- nosuch"),
        case("-l foo -- a.txt a.txt"),
        case("-l foo -- ."),
        case("-l foo -- ./sub/../sub"),
        case("-l foo -- sub/deep/../b.txt"),
    ]
}

fn index_cases() -> Vec<Case> {
    vec![
        case("--cached foo"),
        case("--cached -n foo"),
        case("--cached -l foo"),
        case("--cached -c foo"),
        case("--cached -l foo -- added.txt"),
        case("--cached -l foo -- late.txt"),
        case("--cached -l foo -- untracked.txt"),
        case("--cached -n foo -- sub/b.txt"),
        case("--cached -v foo -- a.txt"),
        case("--cached -e foo --and -e changed"),
        case("--cached foo -- link module"),
        case("--cached -n foo -- bin.dat"),
        case("--cached -c foo -- bin.dat"),
        msg("--cached foo HEAD"),
        msg("--cached foo HEAD --"),
        msg("foo --cached"),
    ]
}

fn revision_cases() -> Vec<Case> {
    vec![
        case("foo HEAD"),
        case("-n foo HEAD"),
        case("-n foo HEAD~1"),
        case("-n foo HEAD^"),
        case("-n foo main"),
        case("-n foo side"),
        case("-n foo light"),
        case("-n foo annot"),
        case("-n foo {head}"),
        case("-n foo {short}"),
        case("-c foo HEAD HEAD~1"),
        case("-l foo HEAD~1 HEAD side"),
        case("-l foo HEAD HEAD"),
        case("-c foo HEAD -- sub"),
        case("-c foo HEAD -- a.txt late.txt"),
        case("-c foo HEAD sub"),
        case("-c foo HEAD -- nosuch"),
        case("-l foo HEAD -- *.txt"),
        case("-l foo HEAD -- link module"),
        case("-l foo HEAD -- staged.txt untracked.txt"),
        case("-h foo HEAD -- a.txt"),
        case("-hc foo HEAD -- sub"),
        case("-v foo HEAD -- a.txt"),
        case("-q foo HEAD"),
        case("-e foo --and -e changed HEAD"),
        case("foo HEAD -- bin.dat"),
        case("-a foo HEAD -- bin.dat"),
        case("-I foo HEAD -- bin.dat"),
        case("-c foo HEAD -- bin.dat"),
        case("-n foo HEAD~1 -- late.txt"),
        msg("foo nosuchrev --"),
        msg("foo HEAD nosuchrev --"),
        msg("foo HEAD~9"),
        msg("foo HEAD nosuch"),
        msg("foo HEAD a.txt nosuch"),
        msg("foo HEAD -n"),
        msg("foo a.txt HEAD"),
        msg("foo -- HEAD"),
        msg("-l foo a.txt -n"),
    ]
}

fn binary_cases() -> Vec<Case> {
    vec![
        case("foo -- bin.dat"),
        case("-n foo -- bin.dat"),
        case("-c foo -- bin.dat"),
        case("-l foo -- bin.dat"),
        case("-h foo -- bin.dat"),
        case("-v zzz -- bin.dat"),
        case("-v foo -- bin.dat"),
        case("-a foo -- bin.dat"),
        case("-an foo -- bin.dat"),
        case("-ac foo -- bin.dat"),
        case("-I foo -- bin.dat"),
        case("-Ic foo"),
        case("-Il foo"),
        case("-e foo --and -e plain -- bin.dat"),
        case("-e foo --and -e plain -a -- bin.dat"),
        case("zzz -- bin.dat"),
        case("foo -- bin2.dat bin.dat"),
        case("--cached foo -- bin.dat"),
    ]
}

fn error_cases() -> Vec<Case> {
    vec![
        Case {
            cwd: "",
            args: vec![],
            stderr: true,
        },
        msg("-n"),
        case("-e"),
        case("-e foo -e"),
        case("--bogus"),
        case("-y foo"),
        msg("foo nosuch"),
        msg("foo -n"),
        msg("foo a.txt nosuch"),
        msg("foo nosuch a.txt"),
        msg("foo a.txt -n"),
        msg("foo -- -n"),
        case("-E ("),
        case("-E ) a.txt"),
        msg("-e ( -e foo"),
        case("( -e foo"),
        case(") -e foo"),
        case("-e foo )"),
        msg("--not"),
        msg("-e foo --not"),
        msg("-e foo --and"),
        msg("--and -e foo"),
        msg("-e foo --and --and -e bar"),
        msg("--and foo"),
        msg("-E (a"),
        msg("\\( a.txt"),
        msg("-e \\( a.txt"),
        msg("-e foo -e \\( a.txt"),
        msg("-E * a.txt"),
        msg("-E + a.txt"),
        msg("-E ? a.txt"),
        msg("-E a{ a.txt"),
        msg("-E a{1 a.txt"),
        msg("-E a{x a.txt"),
        msg("-E a{2,1} a.txt"),
        msg("a\\{ a.txt"),
        msg("a\\{2,1\\} a.txt"),
        msg("a\\{x\\} a.txt"),
        msg("[ a.txt"),
        msg("[a a.txt"),
        msg("[[:alpha:] a.txt"),
        msg("[[:foo:]] a.txt"),
        msg("[z-a] a.txt"),
        msg("[[.ab.]] a.txt"),
        msg("[a-b-c] a.txt"),
        msg("a\\ a.txt"),
        msg("\\1 a.txt"),
        msg("-E \\1 a.txt"),
        msg("-E a**{ a.txt"),
        msg("a** a.txt"),
        msg("\\{1\\} a.txt"),
        msg("-E {1} a.txt"),
        msg("a\\{1\\}\\{2\\} a.txt"),
        msg("a\\) a.txt"),
        msg("-E ^* a.txt"),
        msg("-E a{65536} a.txt"),
        msg("a\\{65536\\} a.txt"),
    ]
}

const PATTERNS: &[&str] = &[
    "a",
    "ab",
    "^a",
    "a$",
    "^a$",
    "a*",
    "*a",
    "^*a",
    "a\\+",
    "a\\?",
    "ab\\+",
    "a\\{2\\}",
    "b\\{1,\\}",
    "a\\{,2\\}",
    "\\(ab\\)*",
    "\\(ab\\)\\{2\\}",
    "\\(a\\|b\\)\\(a\\|b\\)",
    "a\\|b",
    "a|b",
    "a+",
    "a?",
    "a{",
    "a{1",
    "(x)",
    "x)",
    "[x]",
    "[]x]",
    "[^]x]",
    "[a-]",
    "[-a]",
    "[a-b]",
    "[^a-b]",
    "[[:digit:]]",
    "[[:alpha:]]",
    "[[:space:]]",
    "[[:upper:]]",
    "[[:alnum:]_]",
    "[[:punct:]]",
    "[[.a.]]",
    "[[=a=]]",
    "[\\]",
    "[a\\]",
    ".",
    "a.b",
    "a\\.b",
    "\\.",
    "\\(",
    "\\)",
    "\\[",
    "\\*",
    "\\^a",
    "a\\$",
    "a^b",
    "a$b",
    "\\^",
    "\\$",
    "\\w",
    "\\W",
    "\\s",
    "\\S",
    "\\<a",
    "a\\>",
    "\\<ab\\>",
    "\\bab\\b",
    "\\Bb",
    "\\a",
    "\\n",
    "\\/",
    "\\-",
    "\\{1\\}",
    "\\+",
    "\\?",
    "\\|",
    "^$",
    "$",
    "^",
    "",
    "x*",
    "\\(\\)",
    "\\(^a\\)",
    "\\(a$\\)",
    "a\\|^b",
    "a$\\|b",
    "\\(*a\\)",
    "\\(\\(a\\)\\)",
    "a\\{1\\}b",
    "a\\{1,2\\}b",
    "wor_d",
    "  indented",
    "^ *indented",
    "FOO",
    "foo bar",
    "o\\{2\\}",
    "fo\\+",
    "\\(fo\\)\\+",
    "[[:upper:]][[:upper:]]",
];

const ERE_ONLY: &[&str] = &[
    "a+",
    "a?",
    "ab+",
    "a{2}",
    "a{1,2}b",
    "a{,2}",
    "a{1,}",
    "(ab)+",
    "(ab){2}",
    "(a|b)(a|b)",
    "a|b",
    "a|",
    "a||b",
    "()",
    "(a|)",
    "^(ab)*$",
    "a+*",
    "a**",
    "a{1}{2}",
    "a)",
    "x)",
    "(x)",
    "\\(x\\)",
    "\\(",
    "\\)",
    "\\{1\\}",
    "a\\{1\\}",
    "a\\|b",
    "a\\+",
    "^a|b$",
    "a^b",
    "a$b",
    "(^a)",
    "(a$)",
    "a|^b",
    "^*",
    "\\<a",
    "a\\>",
    "[x]+",
    "[a-c]{2}",
    "[[:alpha:]]+",
    "\\.",
    "a.b",
    "a\\.b",
    "x|y|",
    "foo|FOO",
    "(foo|FOO) bar",
    "o{2}",
    "fo+",
    "(fo)+",
    "\\w+",
    "\\s",
];

fn pattern_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for p in PATTERNS {
        cases.push(Case {
            cwd: "",
            args: vec!["-n", p, "--", "rx.txt"],
            stderr: true,
        });
        cases.push(Case {
            cwd: "",
            args: vec!["-nw", p, "--", "rx.txt"],
            stderr: true,
        });
        let folds = p
            .as_bytes()
            .windows(2)
            .any(|w| w[0] == b'\\' && w[1].is_ascii_lowercase() && !b"wsb".contains(&w[1]));
        if !folds {
            cases.push(Case {
                cwd: "",
                args: vec!["-ni", p, "--", "rx.txt"],
                stderr: true,
            });
        }
    }
    for p in ERE_ONLY {
        cases.push(Case {
            cwd: "",
            args: vec!["-nE", p, "--", "rx.txt"],
            stderr: true,
        });
        cases.push(Case {
            cwd: "",
            args: vec!["-nEw", p, "--", "rx.txt"],
            stderr: true,
        });
        cases.push(Case {
            cwd: "",
            args: vec!["-nEv", p, "--", "rx.txt"],
            stderr: true,
        });
    }
    for p in ["a.b", "a*", "[ab]", "a\\", "", "wor", "foo bar", "o", "FOO"] {
        cases.push(Case {
            cwd: "",
            args: vec!["-nF", p, "--", "rx.txt"],
            stderr: true,
        });
        cases.push(Case {
            cwd: "",
            args: vec!["-nFw", p, "--", "rx.txt"],
            stderr: true,
        });
        cases.push(Case {
            cwd: "",
            args: vec!["-nFi", p, "--", "rx.txt"],
            stderr: true,
        });
    }
    cases
}

fn folder_cases() -> Vec<Case> {
    vec![
        at("sub", "foo"),
        at("sub", "-n foo"),
        at("sub", "-n foo HEAD"),
        at("sub", "-c foo HEAD HEAD~1"),
        at("sub", "-l foo .."),
        at("sub", "-l foo ../"),
        at("sub", "-l foo ../a.txt ."),
        at("sub", "-l foo ../*.txt"),
        at("sub", "-l foo -- *.txt"),
        at("sub", "-l foo -- ../*.txt"),
        at("sub", "-l foo HEAD -- ../*.txt"),
        at("sub", "-l foo HEAD -- ."),
        at("sub", "--cached -l foo"),
        at("sub", "--cached -n foo -- ../a.txt"),
        at("sub", "--full-name -n foo"),
        at("sub", "--full-name -n foo HEAD"),
        at("sub", "--full-name -l foo .."),
        at("sub", "-l foo deep"),
        at("sub", "-l foo deep/c.txt"),
        at("sub", "-l foo -- deep/../b.txt"),
        at("sub", "-l foo -- nosuch"),
        at("sub/deep", "-l foo"),
        at("sub/deep", "-l foo .."),
        at("sub/deep", "-l foo ../.."),
        at("sub/deep", "-l foo ../../a.txt"),
        at("sub/deep", "-n foo ../b.txt c.txt"),
        at("sub/deep", "-n foo HEAD -- ../b.txt"),
        at("sub/deep", "-c foo HEAD -- ../../a.txt ."),
        Case {
            cwd: "sub",
            args: vec!["-l", "foo", "../.."],
            stderr: false,
        },
        Case {
            cwd: "sub",
            args: vec!["-l", "foo", "--", "../.."],
            stderr: false,
        },
    ]
}

fn run_all(format: &str) {
    let f = fixture(format);
    for cases in [
        matching_cases(),
        index_cases(),
        revision_cases(),
        binary_cases(),
        error_cases(),
        folder_cases(),
        pattern_cases(),
    ] {
        check(&f, cases);
    }
}

#[test]
fn grep_matches_git_sha1() {
    run_all("sha1");
}

#[test]
fn grep_matches_git_sha256() {
    run_all("sha256");
}

#[test]
fn grep_follows_the_grep_settings() {
    let f = fixture("sha1");
    f.git(&["config", "grep.lineNumber", "true"]);
    check(
        &f,
        vec![
            case("foo -- a.txt"),
            case("-c foo -- a.txt"),
            case("--no-line-number foo -- a.txt"),
        ],
    );
    f.git(&["config", "--unset", "grep.lineNumber"]);
    f.git(&["config", "grep.extendedRegexp", "true"]);
    check(
        &f,
        vec![
            case("-n foo|hello -- a.txt"),
            case("-n -G foo|hello -- a.txt"),
        ],
    );
    f.git(&["config", "grep.patternType", "fixed"]);
    check(
        &f,
        vec![
            case("-n o. -- a.txt"),
            case("-n -E foo|hello -- a.txt"),
            case("-n foo -- a.txt"),
        ],
    );
    f.git(&["config", "grep.patternType", "basic"]);
    check(
        &f,
        vec![
            case("-n foo\\|hello -- a.txt"),
            case("-n foo|hello -- a.txt"),
        ],
    );
    f.git(&["config", "grep.patternType", "extended"]);
    check(&f, vec![case("-n foo|hello -- a.txt")]);
    f.git(&["config", "--unset", "grep.patternType"]);
    f.git(&["config", "--unset", "grep.extendedRegexp"]);
    f.git(&["config", "grep.fullName", "true"]);
    check(&f, vec![at("sub", "-n foo"), at("sub", "-l foo HEAD")]);
}

#[test]
fn grep_in_a_conflicted_index_and_an_unborn_branch() {
    let f = fixture("sha1");
    f.git(&["checkout", "-q", "-f", "-b", "left", "side"]);
    f.write("c.txt", b"base foo\n");
    f.git(&["add", "c.txt"]);
    f.git(&["commit", "-qm", "base"]);
    f.git(&["checkout", "-q", "-b", "right"]);
    f.write("c.txt", b"right foo\n");
    f.git(&["commit", "-qam", "right"]);
    f.git(&["checkout", "-q", "left"]);
    f.write("c.txt", b"left foo\n");
    f.git(&["commit", "-qam", "left"]);
    let merge = f
        .command("git", &f.root, &["merge", "right"])
        .output()
        .unwrap();
    assert_eq!(merge.status.code(), Some(1));
    check(
        &f,
        vec![
            case("-n foo -- c.txt"),
            case("-c foo -- c.txt"),
            case("--cached -n foo -- c.txt"),
            case("--cached -l foo"),
            case("-l ======= -- c.txt"),
            case("-n foo right -- c.txt"),
        ],
    );
    f.git(&["merge", "--abort"]);
    f.git(&["checkout", "-q", "--orphan", "fresh"]);
    check(
        &f,
        vec![
            case("-l foo"),
            case("--cached -l foo"),
            msg("foo HEAD"),
            case("-l foo main"),
        ],
    );
}

#[test]
fn grep_refuses_what_it_cannot_match_exactly() {
    let f = fixture("sha1");
    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        let out = f.run("pyn", "", &args);
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    for option in [
        "--untracked",
        "--no-index",
        "-P",
        "-o",
        "-A1",
        "-L",
        "-z",
        "--color",
        "--heading",
    ] {
        let (code, err) = run(&["grep", option, "foo"]);
        assert_eq!(code, Some(129), "{option}: {err}");
        assert!(err.contains("not supported"), "{option}: {err}");
    }
    let (code, err) = run(&["grep", "\\(a\\)\\1", "a.txt"]);
    assert_eq!(code, Some(128));
    assert!(err.contains("back-references are not supported"), "{err}");
    let (code, err) = run(&["grep", "-i", "\\a", "a.txt"]);
    assert_eq!(code, Some(128), "{err}");
    let (code, err) = run(&["grep", "foo", "HEAD:sub"]);
    assert_eq!(code, Some(128), "{err}");
    let (code, err) = run(&["grep", "--help"]);
    assert_eq!(code, Some(0), "{err}");
}

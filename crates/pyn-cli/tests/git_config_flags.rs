//! `pyn config` in git's flag forms against real git (the oracle, in these tests only) on twin repositories: standard
//! output, exit status, whether standard error carries anything, and the resulting config files byte for byte.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::{TempDir, tempdir};

struct Side {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

fn side(tmp: &TempDir, name: &str) -> Side {
    let root = tmp.path().canonicalize().unwrap().join(name);
    let repo = root.join("repo");
    let home = root.join("home");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&home).unwrap();
    let s = Side { root, repo, home };
    let out = s.run(Command::new("git"), &s.repo, &["init", "-q"]);
    assert_eq!(out.0, 0);
    s
}

impl Side {
    fn run(&self, mut c: Command, cwd: &Path, args: &[&str]) -> (i32, Vec<u8>, Vec<u8>) {
        let mut child = c
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"").unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap(), out.stdout, out.stderr)
    }

    fn files(&self) -> Vec<Option<Vec<u8>>> {
        [
            self.repo.join(".git/config"),
            self.home.join(".gitconfig"),
            self.home.join(".config/git/config"),
            self.root.join("f.cfg"),
        ]
        .iter()
        .map(|p| fs::read(p).ok())
        .collect()
    }
}

fn same(git: &Side, pyn: &Side, cwd: &str, args: &[&str]) {
    let at = |s: &Side| {
        if cwd.is_empty() {
            s.root.clone()
        } else {
            s.repo.join(cwd)
        }
    };
    let want = git.run(Command::new("git"), &at(git), &[&["config"], args].concat());
    let got = pyn.run(
        Command::new(env!("CARGO_BIN_EXE_pyn")),
        &at(pyn),
        &[&["config"], args].concat(),
    );
    let shown = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
    assert_eq!(shown(&got.1), shown(&want.1), "stdout of config {args:?}");
    assert_eq!(
        got.0,
        want.0,
        "status of config {args:?} (pyn: {})",
        shown(&got.2)
    );
    assert_eq!(
        got.2.is_empty(),
        want.2.is_empty(),
        "stderr use of config {args:?}: pyn {:?}, git {:?}",
        shown(&got.2),
        shown(&want.2)
    );
    let rel = |s: &Side, b: Vec<Option<Vec<u8>>>| {
        b.into_iter()
            .map(|f| f.map(|f| shown(&f).replace(s.root.to_str().unwrap(), "")))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rel(pyn, pyn.files()),
        rel(git, git.files()),
        "files after config {args:?}"
    );
}

#[test]
fn config_flag_forms_match_git() {
    let tmp = tempdir().unwrap();
    let (git, pyn) = (side(&tmp, "git"), side(&tmp, "pyn"));
    for s in [&git, &pyn] {
        fs::create_dir_all(s.repo.join("d")).unwrap();
    }
    let steps: Vec<(&str, Vec<&str>)> = vec![
        (".", vec!["--list"]),
        (".", vec!["user.name"]),
        (".", vec!["user.name", "Bob"]),
        (".", vec!["user.name"]),
        ("d", vec!["--get", "user.name"]),
        (".", vec!["--get", "user.nosuch"]),
        (".", vec!["--get-all", "user.name"]),
        (".", vec!["--add", "user.x", "a"]),
        (".", vec!["--add", "user.x", "b"]),
        (".", vec!["--get-all", "user.x"]),
        (".", vec!["user.x"]),
        (".", vec!["user.x", "c"]),
        (".", vec!["--unset", "user.x"]),
        (".", vec!["--unset-all", "user.x"]),
        (".", vec!["--unset", "user.x"]),
        (".", vec!["--unset", "nosuch.k"]),
        (".", vec!["Sec.Sub.Key", "v"]),
        (".", vec!["sec.Sub.key"]),
        (".", vec!["sec.sub.key"]),
        (".", vec!["--list"]),
        (".", vec!["-l"]),
        (".", vec!["--local", "--list"]),
        (".", vec!["--global", "a.b", "c"]),
        (".", vec!["--global", "--list"]),
        (".", vec!["--global", "a.b"]),
        (".", vec!["--local", "a.b"]),
        (".", vec!["a.b"]),
        (".", vec!["--list"]),
        (".", vec!["--global", "--unset", "a.b"]),
        (".", vec!["--global", "--unset", "a.b"]),
        (".", vec!["--file", "nosuch.cfg", "--list"]),
        (".", vec!["--file", "nosuch.cfg", "a.b"]),
        (".", vec!["--system", "--list"]),
        (".", vec!["--file", "../f.cfg", "a.b", "c"]),
        (".", vec!["-f", "../f.cfg", "--list"]),
        (".", vec!["foo"]),
        (".", vec!["foo."]),
        (".", vec!["--get", "foo"]),
        (".", vec!["a.b c", "d"]),
        (".", vec!["--list", "foo"]),
        (".", vec!["--local", "--global", "a.b", "c"]),
        (".", vec!["--get"]),
        (".", vec!["a.b", "-"]),
        (".", vec!["a.b"]),
        (".", vec!["a.b", ""]),
        (".", vec!["a.b"]),
        (".", vec!["core.quoted", "a \"q\" b;c#d"]),
        (".", vec!["core.quoted"]),
        (".", vec!["--list"]),
        (".", vec!["bare.key"]),
        (".", vec!["--get-all", "core.nosuch"]),
        (".", vec!["--global", "x.y", "z"]),
        (".", vec!["x.y"]),
        (".", vec!["--list"]),
    ];
    for (cwd, args) in &steps {
        same(&git, &pyn, cwd, args);
    }
    for (cwd, args) in [
        ("", vec!["--list"]),
        ("", vec!["--local", "--list"]),
        ("", vec!["--local", "a.b", "c"]),
        ("", vec!["a.b", "c"]),
        ("", vec!["--unset", "a.b"]),
        ("", vec!["a.b"]),
        ("", vec!["--global", "a.b", "c"]),
        ("", vec!["--list"]),
    ] {
        same(&git, &pyn, cwd, &args);
    }
}

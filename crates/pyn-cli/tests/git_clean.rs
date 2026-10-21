//! `pyn clean` on a git repository, compared with the real `git clean` on a copy of the same work tree: the exit
//! status, the work tree afterwards and the set of `Would remove` / `Removing` lines. Needs `git` on the PATH (tests only).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

impl Env {
    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
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
fn x_and_capital_x_cannot_be_combined_and_interactive_is_not_built() {
    let p = repo("sha1", "*.log\n");
    p.same("", &["-nxX"], "x and X");
    let out = p.env.pyn(&p.pyn, &["clean", "-i"]);
    assert_eq!(out.status.code(), Some(129), "{out:?}");
    assert!(p.pyn.join("u").exists());
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

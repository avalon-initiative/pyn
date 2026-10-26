//! `pyn init-git`: a new repository used like git, written without git. Real git is the oracle in one test only, and
//! only to confirm it accepts what pyn wrote; semantics are compared, never message wording.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::{TempDir, tempdir};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Rig {
    fn new() -> Self {
        Self::with_config("")
    }

    fn with_config(extra: &str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".gitconfig"),
            format!("[user]\n\tname = T\n\temail = t@example.com\n{extra}"),
        )
        .unwrap();
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        Rig {
            _tmp: tmp,
            root,
            home,
        }
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Out {
        let o = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        Out {
            code: o.status.code().unwrap(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let o = self.pyn(cwd, args);
        assert_eq!(o.code, 0, "pyn {args:?}: {}", o.stderr);
        o.stdout
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Out {
        let o = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        Out {
            code: o.status.code().unwrap(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        }
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn both_object_formats_make_an_empty_normal_repository() {
    for (format, version) in [("sha1", "0"), ("sha256", "1")] {
        let rig = Rig::new();
        let work = rig.work();
        rig.ok(&work, &["init-git", "--object-format", format]);
        let git = work.join(".git");
        assert_eq!(read(&git.join("HEAD")), "ref: refs/heads/main\n");
        let config = read(&git.join("config"));
        assert!(config.contains(&format!("repositoryformatversion = {version}")));
        assert!(config.contains("filemode = true"));
        assert!(config.contains("bare = false"));
        assert!(config.contains("logallrefupdates = true"));
        assert_eq!(config.contains("objectformat = sha256"), format == "sha256");
        for dir in [
            "hooks",
            "info",
            "objects/info",
            "objects/pack",
            "refs/heads",
            "refs/tags",
        ] {
            assert!(git.join(dir).is_dir(), "{dir}");
        }
        assert!(git.join("description").is_file());
        assert_eq!(fs::read_dir(git.join("refs/heads")).unwrap().count(), 0);
        let status = rig.ok(&work, &["status"]);
        assert!(status.contains("main"), "{status}");
    }
}

#[test]
fn a_first_commit_works_with_pyn_on_both_formats() {
    for format in ["sha1", "sha256"] {
        let rig = Rig::new();
        let work = rig.work();
        rig.ok(&work, &["init-git", "--object-format", format]);
        fs::write(work.join("a.txt"), "hello\n").unwrap();
        rig.ok(&work, &["add", "a.txt"]);
        rig.ok(&work, &["commit", "-m", "x"]);
        let log = rig.ok(&work, &["log"]);
        assert!(log.contains('x'), "{log}");
        assert!(read(&work.join(".git/refs/heads/main")).trim().len() >= 40);
        assert!(!rig.ok(&work, &["status"]).contains("a.txt"));
    }
}

#[test]
fn the_initial_branch_is_main_then_the_config_then_the_flag() {
    let rig = Rig::new();
    let work = rig.work();
    rig.ok(&work, &["init-git"]);
    assert_eq!(read(&work.join(".git/HEAD")), "ref: refs/heads/main\n");

    let rig = Rig::with_config("[init]\n\tdefaultBranch = trunk\n");
    let work = rig.work();
    rig.ok(&work, &["init-git"]);
    assert_eq!(read(&work.join(".git/HEAD")), "ref: refs/heads/trunk\n");

    let other = rig.root.join("other");
    rig.ok(&rig.root, &["init-git", "-b", "dev", "other"]);
    assert_eq!(read(&other.join(".git/HEAD")), "ref: refs/heads/dev\n");
    let third = rig.root.join("third");
    rig.ok(&rig.root, &["init-git", "--initial-branch", "x/y", "third"]);
    assert_eq!(read(&third.join(".git/HEAD")), "ref: refs/heads/x/y\n");
}

#[test]
fn an_invalid_branch_name_is_fatal_and_creates_nothing() {
    let rig = Rig::new();
    let o = rig.pyn(&rig.root, &["init-git", "-b", "bad..name", "fresh"]);
    assert_eq!(o.code, 128, "{}", o.stderr);
    assert!(!rig.root.join("fresh/.git").exists());
}

#[test]
fn the_directory_argument_is_created_or_may_already_exist() {
    let rig = Rig::new();
    let out = rig.ok(&rig.root, &["init-git", "deep/new"]);
    let made = rig.root.join("deep/new");
    assert!(made.join(".git/HEAD").is_file());
    assert_eq!(
        out,
        format!(
            "Initialized empty Git repository in {}/.git/\n",
            made.display()
        )
    );
    let existing = rig.root.join("empty");
    fs::create_dir(&existing).unwrap();
    rig.ok(&rig.root, &["init-git", "empty"]);
    assert!(existing.join(".git/HEAD").is_file());
}

#[test]
fn the_message_goes_to_stdout_and_quiet_prints_nothing() {
    let rig = Rig::new();
    let o = rig.pyn(&rig.work(), &["init-git"]);
    assert_eq!(o.code, 0);
    assert_eq!(
        o.stdout,
        format!(
            "Initialized empty Git repository in {}/.git/\n",
            rig.work().display()
        )
    );
    assert!(o.stderr.is_empty(), "{}", o.stderr);
    let q = rig.pyn(&rig.root, &["init-git", "-q", "quiet"]);
    assert_eq!((q.code, q.stdout.as_str(), q.stderr.as_str()), (0, "", ""));
    let q = rig.pyn(&rig.root, &["init-git", "--quiet", "quiet2"]);
    assert_eq!((q.code, q.stdout.as_str()), (0, ""));
}

#[test]
fn pyn_files_are_recorded_and_hidden_from_git() {
    let rig = Rig::new();
    let work = rig.work();
    rig.ok(&work, &["init-git"]);
    assert_eq!(
        read(&work.join(".pyn/local_only/repo.toml")),
        "repo_tool = \"git\"\n"
    );
    let exclude = read(&work.join(".git/info/exclude"));
    assert!(exclude.lines().any(|l| l == ".pyn/"), "{exclude}");
    assert!(!rig.ok(&work, &["status"]).contains(".pyn"));
}

#[test]
fn a_tracked_or_covered_directory_is_refused_naming_convert() {
    let rig = Rig::new();
    let outer = rig.root.join("outer");
    rig.ok(&rig.root, &["init-git", "outer"]);
    let again = rig.pyn(&outer, &["init-git"]);
    let inside = rig.pyn(&outer, &["init-git", "sub/dir"]);
    for o in [&again, &inside] {
        assert_eq!(o.code, 128, "{}", o.stderr);
        assert!(o.stderr.contains("pyn convert"), "{}", o.stderr);
        assert!(o.stdout.is_empty());
    }
    assert!(!outer.join("sub").exists());

    let pyn_only = rig.root.join("pyn-ws");
    fs::create_dir_all(pyn_only.join(".pyn/local_only")).unwrap();
    for args in [&["init-git"][..], &["init-git", "child"][..]] {
        let o = rig.pyn(&pyn_only, args);
        assert_eq!(o.code, 128, "{}", o.stderr);
        assert!(o.stderr.contains("pyn convert"), "{}", o.stderr);
    }
    assert!(!pyn_only.join(".git").exists());
}

#[test]
fn a_directory_inside_a_plain_git_repository_is_refused() {
    let rig = Rig::new();
    let repo = rig.root.join("plain");
    fs::create_dir_all(&repo).unwrap();
    assert_eq!(rig.git(&repo, &["init", "-q"]).code, 0);
    let o = rig.pyn(&repo, &["init-git", "nested"]);
    assert_eq!(o.code, 128, "{}", o.stderr);
    assert!(o.stderr.contains("pyn convert"), "{}", o.stderr);
    assert!(!repo.join("nested").exists());
}

#[test]
fn git_options_pyn_does_not_build_are_usage_errors() {
    let rig = Rig::new();
    for (args, flag) in [
        (&["init-git", "--template", "t"][..], "--template"),
        (&["init-git", "--shared"][..], "--shared"),
        (&["init-git", "--shared=group"][..], "--shared"),
    ] {
        let o = rig.pyn(&rig.work(), args);
        assert_eq!(o.code, 129, "{args:?}: {}", o.stderr);
        assert!(o.stderr.contains(flag), "{}", o.stderr);
    }
    assert!(!rig.work().join(".git").exists());
}

#[test]
fn init_git_is_listed_in_help() {
    let rig = Rig::new();
    assert!(rig.ok(&rig.work(), &["--help"]).contains("init-git"));
}

#[test]
fn git_accepts_what_pyn_wrote() {
    for format in ["sha1", "sha256"] {
        let rig = Rig::new();
        let work = rig.work();
        rig.ok(&work, &["init-git", "--object-format", format]);
        let fmt = rig.git(&work, &["rev-parse", "--show-object-format"]);
        assert_eq!(fmt.stdout.trim(), format, "{}", fmt.stderr);
        assert_eq!(rig.git(&work, &["fsck"]).code, 0);
        let status = rig.git(&work, &["status", "--porcelain"]);
        assert_eq!(
            (status.code, status.stdout.as_str()),
            (0, ""),
            "{}",
            status.stderr
        );
        let branch = rig.git(&work, &["symbolic-ref", "--short", "HEAD"]);
        assert_eq!(branch.stdout.trim(), "main");

        fs::write(work.join("g.txt"), "from git\n").unwrap();
        assert_eq!(rig.git(&work, &["add", "g.txt"]).code, 0);
        let c = rig.git(&work, &["commit", "-q", "-m", "by git"]);
        assert_eq!(c.code, 0, "{}", c.stderr);
        assert_eq!(rig.git(&work, &["fsck"]).code, 0);

        fs::write(work.join("p.txt"), "from pyn\n").unwrap();
        rig.ok(&work, &["add", "p.txt"]);
        rig.ok(&work, &["commit", "-m", "by pyn"]);
        let log = rig.git(&work, &["log", "--format=%s"]);
        assert_eq!(log.stdout, "by pyn\nby git\n");
        assert_eq!(rig.git(&work, &["fsck"]).code, 0);
    }
}

//! Staging on a git repository: `add`, `commit`, `restore --staged` and `diff --cached` compared with what git
//! produced from the same starting point (`fixtures/git/regenerate-staging.sh`; the tests never run git), and that
//! pyn writes only what the command asked for.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::gitfix::*;
use pyn_git::{Index, Repository};
use pyn_object::Algorithm;

struct Scenario {
    name: &'static str,
    linked: bool,
    sub: &'static str,
    commands: &'static [&'static [&'static str]],
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "add-file",
        linked: false,
        sub: "",
        commands: &[&["add", "README.md"]],
    },
    Scenario {
        name: "add-dot",
        linked: false,
        sub: "",
        commands: &[&["add", "."]],
    },
    Scenario {
        name: "add-update",
        linked: false,
        sub: "",
        commands: &[&["add", "-u"]],
    },
    Scenario {
        name: "add-subdir",
        linked: false,
        sub: "src",
        commands: &[&["add", "."]],
    },
    Scenario {
        name: "add-partial",
        linked: false,
        sub: "",
        commands: &[&["add", "src/lib.rs"]],
    },
    Scenario {
        name: "add-new",
        linked: false,
        sub: "",
        commands: &[&["add", "new.txt", "newlink", "newdir"]],
    },
    Scenario {
        name: "add-deleted",
        linked: false,
        sub: "",
        commands: &[&["add", "data.bin"]],
    },
    Scenario {
        name: "add-mode",
        linked: false,
        sub: "",
        commands: &[&["add", "feature.txt"]],
    },
    Scenario {
        name: "add-ignored",
        linked: false,
        sub: "",
        commands: &[&["add", "debug.out", "build/o.bin"]],
    },
    Scenario {
        name: "add-force",
        linked: false,
        sub: "",
        commands: &[&["add", "-f", "debug.out"]],
    },
    Scenario {
        name: "commit-all",
        linked: false,
        sub: "",
        commands: &[&["commit", "-a", "-m", "commit all"]],
    },
    Scenario {
        name: "commit-staged",
        linked: false,
        sub: "",
        commands: &[&["commit", "-m", "staged only"]],
    },
    Scenario {
        name: "restore-staged",
        linked: false,
        sub: "",
        commands: &[&["restore", "--staged", "src/lib.rs", "staged.txt"]],
    },
    Scenario {
        name: "linked-commit",
        linked: true,
        sub: "",
        commands: &[
            &["add", "feature.txt", "wt-only.txt"],
            &["commit", "-m", "in the worktree"],
        ],
    },
];

fn algorithm(format: &str) -> Algorithm {
    Algorithm::from_name(format).unwrap()
}

/// `git ls-files -s` for the index at `path`.
fn listing(path: &Path, format: &str) -> String {
    let index = Index::read(path, algorithm(format)).unwrap();
    index
        .entries
        .iter()
        .map(|e| {
            format!(
                "{:06o} {} {}\t{}\n",
                e.mode,
                e.id,
                e.stage,
                String::from_utf8_lossy(&e.path)
            )
        })
        .collect()
}

fn index_path(repo: &Repo, linked: bool) -> PathBuf {
    if linked {
        repo.main.join(".git/worktrees/linked/index")
    } else {
        repo.main.join(".git/index")
    }
}

fn expected_in(repo: &Repo, name: &str, file: &str) -> Option<String> {
    let path = fixtures()
        .join(repo.format)
        .join("expected/staging")
        .join(name)
        .join(file);
    fs::read_to_string(path).ok()
}

fn commit_object(repo: &Repo, id: &str) -> String {
    let git = Repository::open(&repo.main).unwrap();
    let id = pyn_object::ObjectId::from_hex(algorithm(repo.format), id).unwrap();
    String::from_utf8(git.objects().unwrap().read(&id).unwrap().data).unwrap()
}

#[test]
fn staging_matches_what_git_staged_and_committed() {
    for format in FORMATS {
        for s in SCENARIOS {
            let repo = Repo::new(format);
            let cwd = if s.linked {
                repo.linked.clone()
            } else {
                repo.main.clone()
            }
            .join(s.sub);
            let want_exit: i32 =
                expected_in(&repo, s.name, "exit.txt").map_or(0, |t| t.trim().parse().unwrap());
            for args in s.commands {
                let out = repo.run_in(&cwd, args);
                let last = std::ptr::eq(*args, *s.commands.last().unwrap());
                let code = out.status.code().unwrap();
                assert_eq!(
                    code,
                    if last { want_exit } else { 0 },
                    "{format} {}: pyn {args:?}: {}",
                    s.name,
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            let at = format!("{format} {}", s.name);
            assert_eq!(
                listing(&index_path(&repo, s.linked), format),
                expected_in(&repo, s.name, "ls-files.txt").unwrap(),
                "{at}: index"
            );
            let root = if s.linked { &repo.linked } else { &repo.main };
            let status = repo.ok_in(root, &["status"]);
            assert_eq!(
                porcelain(&status),
                sorted_lines(&expected_in(&repo, s.name, "status.txt").unwrap()),
                "{at}: status\n{status}"
            );
            let cached = repo.ok_in(root, &["diff", "--cached"]);
            assert_eq!(
                cached,
                expected_in(&repo, s.name, "diff-cached.txt").unwrap(),
                "{at}: diff --cached"
            );
            if let Some(head) = expected_in(&repo, s.name, "head.txt") {
                let branch = if s.linked { "feature" } else { "main" };
                let got = fs::read_to_string(repo.main.join(format!(".git/refs/heads/{branch}")))
                    .unwrap();
                assert_eq!(got, head, "{at}: the branch is where git put it");
                assert_eq!(
                    commit_object(&repo, head.trim()),
                    expected_in(&repo, s.name, "commit.txt").unwrap(),
                    "{at}: commit object"
                );
            }
        }
    }
}

#[test]
fn commit_dash_am_is_commit_dash_a_dash_m() {
    let repo = Repo::new("sha1");
    repo.ok(&["commit", "-am", "commit all"]);
    let head = fs::read_to_string(repo.main.join(".git/refs/heads/main")).unwrap();
    assert_eq!(head, expected_in(&repo, "commit-all", "head.txt").unwrap());
}

/// What changed under a git directory and the work tree between two snapshots.
fn changed(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let mut keys: Vec<String> = after
        .iter()
        .filter(|(k, v)| before.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .chain(
            after
                .keys()
                .chain(before.keys())
                .filter(|k| !after.contains_key(*k) || !before.contains_key(*k))
                .cloned(),
        )
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

#[test]
fn add_changes_the_index_and_adds_objects_and_nothing_else() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let before = snapshot(&repo.main);
        repo.ok(&["add", "."]);
        let after = snapshot(&repo.main);
        let moved = changed(&before, &after);
        assert!(
            moved
                .iter()
                .all(|k| k == ".git/index" || k.starts_with(".git/objects/")),
            "{format}: {moved:?}"
        );
        assert!(moved.contains(&".git/index".to_string()));
    }
}

#[test]
fn commit_moves_the_branch_and_adds_objects_and_nothing_else() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let before = snapshot(&repo.main);
        repo.ok(&["commit", "-m", "staged only"]);
        let moved = changed(&before, &snapshot(&repo.main));
        assert!(
            moved.iter().all(|k| k == ".git/refs/heads/main"
                || k.starts_with(".git/objects/")
                || k.starts_with(".git/logs")),
            "{format}: {moved:?}"
        );
        repo.ok(&["commit", "-a", "-m", "all"]);
        let moved = changed(&before, &snapshot(&repo.main));
        assert!(
            moved.iter().all(|k| k == ".git/refs/heads/main"
                || k == ".git/index"
                || k.starts_with(".git/logs")
                || k.starts_with(".git/objects/")),
            "{format}: {moved:?}"
        );
    }
}

#[test]
fn restore_staged_and_dry_run_and_a_refused_add_change_no_work_tree_file() {
    let repo = Repo::new("sha1");
    let tree = |r: &Repo| {
        snapshot(&r.main)
            .into_iter()
            .filter(|(k, _)| !k.starts_with(".git"))
            .collect::<BTreeMap<_, _>>()
    };
    let before = tree(&repo);
    let git_before = snapshot(&repo.main);
    let out = repo.ok(&["add", "-n", "."]);
    assert!(
        out.contains("add 'README.md'")
            && out.contains("remove 'data.bin'")
            && out.contains("add 'newlink'"),
        "{out}"
    );
    assert!(
        !out.contains("debug.out") && !out.contains("build/"),
        "{out}"
    );
    assert_eq!(snapshot(&repo.main), git_before, "a dry run writes nothing");
    repo.ok(&["add", "-v", "README.md"]);
    repo.ok(&["restore", "--staged", "."]);
    assert_eq!(tree(&repo), before);
}

#[test]
fn what_is_staged_stays_staged_when_the_file_changes_again() {
    let repo = Repo::new("sha1");
    fs::write(repo.main.join("README.md"), "first\n").unwrap();
    repo.ok(&["add", "README.md"]);
    fs::write(repo.main.join("README.md"), "second\n").unwrap();
    let status = repo.ok(&["status"]);
    assert!(
        status.lines().any(|l| l.starts_with("modified")
            && l.contains("modified")
            && l.ends_with("README.md")),
        "staged and unstaged: {status}"
    );
    repo.ok(&["commit", "-m", "first only"]);
    let shown = repo.ok(&["show", "HEAD"]);
    assert!(
        shown.contains("+first") && !shown.contains("second"),
        "{shown}"
    );
    let after = repo.ok(&["diff", "README.md"]);
    assert!(
        after.contains("-first") && after.contains("+second"),
        "{after}"
    );
}

#[test]
fn unknown_pathspecs_and_ignored_files_are_refused_like_git() {
    let repo = Repo::new("sha1");
    let (code, err) = repo.fails(&["add", "nothing-here"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("pathspec 'nothing-here' did not match any files"),
        "{err}"
    );
    let (code, err) = repo.fails(&["restore", "--staged", "nothing-here"]);
    assert_eq!(code, 128);
    assert!(err.contains("did not match"), "{err}");
    let out = repo.run_in(&repo.main, &["add", "debug.out"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("ignored by one of your .gitignore files")
            && err.contains("debug.out")
            && err.contains("-f"),
        "{err}"
    );
    let out = repo.run_in(&repo.main, &["add", "private"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "an excluded folder is refused too"
    );
    let out = repo.run_in(&repo.main, &["add"]);
    assert_eq!(out.status.code(), Some(129), "add needs a path, -u or -A");
}

#[test]
fn paths_are_taken_from_where_you_stand() {
    let repo = Repo::new("sha1");
    repo.ok_in(&repo.main.join("src"), &["add", "lib.rs", "../README.md"]);
    let listing = listing(&index_path(&repo, false), "sha1");
    let status = repo.ok(&["status"]);
    assert!(status.contains("README.md"), "{status}");
    assert!(listing.contains("src/lib.rs"));
    let (code, err) = repo.fails(&["add", "/nowhere/else"]);
    assert_eq!(code, 128);
    assert!(err.contains("outside"), "{err}");
}

fn git_dir_file(repo: &Repo, name: &str, text: &str) {
    fs::write(repo.main.join(".git").join(name), text).unwrap();
}

#[test]
fn commit_refuses_what_it_cannot_do_safely_and_changes_nothing() {
    let repo = Repo::new("sha1");
    let before = snapshot(&repo.main);
    let (code, err) = repo.fails(&["commit", "-m", "x", "README.md"]);
    assert_eq!(code, 128);
    assert!(err.contains("pyn add"), "{err}");

    git_dir_file(
        &repo,
        "CHERRY_PICK_HEAD",
        "0000000000000000000000000000000000000000\n",
    );
    let (code, err) = repo.fails(&["commit", "-m", "x"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("in progress") && err.contains("CHERRY_PICK_HEAD"),
        "{err}"
    );
    fs::remove_file(repo.main.join(".git/CHERRY_PICK_HEAD")).unwrap();
    assert_eq!(snapshot(&repo.main), before);

    fs::create_dir(repo.main.join(".git/rebase-merge")).unwrap();
    let (_, err) = repo.fails(&["commit", "-m", "x"]);
    assert!(err.contains("rebase-merge"), "{err}");
    fs::remove_dir(repo.main.join(".git/rebase-merge")).unwrap();

    assert_eq!(
        snapshot(&repo.main),
        before,
        "every refusal left the repository as it was"
    );
}

#[test]
fn commit_needs_an_identity_and_staged_changes() {
    let repo = Repo::new("sha1");
    let run = |args: &[&str], with_identity: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pyn"));
        cmd.args(args)
            .current_dir(&repo.main)
            .env("HOME", &repo.home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("EMAIL")
            .env("PYN_CONFIG_DIR", repo.home.join("pyn-config"));
        for var in ["NAME", "EMAIL"] {
            for role in ["AUTHOR", "COMMITTER"] {
                if with_identity {
                    cmd.env(format!("GIT_{role}_{var}"), "x");
                } else {
                    cmd.env_remove(format!("GIT_{role}_{var}"));
                }
            }
        }
        cmd.output().unwrap()
    };
    let before = snapshot(&repo.main);
    let out = run(&["commit", "-a", "-m", "x"], false);
    assert_eq!(out.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&out.stderr).contains("user.name and user.email"));
    assert_eq!(
        snapshot(&repo.main),
        before,
        "-a staged nothing before failing"
    );

    let out = run(&["commit", "-m", "x"], true);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = run(&["commit", "-m", "again"], true);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nothing to commit"));
}

#[test]
fn the_first_commit_of_a_new_repository_has_no_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for format in FORMATS {
        let dir = root.join(format);
        fs::create_dir_all(dir.join(".git/objects")).unwrap();
        fs::create_dir_all(dir.join(".git/refs/heads")).unwrap();
        fs::write(dir.join(".git/HEAD"), "ref: refs/heads/trunk\n").unwrap();
        fs::write(
            dir.join(".git/config"),
            format!(
                "[core]\n\trepositoryformatversion = {}\n{}",
                u8::from(format == "sha256"),
                if format == "sha256" {
                    "[extensions]\n\tobjectFormat = sha256\n"
                } else {
                    ""
                }
            ),
        )
        .unwrap();
        fs::write(dir.join("a.txt"), "a\n").unwrap();
        let run = |args: &[&str]| {
            Command::new(env!("CARGO_BIN_EXE_pyn"))
                .args(args)
                .current_dir(&dir)
                .env("HOME", &root)
                .env("GIT_AUTHOR_NAME", "A")
                .env("GIT_AUTHOR_EMAIL", "a@x")
                .env("GIT_COMMITTER_NAME", "A")
                .env("GIT_COMMITTER_EMAIL", "a@x")
                .output()
                .unwrap()
        };
        assert_eq!(
            run(&["commit", "-m", "x"]).status.code(),
            Some(1),
            "nothing staged"
        );
        assert!(run(&["add", "a.txt"]).status.success());
        let out = run(&["commit", "-m", "first"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("[trunk (root-commit) "));
        let id = fs::read_to_string(dir.join(".git/refs/heads/trunk")).unwrap();
        let git = Repository::open(&dir).unwrap();
        let id = pyn_object::ObjectId::from_hex(algorithm(format), id.trim()).unwrap();
        let object = git.objects().unwrap().read(&id).unwrap();
        assert!(!String::from_utf8_lossy(&object.data).contains("parent"));
    }
}

#[test]
fn an_index_in_version_4_stays_in_version_4() {
    let repo = Repo::new("sha1");
    let path = index_path(&repo, false);
    let mut index = Index::read(&path, Algorithm::Sha1).unwrap();
    index.version = 4;
    index.write(&path, Algorithm::Sha1).unwrap();
    repo.ok(&["add", "new.txt"]);
    let after = Index::read(&path, Algorithm::Sha1).unwrap();
    assert_eq!(after.version, 4);
    assert!(after.entries.iter().any(|e| e.path == b"new.txt"));
}

#[test]
fn unmerged_paths_and_a_lock_file_stop_staging() {
    let repo = Repo::new("sha1");
    let path = index_path(&repo, false);
    let mut index = Index::read(&path, Algorithm::Sha1).unwrap();
    let mut conflict = index
        .entries
        .iter()
        .find(|e| e.path == b"README.md")
        .unwrap()
        .clone();
    index.entries.retain(|e| e.path != b"README.md");
    for stage in 1..=3 {
        conflict.stage = stage;
        index.entries.push(conflict.clone());
    }
    index.write(&path, Algorithm::Sha1).unwrap();
    let (code, err) = repo.fails(&["commit", "-m", "x"]);
    assert_eq!(code, 128);
    assert!(err.contains("unmerged"), "{err}");
    repo.ok(&["add", "README.md"]);
    let index = Index::read(&path, Algorithm::Sha1).unwrap();
    let stages: Vec<u8> = index
        .entries
        .iter()
        .filter(|e| e.path == b"README.md")
        .map(|e| e.stage)
        .collect();
    assert_eq!(stages, [0], "adding an unmerged path resolves it");

    let repo = Repo::new("sha1");
    fs::write(repo.main.join(".git/index.lock"), "").unwrap();
    let (code, err) = repo.fails(&["add", "new.txt"]);
    assert_eq!(code, 128);
    assert!(err.contains("index.lock"), "{err}");
}

/// Needs a git binary: checks the repository pyn wrote is one git accepts. Run with `cargo test -- --ignored`.
#[test]
#[ignore = "runs the real git binary"]
fn git_accepts_what_pyn_wrote() {
    for format in FORMATS {
        let repo = Repo::new(format);
        repo.ok(&["add", "-A"]);
        repo.ok(&["commit", "-m", "from pyn"]);
        repo.ok(&["add", "-A"]);
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&repo.main)
                .env("HOME", &repo.home)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{format}: git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap()
        };
        git(&["fsck", "--strict"]);
        let status = git(&["status", "--porcelain"]);
        assert_eq!(
            sorted_lines(&status),
            porcelain(&repo.ok(&["status"])),
            "{format}"
        );
        assert!(git(&["log", "-1", "--format=%s"]).starts_with("from pyn"));
        assert_eq!(git(&["diff", "--cached", "--stat"]), "");
    }
}

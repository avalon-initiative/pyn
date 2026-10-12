//! Branches, switching, checkout, restore, tags, merges and stashes on a git repository, compared with what git left
//! on disk for the same commands (`fixtures/git/scenarios-writes.txt`; the tests never run git), and the refusals
//! that must leave the repository byte for byte as it was.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::gitfix::*;
use pyn_git::Repository;
use pyn_object::{Algorithm, ObjectId};

fn algorithm(format: &str) -> Algorithm {
    Algorithm::from_name(format).unwrap()
}

/// `git ls-files -s` for the index at `path`.
fn listing(path: &Path, format: &str) -> String {
    let index = pyn_git::Index::read(path, algorithm(format)).unwrap();
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

fn git_dir_of(root: &Path) -> PathBuf {
    pyn_git::resolve_git_dir(&root.join(".git")).unwrap()
}

fn object_text(repo: &Repository, id: &ObjectId) -> String {
    String::from_utf8_lossy(&repo.objects().unwrap().read(id).unwrap().data).into_owned()
}

/// What the fixture script records, built from the repository `root` as pyn left it.
struct Dump {
    refs: String,
    ls_files: String,
    tree: String,
    logs: String,
    state: String,
    objects: String,
    config: String,
}

fn dump(root: &Path, format: &str) -> Dump {
    let repo = Repository::open(root).unwrap();
    let git_dir = git_dir_of(root);
    let mut refs: String = repo
        .refs()
        .list("refs/")
        .unwrap()
        .iter()
        .map(|(n, id)| format!("{n} {id}\n"))
        .collect();
    refs.push_str(&format!(
        "HEAD {}",
        fs::read_to_string(git_dir.join("HEAD")).unwrap()
    ));
    let mut state = String::new();
    for name in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "ORIG_HEAD"] {
        if let Ok(text) = fs::read_to_string(git_dir.join(name)) {
            state.push_str(&format!("== {name}\n{text}"));
        }
    }
    let mut objects = String::from("== HEAD\n");
    if let Some(head) = repo.refs().resolve("HEAD").unwrap() {
        objects.push_str(&object_text(&repo, &head));
    }
    for (name, id) in repo.refs().list("refs/").unwrap() {
        if name.starts_with("refs/tags/") || name == "refs/stash" {
            objects.push_str(&format!("== {name}\n{}", object_text(&repo, &id)));
        }
    }
    Dump {
        refs,
        ls_files: listing(&git_dir.join("index"), format),
        tree: tree_dump(root),
        logs: logs_dump(repo.common_dir()),
        state,
        objects,
        config: fs::read_to_string(repo.common_dir().join("config")).unwrap(),
    }
}

/// Runs the scenario's commands with pyn and returns the last one's exit code and standard output.
fn run(repo: &Repo, s: &Scenario) -> (i32, String) {
    let root = if s.linked { &repo.linked } else { &repo.main };
    let cwd = root.join(&s.sub);
    let (mut code, mut stdout) = (0, String::new());
    for command in &s.commands {
        let args: Vec<&str> = command.iter().map(String::as_str).collect();
        match args.as_slice() {
            ["@write", path, text] => {
                fs::write(root.join(path), text.replace("\\n", "\n")).unwrap();
                continue;
            }
            ["@resume"] => {
                let out = repo.run_in(&cwd, &["commit"]);
                code = out.status.code().unwrap();
                stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            }
            _ => {
                let out = repo.run_in(&cwd, &args);
                code = out.status.code().unwrap();
                stdout = String::from_utf8_lossy(&out.stdout).into_owned();
            }
        }
    }
    (code, stdout)
}

#[test]
fn every_scenario_leaves_what_git_left() {
    let mut failures = Vec::new();
    for format in FORMATS {
        for s in scenarios() {
            if std::env::var("SCENARIO").is_ok_and(|only| only != s.name) {
                continue;
            }
            let repo = if s.fixture == "merge" {
                Repo::merge(format)
            } else {
                Repo::new(format)
            };
            let (code, stdout) = run(&repo, &s);
            let root = if s.linked { &repo.linked } else { &repo.main };
            let got = dump(root, format);
            let want = |file: &str| repo.expected_writes(&s.fixture, &s.name, file);
            let mut diffs: Vec<String> = Vec::new();
            let detail = std::env::var("SCENARIO").is_ok();
            let mut check = |what: &str, got: &str, want: String| {
                if got != want {
                    diffs.push(if detail {
                        format!("--- {what}\nwant:\n{want}\ngot:\n{got}")
                    } else {
                        format!("--- {what}")
                    });
                }
            };
            let want_code: i32 = want("exit.txt").trim().parse().unwrap();
            check("exit", &code.to_string(), want_code.to_string());
            check("refs", &got.refs, want("refs.txt"));
            check("ls-files", &got.ls_files, want("ls-files.txt"));
            check("tree", &got.tree, want("tree.txt"));
            check("logs", &got.logs, want("logs.txt"));
            check("state", &got.state, want("state.txt"));
            check("objects", &got.objects, want("objects.txt"));
            check("config", &got.config, want("config.txt"));
            let status = repo.ok_in(root, &["status"]);
            let unmerged = |line: &str| {
                let (xy, path) = line.split_at(2);
                let conflicted =
                    matches!(xy, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU" | " U");
                if conflicted {
                    format!("U{path}")
                } else {
                    line.to_string()
                }
            };
            let rows = |lines: Vec<String>| {
                let mut rows: Vec<String> = lines.iter().map(|l| unmerged(l)).collect();
                rows.sort();
                rows.join("\n")
            };
            check(
                "status",
                &rows(porcelain(&status)),
                rows(sorted_lines(&want("status.txt"))),
            );
            if s.name.starts_with("out-") {
                check("stdout", &stdout, want("stdout.txt"));
            }
            if !diffs.is_empty() {
                failures.push(format!("{format} {}:\n{}", s.name, diffs.join("\n")));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

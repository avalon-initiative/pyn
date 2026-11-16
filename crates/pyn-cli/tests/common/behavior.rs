//! Behavior comparison for twin tests (decision #1032): pyn and git must do the same thing, not word it the same.
//! Compared: repository state, exit status, standard output only for machine formats; standard error only when `Stderr::Quiet`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use super::oracle::{assert_has_facts, status_class};

/// Whether standard output is a contract (`Machine`: porcelain, plumbing, `-z`, `--numstat`, `--raw`, ...) or prose.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Human,
    Machine,
}

/// Standard error is not compared by default: git moves advice between stdout and stderr across releases.
/// `Quiet` is for a command git leaves silent in every release; pyn must then print nothing either.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stderr {
    Ignored,
    Quiet,
}

/// What one command run produced.
pub struct Ran {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Ran {
    pub fn out(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn err(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// Panics unless standard output states every fact (ids abbreviated, whitespace squeezed).
    pub fn has_facts(&self, facts: &[&str], what: &str) {
        assert_has_facts(&self.out(), facts, what);
    }
}

/// A repository as plumbing reports it. Sections are sorted lines, so two snapshots compare and diff by line.
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    sections: Vec<(&'static str, Vec<String>)>,
}

fn plumbing(dir: &Path, args: &[&str]) -> Vec<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    lines.sort();
    lines
}

/// Drops the label after a conflict marker (`<<<<<<< HEAD`, `||||||| constructed ...`): git words it differently by release.
pub fn mask_conflict_labels(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let run = line.chars().take_while(|c| "<|>".contains(*c)).count();
            let head: Vec<char> = line.chars().take(run).collect();
            let uniform = head.iter().all(|c| Some(c) == head.first());
            if run >= 7 && uniform && line[run..].starts_with(' ') {
                let eol = if line.ends_with('\n') { "\n" } else { "" };
                format!("{}{eol}", &line[..run])
            } else {
                line.to_string()
            }
        })
        .collect()
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path.strip_prefix(root).unwrap().display().to_string();
        if rel == ".git" {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            walk(root, &path, out);
        } else if meta.file_type().is_symlink() {
            let to = fs::read_link(&path).unwrap();
            out.push(format!("{rel} -> {}", to.display()));
        } else {
            let exec = if meta.permissions().mode() & 0o111 != 0 {
                "exec"
            } else {
                "file"
            };
            let text = mask_conflict_labels(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            out.push(format!("{rel} {exec} {text:?}"));
        }
    }
}

/// Entries of `.git` that exist only while an operation is in progress, with the id inside a `*_HEAD` file.
fn operation_state(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir.join(".git")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with("rebase-") || name == "sequencer" {
            out.push(name);
        } else if name.ends_with("_HEAD") && name != "ORIG_HEAD" && name != "FETCH_HEAD" {
            let id = fs::read_to_string(&path).unwrap_or_default();
            out.push(format!("{name} {}", id.trim()));
        }
    }
    out.sort();
    out
}

fn counted(lines: &[String]) -> BTreeMap<&str, i32> {
    let mut m = BTreeMap::new();
    lines
        .iter()
        .for_each(|l| *m.entry(l.as_str()).or_default() += 1);
    m
}

impl Snapshot {
    /// Refs and `HEAD`, reachable objects, index entries (`mode id stage path`), work tree files with modes,
    /// local config, and the state of an operation in progress.
    pub fn of(dir: &Path) -> Self {
        let mut tree = Vec::new();
        walk(dir, dir, &mut tree);
        tree.sort();
        let mut refs = plumbing(dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
        refs.extend(plumbing(dir, &["symbolic-ref", "-q", "HEAD"]));
        refs.extend(plumbing(dir, &["rev-parse", "-q", "--verify", "HEAD"]));
        Self {
            sections: vec![
                ("refs", refs),
                (
                    "objects",
                    plumbing(dir, &["rev-list", "--objects", "--all"]),
                ),
                ("index", plumbing(dir, &["ls-files", "-s"])),
                ("work tree", tree),
                ("config", plumbing(dir, &["config", "--list", "--local"])),
                ("operation", operation_state(dir)),
            ],
        }
    }

    /// Rewrites the path of every index entry, for names git versions choose differently.
    pub fn map_index_paths(mut self, f: impl Fn(&str) -> String) -> Self {
        for (name, lines) in &mut self.sections {
            if *name == "index" {
                for line in lines.iter_mut() {
                    if let Some((head, path)) = line.split_once('\t') {
                        *line = format!("{head}\t{}", f(path));
                    }
                }
                lines.sort();
            }
        }
        self
    }

    /// A readable list of the lines only one side has, per section; `None` when they match.
    pub fn diff(&self, other: &Self) -> Option<String> {
        let mut report = String::new();
        for ((name, a), (_, b)) in self.sections.iter().zip(&other.sections) {
            let (ca, cb) = (counted(a), counted(b));
            for (line, n) in &ca {
                if cb.get(line) != Some(n) {
                    report.push_str(&format!("[{name}] only in first: {line}\n"));
                }
            }
            for (line, n) in &cb {
                if ca.get(line) != Some(n) {
                    report.push_str(&format!("[{name}] only in second: {line}\n"));
                }
            }
        }
        (!report.is_empty()).then_some(report)
    }
}

/// The stable codes must match exactly; any other code is compared by class.
pub fn assert_same_status(what: &str, want: &Ran, got: &Ran) {
    let stable = [0, 1, 2, 128, 129];
    let same = if stable.contains(&want.code) {
        want.code == got.code
    } else {
        status_class(want.code) == status_class(got.code)
    };
    assert!(
        same,
        "exit status of {what}: git {}, pyn {} (pyn stderr: {})",
        want.code,
        got.code,
        got.err()
    );
}

/// Panics when the repositories differ, with the lines that do.
pub fn assert_same_state(what: &str, want: &Path, got: &Path) {
    if let Some(d) = Snapshot::of(want).diff(&Snapshot::of(got)) {
        panic!("state after {what} (first git, second pyn):\n{d}");
    }
}

/// Compares what two tools did: exit status, state, standard output (`Machine` only), standard error (`Quiet` only).
pub fn assert_same_behavior(
    what: &str,
    format: Format,
    stderr: Stderr,
    (want, want_repo): (&Ran, &Path),
    (got, got_repo): (&Ran, &Path),
) {
    assert_same_status(what, want, got);
    if format == Format::Machine {
        assert_eq!(
            got.out(),
            want.out(),
            "stdout of {what} (pyn stderr: {})",
            got.err()
        );
    }
    if stderr == Stderr::Quiet {
        assert!(
            want.stderr.is_empty() && got.stderr.is_empty(),
            "stderr of {what} must be empty: pyn {:?}, git {:?}",
            got.err(),
            want.err()
        );
    }
    assert_same_state(what, want_repo, got_repo);
}

//! The `merge` attribute on a git repository (`binary`, `-merge`, `union`, `text`, `merge.<name>.driver`,
//! `merge.default`), compared with the real `git merge` on a copy of the same repository: exit status class, index,
//! status and work tree. Real git is the oracle for meaning only; its message text is not compared.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::{FORMATS, copy_tree, snapshot};

#[path = "common/oracle.rs"]
mod oracle;

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

impl Pair {
    fn new(format: &str, script: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let git = root.join("git");
        fs::create_dir_all(&git).unwrap();
        let init = format!(
            "git init -q -b main --object-format={format} .\n\
             git config gc.auto 0\ngit config maintenance.auto false\ngit config merge.conflictStyle diff3\n{script}"
        );
        let out = Command::new("sh")
            .args(["-e", "-c", &init])
            .current_dir(&git)
            .env("HOME", &home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        let pyn = root.join("pyn");
        copy_tree(&git, &pyn);
        Self {
            _tmp: tmp,
            home,
            git,
            pyn,
        }
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn pyn_cmd(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    fn state(&self, dir: &Path) -> Vec<String> {
        let g = |args: &[&str]| text(&self.git_in(dir, args).stdout);
        let mut out = vec![
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!("status\n{}", g(&["status", "--porcelain"])),
        ];
        for (path, content) in snapshot(dir) {
            if path == ".git" || path.starts_with(".git/") || path.starts_with(".pyn") {
                continue;
            }
            out.push(format!("{path} {content}"));
        }
        out
    }

    /// Runs the same `git merge <args>` and `pyn merge <args>`; the status class and the resulting state must agree.
    fn merge(&self, args: &[&str]) {
        let mut a = vec!["merge"];
        a.extend_from_slice(args);
        let (g, p) = (self.git_in(&self.git, &a), self.pyn_cmd(&a));
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{a:?}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(self.state(&self.git), self.state(&self.pyn), "{a:?}");
    }
}

/// `f` and `sub/g` start with five lines; `side` changes the first one and `main` the last one, so a line merge is clean.
/// `extra` is shell run on the base commit (attributes, config).
fn disjoint(extra: &str) -> String {
    format!(
        "mkdir sub; printf 'a\\nb\\nc\\nd\\ne\\n' >f; cp f sub/g; {extra}\ngit add -A .; git commit -qm base\n\
         git checkout -q -b side\nprintf 'A\\nb\\nc\\nd\\ne\\n' >f; cp f sub/g; git add -A .; git commit -qm side\n\
         git checkout -q main\nprintf 'a\\nb\\nc\\nd\\nE\\n' >f; cp f sub/g; git add -A .; git commit -qm main\n"
    )
}

/// Both sides change the same line.
fn clashing(extra: &str) -> String {
    format!(
        "mkdir sub; printf 'a\\nb\\nc\\n' >f; cp f sub/g; {extra}\ngit add -A .; git commit -qm base\n\
         git checkout -q -b side\nprintf 'a\\nS\\nc\\n' >f; cp f sub/g; git add -A .; git commit -qm side\n\
         git checkout -q main\nprintf 'a\\nM\\nc\\n' >f; cp f sub/g; git add -A .; git commit -qm main\n"
    )
}

fn each(script: &str, run: impl Fn(&Pair)) {
    for format in FORMATS {
        run(&Pair::new(format, script));
    }
}

#[test]
fn binary_and_unset_merge_conflict_where_text_would_merge() {
    for attr in ["* merge=binary", "* -merge", "* merge=text", "* merge"] {
        let script = disjoint(&format!("printf '{attr}\\n' >.gitattributes"));
        each(&script, |p| p.merge(&["side"]));
    }
}

#[test]
fn binary_takes_a_side_with_x_ours_and_x_theirs() {
    let script = disjoint("printf '* merge=binary\\n' >.gitattributes");
    for side in ["-Xours", "-Xtheirs"] {
        each(&script, |p| p.merge(&[side, "side"]));
    }
}

#[test]
fn union_keeps_both_sides_of_a_conflict() {
    let script = clashing(
        "printf '*.nope merge=union\\nf merge=union\\nsub/g merge=union\\n' >.gitattributes",
    );
    each(&script, |p| p.merge(&["side"]));
    let script = disjoint("printf 'f merge=union\\n' >.gitattributes");
    each(&script, |p| p.merge(&["side"]));
}

#[test]
fn union_adds_the_missing_final_newline_between_sides() {
    let script = "printf 'a\\nb' >f; printf 'f merge=union\\n' >.gitattributes; git add .; git commit -qm base\n\
        git checkout -q -b side\nprintf 'a\\nS' >f; git add .; git commit -qm side\n\
        git checkout -q main\nprintf 'a\\nM' >f; git add .; git commit -qm main\n";
    each(script, |p| p.merge(&["side"]));
}

#[test]
fn a_file_with_nul_bytes_is_binary_to_text_and_union_alike() {
    for attr in ["f merge=text", "f merge=union", "f merge"] {
        let script = format!(
            "printf 'a\\0\\nb\\n' >f; printf '{attr}\\n' >.gitattributes; git add .; git commit -qm base\n\
             git checkout -q -b side\nprintf 'a\\0\\nS\\n' >f; git add .; git commit -qm side\n\
             git checkout -q main\nprintf 'a\\0\\nM\\n' >f; git add .; git commit -qm main\n"
        );
        each(&script, |p| p.merge(&["side"]));
    }
}

#[test]
fn a_driver_command_merges_or_conflicts_by_its_exit_status() {
    let attrs = "printf 'f merge=pick\\nsub/g merge=fail\\n' >.gitattributes; \
        git config merge.pick.driver 'cat %B >%A'; \
        git config merge.fail.driver 'printf \"driver\\n\" >%A; false'";
    each(&clashing(attrs), |p| p.merge(&["side"]));
    each(&disjoint(attrs), |p| p.merge(&["side"]));
}

#[test]
fn a_driver_sees_the_path_marker_size_and_labels() {
    let attrs = "printf '* merge=show\\n' >.gitattributes; \
        git config merge.show.driver 'printf \"%P %L\\n\" >%A; cat %O >>%A; false'";
    each(&clashing(attrs), |p| p.merge(&["side"]));
}

#[test]
fn a_driver_reads_the_three_versions() {
    let attrs = "printf 'f merge=cat\\n' >.gitattributes; \
        git config merge.cat.driver 'cat %O %A %B >%A.out; mv %A.out %A'";
    each(&clashing(attrs), |p| p.merge(&["side"]));
}

#[test]
fn merge_default_names_the_driver_of_an_unspecified_path() {
    let script =
        disjoint("printf 'f merge=text\\n' >.gitattributes; git config merge.default binary");
    each(&script, |p| p.merge(&["side"]));
    let script = clashing("git config merge.default union");
    each(&script, |p| p.merge(&["side"]));
}

#[test]
fn an_unknown_driver_name_merges_as_text() {
    each(
        &disjoint("printf '* merge=nonesuch\\n' >.gitattributes"),
        |p| p.merge(&["side"]),
    );
    each(
        &clashing("printf '* merge=nonesuch\\n' >.gitattributes"),
        |p| p.merge(&["side"]),
    );
}

#[test]
fn a_configured_driver_may_be_named_union() {
    let attrs = "printf 'f merge=union\\n' >.gitattributes; git config merge.union.driver 'printf over >%A'";
    each(&clashing(attrs), |p| p.merge(&["side"]));
}

#[test]
fn cherry_pick_uses_the_attribute_too() {
    let script = disjoint("printf '* merge=binary\\n' >.gitattributes");
    each(&script, |p| {
        let (g, y) = (
            p.git_in(&p.git, &["cherry-pick", "side"]),
            p.pyn_cmd(&["cherry-pick", "side"]),
        );
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(y.status.code().unwrap()),
            "pyn said {}",
            text(&y.stderr)
        );
        assert_eq!(p.state(&p.git), p.state(&p.pyn));
    });
}

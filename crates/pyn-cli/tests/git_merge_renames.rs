//! Rename detection in `pyn merge` on a git repository, compared with the real `git merge` on a copy of the same
//! repository: exit status class, `HEAD`, index stages, work tree and whether a merge is in progress. Real git is the
//! oracle for meaning only; message text is not compared. Needs `git` on the PATH (tests only).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::{FORMATS, copy_tree, snapshot};

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

/// Two copies of one repository: one for git, one for pyn.
struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
}

/// Exit status class: success or failure (git has moved between 1 and 128 across releases).
fn status_class(code: i32) -> i32 {
    i32::from(code != 0)
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
        let pair = Self {
            _tmp: tmp,
            pyn: root.join("pyn"),
            git,
            home,
        };
        let init = format!(
            "git init -q -b main --object-format={format} .\n\
             git config gc.auto 0\ngit config maintenance.auto false\ngit config merge.conflictStyle diff3\n{script}"
        );
        let out = Command::new("sh")
            .args(["-e", "-c", &init])
            .current_dir(&pair.git)
            .env("HOME", &pair.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        copy_tree(&pair.git, &pair.pyn);
        pair
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn pyn_in(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    /// The facts a merge leaves: the commit, the index with its stages, the work tree and the merge in progress.
    fn state(&self, dir: &Path) -> Vec<String> {
        let g = |args: &[&str]| text(&self.git_in(dir, args).stdout);
        let mut out = vec![
            format!("HEAD {}", g(&["rev-parse", "HEAD"]).trim()),
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!("merging {}", dir.join(".git/MERGE_HEAD").exists()),
        ];
        for (path, content) in snapshot(dir) {
            if path != ".git" && !path.starts_with(".git/") && !path.starts_with(".pyn") {
                out.push(format!("{path} {content}"));
            }
        }
        out
    }

    /// Runs the same `git <args>` and `pyn <args>`; the status class and the whole resulting state must agree.
    fn same(&self, args: &[&str]) {
        let (g, p) = (self.git_in(&self.git, args), self.pyn_in(args));
        let what = format!("{args:?}");
        assert_eq!(
            status_class(g.status.code().unwrap()),
            status_class(p.status.code().unwrap()),
            "{what}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(self.state(&self.git), self.state(&self.pyn), "{what}");
    }

    /// A shell command run in both copies.
    fn both_sh(&self, script: &str) {
        for dir in [&self.git, &self.pyn] {
            let out = Command::new("sh")
                .args(["-e", "-c", script])
                .current_dir(dir)
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .envs(IDENTITY)
                .output()
                .unwrap();
            assert!(out.status.success(), "{script}: {}", text(&out.stderr));
        }
    }

    /// pyn alone: the merge stops on a conflict.
    fn expect_conflict(&self, args: &[&str]) {
        let out = self.pyn_in(args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }

    /// pyn refuses `args` and changes nothing.
    fn refused(&self, args: &[&str]) {
        let before = self.state(&self.pyn);
        let p = self.pyn_in(args);
        assert_ne!(p.status.code(), Some(0), "{args:?}: {}", text(&p.stderr));
        assert_eq!(before, self.state(&self.pyn), "{args:?}");
    }
}

fn each_format(script: &str, run: impl Fn(&Pair)) {
    for format in FORMATS {
        run(&Pair::new(format, script));
    }
}

fn merges(script: &str, args: &[&str]) {
    each_format(script, |p| p.same(args));
}

/// As `merges`, with `git config <setting>` applied to the repository first.
fn merges_with(script: &str, setting: &str, args: &[&str]) {
    merges(&format!("{script}git config {setting}\n"), args);
}

/// A base with a ten line file `a`, a bystander `k`; `side` is the branch to merge into `main`.
fn history(side: &str, main: &str) -> String {
    format!(
        "seq 1 10 >a; echo k >k; git add .; git commit -qm base\n\
         git checkout -q -b side\n{side}\ngit add -A; git commit -qm side\n\
         git checkout -q main\n{main}\ngit add -A; git commit -qm main\n"
    )
}

const MOVE: &str = "git mv a b";
const EDIT_5: &str = "sed -i 5s/.*/E5/ a";
const EDIT_5_B: &str = "sed -i 5s/.*/E5/ b";

#[test]
fn an_edit_follows_a_rename_on_the_other_side() {
    merges(&history(MOVE, EDIT_5), &["merge", "side"]);
    merges(&history(EDIT_5, MOVE), &["merge", "side"]);
    merges(
        &history(&format!("{MOVE}; {EDIT_5_B}"), "sed -i 8s/.*/M8/ a"),
        &["merge", "side"],
    );
    merges(
        &history("sed -i 8s/.*/S8/ a", &format!("{MOVE}; {EDIT_5_B}")),
        &["merge", "side"],
    );
}

#[test]
fn a_rename_alone_or_with_an_edit_on_one_side_merges_cleanly() {
    merges(&history(MOVE, "echo z >z"), &["merge", "side"]);
    merges(&history("echo z >z", MOVE), &["merge", "side"]);
    merges(
        &history(&format!("{MOVE}; {EDIT_5_B}"), "echo z >z"),
        &["merge", "side"],
    );
}

#[test]
fn edits_that_clash_across_a_rename_conflict_at_the_new_path() {
    merges(
        &history(&format!("{MOVE}; {EDIT_5_B}"), "sed -i 5s/.*/M5/ a"),
        &["merge", "side"],
    );
    merges(
        &history("sed -i 5s/.*/S5/ a", &format!("{MOVE}; sed -i 5s/.*/M5/ b")),
        &["merge", "side"],
    );
}

#[test]
fn a_mode_change_follows_a_rename() {
    merges(&history(MOVE, "chmod +x a"), &["merge", "side"]);
    merges(&history("chmod +x a", MOVE), &["merge", "side"]);
}

#[test]
fn a_similar_but_not_identical_rename_is_found() {
    let reword = "sed -i '2s/.*/two/;7s/.*/seven/' b";
    merges(
        &history(&format!("{MOVE}; {reword}"), EDIT_5),
        &["merge", "side"],
    );
    merges(
        &history(EDIT_5, &format!("{MOVE}; {reword}")),
        &["merge", "side"],
    );
}

#[test]
fn a_rename_against_a_delete_conflicts() {
    merges(&history(MOVE, "git rm -q a"), &["merge", "side"]);
    merges(&history("git rm -q a", MOVE), &["merge", "side"]);
    merges(
        &history(&format!("{MOVE}; {EDIT_5_B}"), "git rm -q a"),
        &["merge", "side"],
    );
    merges(
        &history("git rm -q a", &format!("{MOVE}; {EDIT_5_B}")),
        &["merge", "side"],
    );
}

#[test]
fn a_file_renamed_to_two_places_conflicts() {
    merges(&history(MOVE, "git mv a c"), &["merge", "side"]);
    merges(
        &history(
            &format!("{MOVE}; {EDIT_5_B}"),
            "git mv a c; sed -i 8s/.*/M8/ c",
        ),
        &["merge", "side"],
    );
}

#[test]
fn both_sides_renaming_a_file_to_one_place_merges_it() {
    merges(&history(MOVE, MOVE), &["merge", "side"]);
    merges(
        &history(
            &format!("{MOVE}; {EDIT_5_B}"),
            &format!("{MOVE}; sed -i 8s/.*/M8/ b"),
        ),
        &["merge", "side"],
    );
    merges(
        &history(
            &format!("{MOVE}; {EDIT_5_B}"),
            &format!("{MOVE}; sed -i 5s/.*/M5/ b"),
        ),
        &["merge", "side"],
    );
}

#[test]
fn two_files_renamed_to_one_path_conflict_as_added_on_both() {
    let script = "seq 1 10 >a; seq 11 20 >z; git add .; git commit -qm base\n\
        git checkout -q -b side\ngit mv a n; git commit -qm side\n\
        git checkout -q main\ngit mv z n; git commit -qm main\n";
    merges(script, &["merge", "side"]);
}

#[test]
fn a_path_added_where_the_other_side_renamed_to_is_an_add_add_conflict() {
    merges(&history(MOVE, "echo other >b"), &["merge", "side"]);
    merges(&history("echo other >b", MOVE), &["merge", "side"]);
}

const FOLDER: &str =
    "mkdir a; seq 1 10 >a/x; seq 11 20 >a/y; echo k >k; git add .; git commit -qm base\n";

fn folder_history(side: &str, main: &str) -> String {
    format!(
        "{FOLDER}git checkout -q -b side\n{side}\ngit add -A; git commit -qm side\n\
         git checkout -q main\n{main}\ngit add -A; git commit -qm main\n"
    )
}

#[test]
fn a_file_added_to_a_renamed_folder_follows_it() {
    let moves = "git mv a b";
    let adds = "echo new >a/n";
    for (side, main) in [(moves, adds), (adds, moves)] {
        let script = folder_history(side, main);
        merges(&script, &["merge", "side"]);
        for value in ["true", "conflict", "false"] {
            merges_with(
                &script,
                &format!("merge.directoryRenames {value}"),
                &["merge", "side"],
            );
        }
    }
}

#[test]
fn folder_renames_work_with_edits_and_deeper_folders() {
    merges(
        &folder_history("git mv a b", "sed -i 3s/.*/E/ a/x; echo new >a/n"),
        &["merge", "side"],
    );
    let nested = "mkdir -p a/s; seq 1 10 >a/s/x; seq 11 20 >a/s/y; git add .; git commit -qm base\n\
        git checkout -q -b side\ngit mv a b; git commit -qm side\n\
        git checkout -q main\necho new >a/s/n; echo top >a/t; git add .; git commit -qm main\n";
    merges(nested, &["merge", "side"]);
    merges_with(nested, "merge.directoryRenames true", &["merge", "side"]);
}

#[test]
fn a_folder_that_keeps_a_file_is_not_renamed() {
    merges(
        &folder_history("git mv a/x b_x", "echo new >a/n"),
        &["merge", "side"],
    );
    merges(
        &folder_history(
            "mkdir b; git mv a/x b/x; git mv a/y b/y; echo stay >a/s",
            "echo new >a/n",
        ),
        &["merge", "side"],
    );
}

#[test]
fn rename_options_and_settings_decide_whether_renames_are_followed() {
    let script = history(MOVE, EDIT_5);
    merges(&script, &["merge", "-X", "find-renames", "side"]);
    merges(&script, &["merge", "-X", "find-renames=90%", "side"]);
    merges(&script, &["merge", "-s", "recursive", "side"]);
    merges(&script, &["merge", "--no-commit", "side"]);
    merges_with(&script, "merge.renames true", &["merge", "side"]);
    merges_with(&script, "diff.renames true", &["merge", "side"]);
}

/// `ort` of git 2.43 follows renames whatever `-X no-renames` and `merge.renames` say, so `recursive` is the oracle.
#[test]
fn renames_can_be_switched_off() {
    let script = history(MOVE, EDIT_5);
    fn with<'a>(extra: &[&'a str]) -> Vec<&'a str> {
        [&["merge", "-s", "recursive"][..], extra, &["side"]].concat()
    }
    merges(&script, &with(&["-X", "no-renames"]));
    merges(&script, &with(&["-X", "no-renames", "-X", "find-renames"]));
    merges(&script, &with(&["-X", "find-renames", "-X", "no-renames"]));
    merges_with(&script, "merge.renames false", &with(&[]));
    merges_with(&script, "diff.renames false", &with(&[]));
    merges_with(
        &script,
        "merge.renames false",
        &with(&["-X", "find-renames"]),
    );
    merges_with(&script, "merge.renames true", &with(&["-X", "no-renames"]));
    each_format(&script, |p| {
        p.expect_conflict(&["merge", "-X", "no-renames", "side"])
    });
    each_format(&script, |p| {
        p.expect_conflict(&["merge", "-s", "ort", "-X", "no-renames", "side"])
    });
}

#[test]
fn the_similarity_threshold_decides_which_rename_is_found() {
    let reword = "sed -i '1,5s/.*/changed/' b";
    let script = history(&format!("{MOVE}; {reword}"), "sed -i 9s/.*/E9/ a");
    for args in [
        &["merge", "side"][..],
        &["merge", "-X", "find-renames=30%", "side"],
        &["merge", "-X", "find-renames=70%", "side"],
        &["merge", "-X", "rename-threshold=70%", "side"],
        &["merge", "-X", "rename-threshold=30%", "side"],
        &["merge", "-X", "find-renames=90", "side"],
    ] {
        merges(&script, args);
    }
}

#[test]
fn strategy_favor_options_apply_to_a_renamed_file() {
    let script = history(&format!("{MOVE}; {EDIT_5_B}"), "sed -i 5s/.*/M5/ a");
    merges(&script, &["merge", "-X", "ours", "side"]);
    merges(&script, &["merge", "-X", "theirs", "side"]);
}

#[test]
fn renames_are_followed_inside_the_virtual_base_of_a_criss_cross() {
    let clock = "T=1700000000\n\
        tick() { T=$((T+100)); GIT_AUTHOR_DATE=\"$T +0000\"; GIT_COMMITTER_DATE=\"$T +0000\"; export GIT_AUTHOR_DATE GIT_COMMITTER_DATE; }\n\
        ci() { tick; git commit -q --allow-empty \"$@\"; }\n\
        mg() { tick; git merge -q --no-edit \"$@\" >/dev/null 2>&1 || true; }\n";
    let cross = |x: &str, y: &str, main: &str, side: &str| {
        format!(
            "{clock}seq 1 9 >f; echo k >k; git add .; ci -m base\n\
             git branch x; git branch y\n\
             git checkout -q x\n{x}\ngit add -A; ci -m x1\n\
             git checkout -q y\n{y}\ngit add -A; ci -m y1\n\
             git checkout -q main\ngit reset -q --hard x\nmg y\n\
             git checkout -q -b side y\nmg x\n\
             git checkout -q main\n{main}\ngit add -A; ci -m more-main\n\
             git checkout -q side\n{side}\ngit add -A; ci -m more-side\n\
             git checkout -q main\n"
        )
    };
    let scripts = [
        cross(
            "git mv f g",
            "sed -i 1s/.*/y1/ f",
            "sed -i 9s/.*/m9/ g",
            "sed -i 5s/.*/s5/ g",
        ),
        cross(
            "git mv f g",
            "sed -i 1s/.*/y1/ f",
            "sed -i 1s/.*/m1/ g",
            "sed -i 1s/.*/s1/ g",
        ),
        cross(
            "git mv f g",
            "sed -i 1s/.*/y1/ f",
            "git mv g h",
            "sed -i 5s/.*/s5/ g",
        ),
    ];
    for script in &scripts {
        merges(script, &["merge", "side"]);
    }
}

#[test]
fn a_folder_moved_to_the_majority_destination_takes_added_files_there() {
    let moves = "mkdir b c; git mv a/x b/x; git mv a/y b/y; git mv a/z c/z";
    let script = format!(
        "mkdir a; seq 1 10 >a/x; seq 11 20 >a/y; seq 21 30 >a/z; git add .; git commit -qm base\n\
         git checkout -q -b side\n{moves}\ngit commit -qm side\n\
         git checkout -q main\necho new >a/n; git add -A; git commit -qm main\n"
    );
    merges(&script, &["merge", "side"]);
    merges_with(&script, "merge.directoryRenames true", &["merge", "side"]);
}

#[test]
fn a_folder_emptied_without_renaming_every_file_still_moves_added_files() {
    let script = folder_history(
        "git mv a/x b_x; git rm -q a/y; mkdir b; git mv b_x b/x",
        "echo new >a/n",
    );
    merges(&script, &["merge", "side"]);
}

#[test]
fn both_sides_renaming_a_folder_differently_conflicts_per_file() {
    merges(
        &folder_history("git mv a b", "git mv a c"),
        &["merge", "side"],
    );
    merges(
        &folder_history("git mv a b", "git mv a b"),
        &["merge", "side"],
    );
}

#[test]
fn local_changes_in_the_way_stop_a_merge_that_follows_a_rename() {
    let script = history(MOVE, EDIT_5);
    each_format(&script, |p| {
        p.both_sh("echo local >>a");
        p.same(&["merge", "side"]);
    });
    each_format(&script, |p| {
        p.both_sh("echo local >b");
        p.same(&["merge", "side"]);
    });
    let script = history(EDIT_5, MOVE);
    each_format(&script, |p| {
        p.both_sh("echo local >b");
        p.same(&["merge", "side"]);
    });
}

#[test]
fn what_is_not_matched_is_refused_without_changes() {
    // a rename onto a path the other side also added, with the source edited: git's result is a nested conflict
    let added_and_edited = history(MOVE, &format!("echo other >b; {EDIT_5}"));
    // two renames to one path, one of them edited on the other side
    let two_to_one = "seq 1 10 >a; seq 11 20 >z; git add .; git commit -qm base\n\
        git checkout -q -b side\ngit mv a n; git commit -qm side\n\
        git checkout -q main\ngit mv z n; sed -i 3s/.*/E/ a; git add -A; git commit -qm main\n";
    // a folder split across two destinations with a file added to it
    let split = "mkdir a; seq 1 10 >a/x; seq 11 20 >a/y; git add .; git commit -qm base\n\
        git checkout -q -b side\nmkdir b c; git mv a/x b/x; git mv a/y c/y; git commit -qm side\n\
        git checkout -q main\necho new >a/n; git add -A; git commit -qm main\n";
    // an edited symlink against a renamed one
    let link = "ln -s one l; git add .; git commit -qm base\n\
        git checkout -q -b side\ngit mv l m; git commit -qm side\n\
        git checkout -q main\nrm l; ln -s two l; git add -A; git commit -qm main\n";
    for script in [added_and_edited.as_str(), two_to_one, split, link] {
        each_format(script, |p| p.refused(&["merge", "side"]));
    }
}

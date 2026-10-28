//! Renames, and files meeting folders, in `pyn rebase`, `cherry-pick`, `revert`, `am -3` and `stash apply`, compared
//! with the real command on a copy of the same repository: exit status class, `HEAD`, index stages, work tree and
//! the command's in-progress state. Real git is the oracle for meaning only. Needs `git` on the PATH (tests only).

mod common;
#[path = "common/oracle.rs"]
mod oracle;

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

const IN_PROGRESS: [&str; 4] = [
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
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

/// A snapshot entry (`<mode> [bytes]`) with git's two spellings of the `am -3` base label made one.
fn same_base_label(entry: &str) -> String {
    let Some((mode, rest)) = entry.split_once(' ') else {
        return entry.to_string();
    };
    let Some(list) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) else {
        return entry.to_string();
    };
    let bytes: Vec<u8> = list.split(", ").filter_map(|n| n.parse().ok()).collect();
    let fixed = text(&bytes).replace("constructed fake ancestor", "constructed merge base");
    format!("{mode} {:?}", fixed.as_bytes())
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
        assert!(out.status.success(), "{script}: {}", text(&out.stderr));
        copy_tree(&pair.git, &pair.pyn);
        pair
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
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
            format!("HEAD {}", g(&["rev-parse", "HEAD"]).trim()),
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!(
                "stash {}",
                g(&["rev-parse", "-q", "--verify", "refs/stash"]).trim()
            ),
        ];
        for name in IN_PROGRESS {
            out.push(format!("{name} {}", dir.join(".git").join(name).exists()));
        }
        for (path, content) in snapshot(dir) {
            if path != ".git" && !path.starts_with(".git/") && !path.starts_with(".pyn") {
                out.push(format!("{path} {}", same_base_label(&content)));
            }
        }
        out
    }

    /// Runs the same `git <args>` and `pyn <args>`; the status class and the whole resulting state must agree.
    fn same(&self, args: &[&str]) {
        let (g, p) = (self.git_in(&self.git, args), self.pyn_in(args));
        let what = format!("{args:?}");
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{what}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(self.state(&self.git), self.state(&self.pyn), "{what}");
    }

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

/// A base with a ten line file `a` and a bystander `k`; `side` and `main` are the two branches' changes.
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

/// A cherry-pick and a rebase of `side`'s change onto `main`, each in a fresh pair, with `extra` options.
fn replays(script: &str, setup: &str, extra: &[&str]) {
    let command = |verb: &str, target: &str| {
        let mut args = vec![verb.to_string()];
        args.extend(extra.iter().map(|e| e.to_string()));
        args.push(target.to_string());
        args
    };
    let run = |p: &Pair, args: Vec<String>| {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        p.same(&args);
    };
    each_format(script, |p| {
        p.both_sh(setup);
        run(p, command("cherry-pick", "side"));
    });
    each_format(script, |p| {
        p.both_sh(&format!("git checkout -q side\n{setup}"));
        run(p, command("rebase", "main"));
    });
}

#[test]
fn a_pick_or_rebase_follows_a_rename_on_the_other_side() {
    replays(&history(EDIT_5, MOVE), "", &[]);
    replays(&history(MOVE, EDIT_5), "", &[]);
    replays(
        &history(&format!("{MOVE}; {EDIT_5_B}"), "sed -i 8s/.*/M8/ a"),
        "",
        &[],
    );
    replays(
        &history("sed -i 8s/.*/S8/ a", &format!("{MOVE}; {EDIT_5_B}")),
        "",
        &[],
    );
    replays(
        &history("sed -i 5s/.*/S5/ a", &format!("{MOVE}; sed -i 5s/.*/M5/ b")),
        "",
        &[],
    );
    replays(&history("chmod +x a", MOVE), "", &[]);
}

#[test]
fn a_rename_against_a_delete_conflicts_in_a_replay() {
    replays(&history(MOVE, "git rm -q a"), "", &[]);
    replays(&history("git rm -q a", MOVE), "", &[]);
    replays(
        &history(&format!("{MOVE}; {EDIT_5_B}"), "git rm -q a"),
        "",
        &[],
    );
}

#[test]
fn a_file_renamed_to_two_places_conflicts_in_a_replay() {
    replays(&history(MOVE, "git mv a c"), "", &[]);
    replays(&history(MOVE, MOVE), "", &[]);
}

#[test]
fn a_similar_but_not_identical_rename_is_found_in_a_replay() {
    let reword = "sed -i '2s/.*/two/;7s/.*/seven/' b";
    replays(&history(EDIT_5, &format!("{MOVE}; {reword}")), "", &[]);
}

#[test]
fn the_similarity_threshold_applies_to_a_replay() {
    let reword = "sed -i '1,5s/.*/changed/' b";
    let script = history("sed -i 9s/.*/E9/ a", &format!("{MOVE}; {reword}"));
    for opt in [
        "find-renames=30%",
        "find-renames=70%",
        "rename-threshold=70%",
        "find-renames",
    ] {
        replays(&script, "", &["-X", opt]);
    }
}

#[test]
fn renames_can_be_switched_off_in_a_replay() {
    let script = history(EDIT_5, MOVE);
    replays(&script, "", &["-s", "recursive", "-X", "no-renames"]);
    replays(
        &script,
        "git config merge.renames false",
        &["-s", "recursive"],
    );
    replays(
        &script,
        "git config diff.renames false",
        &["-s", "recursive", "-X", "find-renames"],
    );
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
fn a_file_added_to_a_renamed_folder_follows_it_in_a_replay() {
    for (side, main) in [
        ("echo new >a/n", "git mv a b"),
        ("git mv a b", "echo new >a/n"),
    ] {
        let script = folder_history(side, main);
        replays(&script, "", &[]);
        for value in ["true", "conflict", "false"] {
            replays(
                &script,
                &format!("git config merge.directoryRenames {value}"),
                &[],
            );
        }
    }
}

#[test]
fn both_sides_renaming_a_folder_differently_conflicts_per_file_in_a_replay() {
    replays(&folder_history("git mv a b", "git mv a c"), "", &[]);
}

#[test]
fn a_revert_follows_a_rename() {
    let script = "seq 1 10 >a; echo k >k; git add .; git commit -qm base\n\
        sed -i 5s/.*/E5/ a; git commit -qam edit\n\
        git mv a b; git commit -qm move\n";
    each_format(script, |p| p.same(&["revert", "HEAD~1"]));
    let script = "seq 1 10 >a; echo k >k; git add .; git commit -qm base\n\
        git mv a b; sed -i 5s/.*/E5/ b; git commit -qam move\n\
        sed -i 8s/.*/E8/ b; git commit -qam later\n";
    each_format(script, |p| p.same(&["revert", "HEAD~1"]));
    let script = "seq 1 10 >a; echo k >k; git add .; git commit -qm base\n\
        git mv a b; git commit -qm move\n\
        git mv b c; git commit -qm again\n";
    each_format(script, |p| p.same(&["revert", "HEAD~1"]));
    let script = "seq 1 10 >a; echo k >k; git add .; git commit -qm base\n\
        sed -i 5s/.*/E5/ a; git commit -qam edit\n\
        git mv a b; git commit -qm move\n";
    each_format(script, |p| {
        p.same(&["revert", "-s", "recursive", "-X", "no-renames", "HEAD~1"])
    });
}

#[test]
fn a_continued_rebase_keeps_following_renames() {
    let script = format!(
        "{}git checkout -q side\nsed -i 2s/.*/S2/ a; git commit -qam second\n",
        history("sed -i 5s/.*/S5/ a", &format!("{MOVE}; sed -i 5s/.*/M5/ b"))
    );
    each_format(&script, |p| {
        p.same(&["rebase", "main"]);
        p.both_sh("sed -i '/[<=>|]\\{7\\}/d' b; git add b");
        p.same(&["rebase", "--continue"]);
    });
}

#[test]
fn local_changes_in_the_way_stop_a_replay_that_follows_a_rename() {
    let script = history(EDIT_5, MOVE);
    each_format(&script, |p| {
        p.both_sh("echo local >b");
        p.same(&["cherry-pick", "side"]);
    });
    each_format(&script, |p| {
        p.both_sh("echo local >>b");
        p.same(&["cherry-pick", "side"]);
    });
}

fn mbox(p: &Pair) {
    let patch = p.home.join("side.mbox");
    let out = p.git_in(&p.git, &["format-patch", "-1", "--stdout", "side"]);
    fs::write(patch, out.stdout).unwrap();
}

#[test]
fn an_am_three_way_follows_a_rename() {
    for (side, main) in [
        (EDIT_5, MOVE),
        ("sed -i 5s/.*/S5/ a", "git mv a b; sed -i 5s/.*/M5/ b"),
        (MOVE, EDIT_5),
        (EDIT_5, "git rm -q a"),
    ] {
        each_format(&history(side, main), |p| {
            mbox(p);
            let patch = p.home.join("side.mbox");
            p.same(&["am", "-3", patch.to_str().unwrap()]);
        });
    }
}

/// Stashes are made by `git stash`, which does not work in a sha256 repository, so those scripts run in sha1 only.
fn sha1_only(script: &str, run: impl Fn(&Pair)) {
    run(&Pair::new("sha1", script));
}

fn stashed(change: &str, head: &str) -> String {
    format!(
        "seq 1 10 >a; echo k >k; git add .; git commit -qm base\n{change}\ngit stash -q\n{head}\ngit add -A; git commit -qm head\n"
    )
}

#[test]
fn a_stash_apply_follows_a_rename() {
    for script in [
        stashed(EDIT_5, MOVE),
        stashed(EDIT_5, &format!("{MOVE}; {EDIT_5_B}")),
        stashed("sed -i 5s/.*/S5/ a", &format!("{MOVE}; sed -i 5s/.*/M5/ b")),
        stashed(EDIT_5, "git rm -q a"),
        stashed("echo n >n; git add n", MOVE),
    ] {
        for cmd in ["apply", "pop"] {
            sha1_only(&script, |p| p.same(&["stash", cmd]));
        }
        sha1_only(&script, |p| p.same(&["stash", "apply", "--index"]));
        sha1_only(&script, |p| p.same(&["stash", "branch", "topic"]));
    }
}

#[test]
fn a_file_meeting_a_folder_is_moved_aside_in_a_replay() {
    let script = history("mkdir d; echo in >d/f", "echo file >d");
    replays(&script, "", &[]);
    let script = history("echo file >d", "mkdir d; echo in >d/f");
    replays(&script, "", &[]);
}

#[test]
fn a_file_meeting_a_folder_is_moved_aside_in_a_revert() {
    let script = "seq 1 10 >a; mkdir d; echo in >d/f; git add .; git commit -qm base\n\
        git rm -rq d; echo file >d; git add -A; git commit -qm swap\n\
        git rm -q d; mkdir d; echo again >d/f; git add -A; git commit -qm back\n";
    each_format(script, |p| p.same(&["revert", "HEAD~1"]));
}

#[test]
fn a_file_meeting_a_folder_is_moved_aside_in_a_stash_apply() {
    let script = stashed("mkdir d; echo in >d/f; git add .", "echo file >d");
    sha1_only(&script, |p| p.same(&["stash", "apply"]));
    let script = stashed(
        "rm k; echo file >k; mkdir d; echo in >d/f; git add .",
        "git rm -q k; mkdir k; echo in >k/f",
    );
    sha1_only(&script, |p| p.same(&["stash", "apply"]));
}

#[test]
fn what_is_not_matched_is_refused_without_changes() {
    let added_and_edited = history(MOVE, &format!("echo other >b; {EDIT_5}"));
    let split = "mkdir a; seq 1 10 >a/x; seq 11 20 >a/y; git add .; git commit -qm base\n\
        git checkout -q -b side\nmkdir b c; git mv a/x b/x; git mv a/y c/y; git commit -qm side\n\
        git checkout -q main\necho new >a/n; git add -A; git commit -qm main\n";
    for script in [added_and_edited.as_str(), split] {
        each_format(script, |p| p.refused(&["cherry-pick", "side"]));
    }
}

/// `rebase --rebase-merges` merges with git's recursive strategy, so a merge it recreates can have several bases.
#[test]
fn a_recreated_merge_over_a_criss_cross_uses_a_virtual_base() {
    let clock = "T=1700000000\n\
        tick() { T=$((T+100)); GIT_AUTHOR_DATE=\"$T +0000\"; GIT_COMMITTER_DATE=\"$T +0000\"; export GIT_AUTHOR_DATE GIT_COMMITTER_DATE; }\n\
        ci() { tick; git commit -q --allow-empty \"$@\"; }\n\
        mg() { tick; git merge -q --no-edit \"$@\" >/dev/null 2>&1 || true; }\n";
    let cross = |x: &str, y: &str, topic: &str, after: &str| {
        format!(
            "{clock}seq 1 9 >f; echo k >k; git add .; ci -m base\n\
             git branch x; git branch y; git branch root\n\
             git checkout -q x\n{x}\ngit add -A; ci -m x1\n\
             git checkout -q y\n{y}\ngit add -A; ci -m y1\n\
             git checkout -q -b other y\nmg x\n\
             git checkout -q -b topic root\n\
             {topic}\ngit add -A; ci -m t1\nmg other\n\
             git checkout -q main\ngit reset -q --hard x\nmg y\n{after}\ngit add -A; ci -m more\n\
             git checkout -q topic\n"
        )
    };
    let scripts = [
        cross(
            "sed -i 1s/.*/x1/ f",
            "sed -i 9s/.*/y9/ f",
            "echo t >t",
            "echo m >m",
        ),
        cross("git mv f g", "sed -i 1s/.*/y1/ f", "echo t >t", "echo m >m"),
        cross(
            "sed -i 1s/.*/x1/ f",
            "sed -i 9s/.*/y9/ f",
            "echo t >t",
            "sed -i 5s/.*/m5/ f",
        ),
    ];
    for script in &scripts {
        each_format(script, |p| {
            p.same(&["rebase", "-r", "main"]);
            let tip = text(
                &p.git_in(&p.pyn, &["rev-list", "--parents", "-n1", "HEAD"])
                    .stdout,
            );
            assert_eq!(
                tip.split_whitespace().count(),
                3,
                "a merge commit is recreated"
            );
        });
    }
}

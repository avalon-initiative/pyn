//! The diffstat `pyn merge` prints after a fast-forward or a merge commit, and the messages for local changes that
//! block it, compared with the real `git merge` on a copy of the same repository: exit status, standard output,
//! standard error and resulting `HEAD`. Needs `git` on the PATH (tests only); real git is the oracle for meaning.

mod common;

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

struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
}

impl Pair {
    /// A repository built by `script` (run by `sh` with real git, ending on `main`), and a copy of it.
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
        // Background auto-gc would remove loose objects while `copy_tree` runs.
        let init = format!(
            "git init -q -b main --object-format={format} .\n\
             git config gc.auto 0\ngit config maintenance.auto false\n{script}"
        );
        let out = Command::new("sh")
            .args(["-e", "-c", &init])
            .current_dir(&pair.git)
            .env("HOME", &pair.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        copy_tree(&pair.git, &pair.pyn);
        pair
    }

    fn git(&self, dir: &Path, args: &[&str], columns: Option<&str>) -> Output {
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY);
        if let Some(c) = columns {
            cmd.env("COLUMNS", c);
        }
        cmd.output().expect("git is installed")
    }

    fn pyn(&self, args: &[&str], columns: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pyn"));
        cmd.args(args)
            .current_dir(&self.pyn)
            .env("HOME", &self.home)
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO");
        if let Some(c) = columns {
            cmd.env("COLUMNS", c);
        }
        cmd.output().unwrap()
    }

    /// Runs `git <args>` and `pyn <args>`; status, both output streams and `HEAD` must agree.
    fn same(&self, args: &[&str]) {
        self.same_at(args, None);
    }

    fn same_at(&self, args: &[&str], columns: Option<&str>) {
        let g = self.git(&self.git, args, columns);
        let p = self.pyn(args, columns);
        let what = format!("{args:?} COLUMNS={columns:?}");
        assert_eq!(
            g.status.code(),
            p.status.code(),
            "{what}: {}",
            text(&p.stderr)
        );
        assert_eq!(text(&g.stdout), text(&p.stdout), "{what} stdout");
        assert_eq!(text(&g.stderr), text(&p.stderr), "{what} stderr");
        let head = |dir: &Path| text(&self.git(dir, &["rev-parse", "HEAD"], None).stdout);
        assert_eq!(head(&self.git), head(&self.pyn), "{what} HEAD");
    }
}

/// Output with abbreviated ids replaced, since a merge commit's id depends on the committer clock.
fn text(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    s.lines()
        .map(|l| match l.strip_prefix("Updating ") {
            Some(_) => "Updating X..Y".to_string(),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const SIDE: &str = "git checkout -q -b side\n";
const BACK: &str = "git checkout -q main\n";

fn both_formats(script: &str, run: impl Fn(&Pair)) {
    for format in common::gitfix::FORMATS {
        run(&Pair::new(format, script));
    }
}

#[test]
fn a_fast_forward_prints_git_s_diffstat_with_summary_lines() {
    let script = format!(
        "mkdir -p src/deep/er; seq 1 50 >src/lib.rs; echo old >gone.txt; echo x >exec.sh; seq 1 10 >ren.txt; \
         printf '\\0bin' >blob.bin; echo link >t; git add .; git commit -qm one\n{SIDE}\
         seq 1 200 >src/lib.rs; git rm -q gone.txt; chmod +x exec.sh; git mv ren.txt src/deep/er/renamed.txt; \
         seq 1 120 >src/deep/er/a_very_long_file_name_that_goes_on_and_on_and_on_for_a_while_to_exercise_it.txt; \
         printf '\\0binary-longer' >blob.bin; echo n >new.txt; ln -s new.txt newlink; : >empty; \
         git add -A; git commit -qm two\n{BACK}"
    );
    both_formats(&script, |p| p.same(&["merge", "side"]));
}

#[test]
fn widths_follow_columns_and_the_graph_width_setting() {
    let script = format!(
        "seq 1 300 >a; seq 1 5 >c; git add .; git commit -qm one\n{SIDE}seq 1 5 >a; seq 1 400 >b; echo x >>c; \
         git add -A; git commit -qm two\n{BACK}"
    );
    for columns in ["20", "40", "120", "200"] {
        both_formats(&script, |p| p.same_at(&["merge", "side"], Some(columns)));
    }
    let narrow = format!("{script}git config diff.statGraphWidth 10\n");
    both_formats(&narrow, |p| p.same(&["merge", "side"]));
}

#[test]
fn renames_are_found_by_content_and_by_similarity() {
    let script = format!(
        "seq 1 100 >big; seq 1 30 >small; seq 1 40 >f; seq 1 40 >g; echo y >same; \
         for i in $(seq 1 20); do echo \"line $i of text in a\"; done >aa; \
         printf '\\0\\1\\2bin' >b.bin; : >empty; mkdir d; seq 1 9 >d/z\n\
         git add .; git commit -qm one\n{SIDE}\
         git rm -q big small aa f; (seq 1 60; echo extra) >bigger; seq 1 25 >tiny; \
         sed 's/5/X/' g >h; rm g; sed 's/1/Q/' aa >/dev/null 2>&1 || true; \
         for i in $(seq 1 20); do echo \"line $i of text in a\"; done | sed 's/1/Q/' >aa2; \
         git mv same d/same2; git mv b.bin c.bin; git mv empty moved_empty; git mv d/z e; \
         git add -A; git commit -qm two\n{BACK}"
    );
    both_formats(&script, |p| p.same(&["merge", "side"]));
}

#[test]
fn a_merge_commit_prints_the_strategy_line_and_the_diffstat() {
    let script = format!(
        "seq 1 5 >a; seq 1 5 >b; git add .; git commit -qm one\n{SIDE}echo 6 >>a; seq 6 8 >>b; git commit -qam two\n\
         {BACK}echo m >m; git add m; git commit -qm three\n"
    );
    both_formats(&script, |p| p.same(&["merge", "-m", "msg", "side"]));
    both_formats(&script, |p| {
        p.same(&["merge", "--no-ff", "-m", "msg", "side"])
    });
}

#[test]
fn stat_flags_and_merge_stat_decide_whether_it_prints() {
    let script = format!(
        "seq 1 5 >a; git add .; git commit -qm one\n{SIDE}echo 6 >>a; echo n >n; git add -A; git commit -qm two\n\
         {BACK}echo m >m; git add m; git commit -qm three\n"
    );
    let with = |config: &str| format!("{script}{config}");
    for args in [
        vec!["merge", "-m", "x", "-n", "side"],
        vec!["merge", "-m", "x", "--no-stat", "side"],
        vec!["merge", "-m", "x", "--stat", "side"],
        vec!["merge", "-m", "x", "--no-stat", "--stat", "side"],
        vec!["merge", "-m", "x", "--stat", "--no-stat", "side"],
        vec!["merge", "-m", "x", "--no-ff", "-n", "side"],
    ] {
        both_formats(&script, |p| p.same(&args));
    }
    let off = with("git config merge.stat false\n");
    both_formats(&off, |p| p.same(&["merge", "-m", "x", "side"]));
    both_formats(&off, |p| p.same(&["merge", "-m", "x", "--stat", "side"]));
    let on = with("git config merge.stat true\n");
    both_formats(&on, |p| p.same(&["merge", "-m", "x", "-n", "side"]));
    let ff = format!(
        "seq 1 5 >a; git add .; git commit -qm one\n{SIDE}echo 6 >>a; git commit -qam two\n{BACK}"
    );
    for args in [vec!["merge", "-n", "side"], vec!["merge", "--stat", "side"]] {
        both_formats(&ff, |p| p.same(&args));
    }
    let ff_off = format!("{ff}git config merge.stat false\n");
    both_formats(&ff_off, |p| p.same(&["merge", "side"]));
}

#[test]
fn blocked_fast_forwards_name_each_case_as_git_does() {
    let cases = [
        // a modified file and an untracked file in the way
        ("echo local >a; echo mine >c", "echo a2 >a; echo c >c"),
        // a staged change
        ("echo local >a; git add a", "echo a2 >a"),
        // an untracked file where a folder arrives, and a folder with untracked files where a file arrives
        ("echo mine >d", "mkdir d; echo x >d/x"),
        ("mkdir d; echo u >d/u", "echo f >d"),
    ];
    for (local, remote) in cases {
        let script = format!(
            "echo a >a; echo b >b; git add .; git commit -qm one\n{SIDE}{remote}; git add -A; git commit -qm two\n\
             {BACK}{local}\n"
        );
        both_formats(&script, |p| p.same(&["merge", "side"]));
    }
    // a tracked file that becomes a folder
    let script = format!(
        "echo d >d; git add .; git commit -qm one\n{SIDE}git rm -q d; mkdir d; echo x >d/x; git add -A; \
         git commit -qm two\n{BACK}echo mod >d\n"
    );
    both_formats(&script, |p| p.same(&["merge", "side"]));
}

#[test]
fn blocked_merge_commits_split_the_index_check_from_the_work_tree_check() {
    let cases = [
        // unstaged edit and untracked file
        ("echo local >a; echo mine >c", "echo a2 >a; echo c >c"),
        // staged edits to several files, one of them untouched by the merge
        ("echo b2 >b; git add b; echo z2 >z; git add z", "echo a2 >a"),
        // an untracked file only
        ("echo mine >c", "echo c >c"),
        // a folder with untracked files where a file arrives
        ("mkdir d; echo u >d/u", "echo f >d"),
    ];
    for (local, remote) in cases {
        let script = format!(
            "echo a >a; echo b >b; echo z >z; git add .; git commit -qm one\n{SIDE}{remote}; git add -A; \
             git commit -qm two\n{BACK}echo m >m; git add m; git commit -qm three\n{local}\n"
        );
        both_formats(&script, |p| p.same(&["merge", "-m", "x", "side"]));
    }
}

#[test]
fn unfinished_merges_are_refused_with_git_s_messages() {
    let conflict = format!(
        "echo a >a; git add .; git commit -qm one\n{SIDE}echo s >a; git commit -qam s\n{BACK}echo m >a; \
         git commit -qam m; git merge side >/dev/null 2>&1 || true\n"
    );
    both_formats(&conflict, |p| p.same(&["merge", "side"]));
    let concluded = format!(
        "echo a >a; echo b >b; git add .; git commit -qm one\n{SIDE}echo a2 >a; git commit -qam s\n{BACK}\
         echo b2 >b; git commit -qam m; git merge --no-commit side >/dev/null 2>&1\n"
    );
    both_formats(&concluded, |p| p.same(&["merge", "side"]));
}

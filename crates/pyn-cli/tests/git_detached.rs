//! A detached `HEAD` on a git repository, compared with the real git on a twin repository: entering it with
//! `switch --detach`, `checkout <commit>` and `checkout --detach`, `status`, `log` and `commit` while on it, and
//! leaving it with the commits-left-behind warning. Exit status, refs, `HEAD`, reflogs and messages must agree.
//! Needs `git` on the PATH (tests only).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::copy_tree;

const BASE_DATE: i64 = 1_700_000_000;

struct Env {
    home: PathBuf,
}

impl Env {
    fn identity(date: i64) -> [(&'static str, String); 6] {
        [
            ("GIT_AUTHOR_NAME", "T".into()),
            ("GIT_AUTHOR_EMAIL", "t@example.com".into()),
            ("GIT_COMMITTER_NAME", "T".into()),
            ("GIT_COMMITTER_EMAIL", "t@example.com".into()),
            ("GIT_AUTHOR_DATE", format!("{date} +0000")),
            ("GIT_COMMITTER_DATE", format!("{date} +0000")),
        ]
    }

    fn git_at(&self, cwd: &Path, args: &[&str], date: i64) -> Output {
        Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .envs(Self::identity(date))
            .output()
            .expect("git is installed")
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.git_at(cwd, args, BASE_DATE);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn pyn_at(&self, cwd: &Path, args: &[&str], date: i64) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("TZ", "UTC")
            .env("GIT_EDITOR", "true")
            .envs(Self::identity(date))
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn commit_file(env: &Env, dir: &Path, name: &str, body: &str, message: &str) {
    fs::write(dir.join(name), body).unwrap();
    env.ok(dir, &["add", "-A"]);
    env.ok(dir, &["commit", "-q", "-m", message]);
}

fn logs(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for entry in rd {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(name, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let root = dir.join(".git/logs");
    let mut out = BTreeMap::new();
    walk(&root, &root, &mut out);
    out
}

struct Pair {
    _tmp: tempfile::TempDir,
    env: Env,
    git: PathBuf,
    pyn: PathBuf,
    step: i64,
}

impl Pair {
    /// `main` with c1, c2, c3 and a `side` branch at c2.
    fn build(format: &str) -> Self {
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
        commit_file(&env, &git, "a", "one\n", "c1");
        commit_file(&env, &git, "a", "two\n", "c2");
        env.ok(&git, &["branch", "side"]);
        commit_file(&env, &git, "b", "b\n", "c3");
        let pyn = root.join("pyn");
        copy_tree(&git, &pyn);
        let _ = env.git_at(&git, &["update-index", "-q", "--refresh"], BASE_DATE);
        Self {
            _tmp: tmp,
            env,
            git,
            pyn,
            step: 0,
        }
    }

    /// Runs `args` in both with a clock that moves on each call; exit status, messages, `HEAD`, refs and reflogs
    /// must agree. A failure compares only the status.
    fn same(&mut self, args: &[&str]) -> String {
        self.step += 1;
        let date = BASE_DATE + 100 * self.step;
        let g = self.env.git_at(&self.git, args, date);
        let p = self.env.pyn_at(&self.pyn, args, date);
        let what = format!("step {}: {args:?}", self.step);
        assert_eq!(
            p.status.code(),
            g.status.code(),
            "{what}: exit status; pyn said {}; git said {}",
            text(&p.stderr),
            text(&g.stderr)
        );
        if g.status.success() {
            assert_eq!(
                text(&p.stderr).replace("pyn ", "git "),
                text(&g.stderr),
                "{what}: messages (pyn, then git)"
            );
        }
        for probe in [
            &["status", "-sb"][..],
            &["for-each-ref"][..],
            &["rev-parse", "HEAD"][..],
        ] {
            let (pr, gr) = if probe[0] == "status" {
                let run = |env: &Env, dir: &Path, pyn: bool| {
                    let o = if pyn {
                        env.pyn_at(dir, probe, date)
                    } else {
                        env.git_at(dir, probe, date)
                    };
                    text(&o.stdout)
                };
                (
                    run(&self.env, &self.pyn, true),
                    run(&self.env, &self.git, false),
                )
            } else {
                (
                    text(&self.env.git_at(&self.pyn, probe, date).stdout),
                    text(&self.env.git_at(&self.git, probe, date).stdout),
                )
            };
            assert_eq!(pr, gr, "{what}: {probe:?}");
        }
        assert_eq!(logs(&self.pyn), logs(&self.git), "{what}: reflogs");
        text(&g.stderr)
    }
}

fn both(format: &str, steps: &[&[&str]]) {
    let mut pair = Pair::build(format);
    for step in steps {
        pair.same(step);
    }
}

#[test]
fn entering_by_every_form() {
    for format in ["sha1", "sha256"] {
        both(
            format,
            &[
                &["switch", "--detach", "HEAD~1"],
                &["switch", "main"],
                &["checkout", "HEAD~2"],
                &["checkout", "main"],
                &["checkout", "--detach"],
                &["checkout", "--detach", "side"],
                &["switch", "--detach"],
                &["switch", "--detach", "main"],
                &["switch", "side"],
                &["checkout", "-"],
                &["checkout", "-"],
                &["switch", "main"],
                &["checkout", "side^0"],
            ],
        );
    }
}

#[test]
fn switch_dash_onto_a_detached_position_is_refused() {
    both(
        "sha1",
        &[
            &["switch", "--detach", "HEAD~1"],
            &["switch", "main"],
            &["switch", "-"],
            &["switch", "@{-1}"],
            &["checkout", "-"],
            &["checkout", "-"],
        ],
    );
}

#[test]
fn commits_on_a_detached_head_and_leaving_them() {
    for format in ["sha1", "sha256"] {
        both(
            format,
            &[
                &["switch", "--detach", "HEAD~1"],
                &["commit", "--allow-empty", "-m", "d1"],
                &["switch", "main"],
                &["checkout", "-"],
                &["commit", "--allow-empty", "-m", "d2"],
                &["commit", "--amend", "--allow-empty", "-m", "d2b"],
                &["commit", "--allow-empty", "-m", "d3"],
                &["checkout", "main"],
                &["checkout", "--detach"],
                &["checkout", "-"],
                &["switch", "-c", "keep"],
                &["switch", "main"],
            ],
        );
    }
}

#[test]
fn many_commits_left_behind_are_cut_off() {
    let mut steps: Vec<&[&str]> = vec![&["switch", "--detach"]];
    for _ in 0..6 {
        steps.push(&["commit", "--allow-empty", "-m", "d"]);
    }
    steps.push(&["switch", "side"]);
    both("sha1", &steps);
}

#[test]
fn detached_to_detached_and_with_a_branch_keeping_the_commits() {
    both(
        "sha1",
        &[
            &["switch", "--detach", "HEAD~2"],
            &["commit", "--allow-empty", "-m", "d1"],
            &["checkout", "main"],
            &["checkout", "-"],
            &["checkout", "main~1"],
            &["checkout", "main"],
            &["switch", "--detach", "side"],
            &["branch", "keeper"],
            &["commit", "--allow-empty", "-m", "d2"],
            &["branch", "keeper2"],
            &["switch", "keeper"],
        ],
    );
}

#[test]
fn the_advice_can_be_turned_off() {
    let mut pair = Pair::build("sha1");
    for dir in [&pair.git, &pair.pyn] {
        pair.env
            .ok(dir, &["config", "advice.detachedHead", "false"]);
    }
    for step in [
        &["checkout", "HEAD~1"][..],
        &["commit", "--allow-empty", "-m", "d1"],
        &["switch", "main"],
    ] {
        pair.same(step);
    }
}

#[test]
fn the_messages_are_git_s_words() {
    let mut pair = Pair::build("sha1");
    let entered = pair.same(&["checkout", "HEAD~1"]);
    assert!(
        entered.contains("You are in 'detached HEAD' state."),
        "{entered}"
    );
    assert!(entered.contains("HEAD is now at "), "{entered}");
    pair.same(&["commit", "--allow-empty", "-m", "d1"]);
    let left = pair.same(&["switch", "main"]);
    assert!(
        left.contains("Warning: you are leaving 1 commit behind"),
        "{left}"
    );
    assert!(left.contains("git branch <new-branch-name> "), "{left}");
}

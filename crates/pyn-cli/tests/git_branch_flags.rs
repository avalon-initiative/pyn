//! The branch and switch flags of a git repository (`branch -f -M -c -C -a -r -u --unset-upstream`, `switch -C
//! --orphan`), compared with the real git on a twin repository: exit status, output, refs, `HEAD`, reflogs and
//! the configuration file must agree. Needs `git` on the PATH (tests only).

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
    /// `main` with c1, c2, c3, a `side` branch at c2, and an `origin` remote with tracking refs.
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
        env.ok(&git, &["remote", "add", "origin", "/nonexistent"]);
        env.ok(&git, &["update-ref", "refs/remotes/origin/main", "main"]);
        env.ok(&git, &["update-ref", "refs/remotes/origin/dev", "side"]);
        env.ok(&git, &["update-ref", "refs/remotes/stray/x", "main~2"]);
        env.ok(
            &git,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
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

    fn both_config(&self, args: &[&str]) {
        for dir in [&self.git, &self.pyn] {
            self.env.ok(dir, &[&["config"], args].concat());
        }
    }

    /// Runs `args` in both with a clock that moves on each call; exit status, output, `HEAD`, refs, reflogs and
    /// the configuration must agree.
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
        if args[0] != "commit" {
            assert_eq!(text(&p.stdout), text(&g.stdout), "{what}: output");
        }
        if g.status.code() != Some(129) {
            let said = text(&p.stderr)
                .replace("pyn ", "git ")
                .replace(self.pyn.to_str().unwrap(), self.git.to_str().unwrap())
                .replace("wt-pyn", "wt-git");
            assert_eq!(
                calm(&said),
                calm(&text(&g.stderr)),
                "{what}: messages (pyn, then git)"
            );
        }
        for probe in [
            &["status", "-sb"][..],
            &["for-each-ref"][..],
            &["rev-parse", "HEAD"][..],
            &["ls-files", "-s"][..],
            &["symbolic-ref", "-q", "HEAD"][..],
        ] {
            let pr = text(&self.env.git_at(&self.pyn, probe, date).stdout);
            let gr = text(&self.env.git_at(&self.git, probe, date).stdout);
            assert_eq!(pr, gr, "{what}: {probe:?}");
        }
        let (pl, gl) = (logs(&self.pyn), logs(&self.git));
        for key in pl.keys().chain(gl.keys()) {
            if pl.get(key) != gl.get(key) {
                let short = |v: Option<&String>| {
                    v.map(|t| {
                        t.lines()
                            .map(|l| {
                                format!("{} {}", &l[..7], l.split('\t').next_back().unwrap_or(""))
                            })
                            .collect::<Vec<_>>()
                    })
                };
                panic!(
                    "{what}: reflog of {key}\n pyn: {:#?}\n git: {:#?}",
                    short(pl.get(key)),
                    short(gl.get(key))
                );
            }
        }
        assert_eq!(
            fs::read_to_string(self.pyn.join(".git/config")).unwrap(),
            fs::read_to_string(self.git.join(".git/config")).unwrap(),
            "{what}: config"
        );
        assert_eq!(files(&self.pyn), files(&self.git), "{what}: work tree");
        text(&g.stderr)
    }
}

/// Drops what git versions word differently: the ref-syntax hint, the advice switch line, trailing blanks.
fn calm(message: &str) -> String {
    message
        .lines()
        .filter(|l| !l.contains("check-ref-format") && !l.contains("Disable this message"))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn both(format: &str, steps: &[&[&str]]) {
    let mut pair = Pair::build(format);
    for step in steps {
        pair.same(step);
    }
}

#[test]
fn force_creates_and_resets() {
    for format in ["sha1", "sha256"] {
        both(
            format,
            &[
                &["branch", "-f", "fresh"],
                &["branch", "-f", "side", "main"],
                &["branch", "-f", "side", "main~2"],
                &["branch", "-f", "side", "side"],
                &["branch", "--force", "other", "side"],
                &["branch", "-f", "main", "side"],
                &["branch", "-f", "main"],
                &["branch", "-f", "nosuch", "nosuch2"],
                &["branch", "side", "main"],
                &["branch", "-f", "-d", "other"],
                &["branch", "-f", "bad..name"],
                &["branch", "-f", "-D", "fresh", "side"],
                &["branch", "-f"],
            ],
        );
    }
}

#[test]
fn forced_rename_overwrites_and_keeps_the_settings() {
    let mut pair = Pair::build("sha1");
    pair.both_config(&["branch.side.remote", "origin"]);
    pair.both_config(&["branch.side.merge", "refs/heads/dev"]);
    pair.both_config(&["branch.main.description", "the trunk"]);
    pair.both_config(&["branch.other.remote", "stray"]);
    for step in [
        &["branch", "other", "main~1"][..],
        &["branch", "-m", "side", "other"],
        &["branch", "-M", "side", "other"],
        &["branch", "-M", "other", "main"],
        &["branch", "-M", "main", "main"],
        &["branch", "-m", "main", "main"],
        &["branch", "-M", "nosuch", "zed"],
        &["branch", "-M"],
        &["branch", "-M", "a", "b", "c"],
        &["branch", "other", "main"],
        &["branch", "-M", "main", "other"],
        &["branch", "-M", "other", "main"],
        &["branch", "--move", "--force", "main", "last"],
        &["branch", "-M", "last", "bad..name"],
    ] {
        pair.same(step);
    }
}

#[test]
fn forced_rename_of_a_branch_checked_out_in_another_work_tree() {
    let mut pair = Pair::build("sha1");
    for (dir, tree) in [(&pair.git, "../wt-git"), (&pair.pyn, "../wt-pyn")] {
        pair.env.ok(dir, &["worktree", "add", "-q", tree, "side"]);
    }
    pair.same(&["branch", "-M", "main", "side"]);
    pair.same(&["branch", "-f", "side", "main"]);
    pair.same(&["branch", "-C", "main", "side"]);
    pair.same(&["branch", "-c", "side", "copy"]);
}

#[test]
fn copy_carries_the_reflog_and_the_settings() {
    for format in ["sha1", "sha256"] {
        let mut pair = Pair::build(format);
        pair.both_config(&["branch.side.remote", "origin"]);
        pair.both_config(&["branch.side.rebase", "true"]);
        pair.both_config(&["branch.main.description", "trunk # not a comment"]);
        for step in [
            &["branch", "-c", "side", "side2"][..],
            &["branch", "-c", "side", "side2"],
            &["branch", "-C", "side", "side2"],
            &["branch", "-C", "main", "side2"],
            &["branch", "-c", "main", "main"],
            &["branch", "-C", "main", "main"],
            &["branch", "-c", "other"],
            &["branch", "-c", "nosuch", "q"],
            &["branch", "-c"],
            &["branch", "-c", "a", "b", "c"],
            &["branch", "-c", "main", "bad..name"],
            &["branch", "--copy", "--force", "side", "main"],
            &["branch", "--copy", "side", "third"],
            &["switch", "--detach"],
            &["branch", "-c", "x"],
            &["branch", "-m", "x"],
            &["branch", "-d", "side", "-c", "x"],
        ] {
            pair.same(step);
        }
    }
}

#[test]
fn listing_remote_tracking_branches() {
    let mut pair = Pair::build("sha1");
    for step in [
        &["branch", "-r"][..],
        &["branch", "-a"],
        &["branch", "-ra"],
        &["branch", "-r", "-v"],
        &["branch", "-a", "-v"],
        &["branch", "-a", "--list", "o*"],
        &["branch", "-r", "--list", "o*"],
        &["branch", "-r", "-l", "origin/m*"],
        &["branch", "-a", "-l", "side", "origin/dev"],
        &["branch", "-r", "--list", "main"],
        &["branch", "-a", "--list", "remotes/*"],
        &["branch", "--list", "-r"],
        &["branch", "-r", "foo"],
        &["branch", "-a", "foo"],
        &["branch", "-a", "--show-current"],
        &["branch", "-a", "-d", "x"],
        &["switch", "--detach", "HEAD~1"],
        &["branch", "-a"],
        &["branch", "-r"],
        &["branch", "-a", "-v"],
    ] {
        pair.same(step);
    }
    let all = text(
        &pair
            .env
            .pyn_at(&pair.pyn, &["branch", "-a"], BASE_DATE)
            .stdout,
    );
    assert!(all.contains("remotes/origin/HEAD -> origin/main"), "{all}");
}

#[test]
fn upstream_is_written_as_git_writes_it() {
    for format in ["sha1", "sha256"] {
        let mut pair = Pair::build(format);
        pair.both_config(&["remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
        pair.both_config(&["branch.side.description", "kept"]);
        for step in [
            &["branch", "-u", "origin/main"][..],
            &["branch", "-u", "origin/main"],
            &["branch", "--set-upstream-to=origin/dev", "side"],
            &["branch", "-u", "main", "side"],
            &["branch", "-u", "refs/remotes/origin/dev", "main"],
            &["branch", "-u", "side", "side"],
            &["branch", "-u", "main"],
            &["branch", "-u", "HEAD"],
            &["branch", "-u", "origin/dev", "nosuch"],
            &["branch", "-u", "nosuch"],
            &["branch", "-u", "origin/nonex", "side"],
            &["branch", "-u", "stray/x"],
            &["branch", "-u", "a", "b", "c"],
            &["branch", "--unset-upstream", "side"],
            &["branch", "--unset-upstream", "side"],
            &["branch", "--unset-upstream", "nosuch"],
            &["branch", "--unset-upstream", "a", "b"],
            &["branch", "-u", "origin/main", "side"],
            &["branch", "--unset-upstream"],
            &["branch", "--unset-upstream"],
            &["branch", "-u", "origin/main", "-d", "x"],
            &["branch", "-u", "origin/main", "newone"],
            &["switch", "--detach"],
            &["branch", "-u", "origin/main"],
            &["branch", "--unset-upstream"],
        ] {
            pair.same(step);
        }
        let config = fs::read_to_string(pair.pyn.join(".git/config")).unwrap();
        assert!(config.contains("[branch \"side\"]"), "{config}");
    }
}

#[test]
fn upstream_of_an_unborn_branch_and_an_advice_switch() {
    let mut pair = Pair::build("sha1");
    pair.both_config(&["advice.setUpstreamFailure", "false"]);
    pair.same(&["switch", "--orphan", "fresh"]);
    pair.same(&["branch", "-u", "main"]);
    pair.same(&["branch", "-u", "main", "side"]);
    pair.same(&["branch", "-u", "nosuch", "side"]);
    pair.same(&["branch", "-u", "main", "nosuch"]);
}

#[test]
fn switch_force_create() {
    for format in ["sha1", "sha256"] {
        both(
            format,
            &[
                &["switch", "-C", "side"],
                &["switch", "-C", "side", "main~1"],
                &["switch", "-C", "fresh"],
                &["switch", "-C", "fresh", "main~2"],
                &["switch", "--force-create", "fresh", "main"],
                &["switch", "-C", "main", "main"],
                &["switch", "-C", "other", "HEAD~9"],
                &["switch", "-C", "bad..name"],
                &["switch", "-C", "x", "-c", "y"],
                &["switch", "-C", "x", "--detach"],
                &["switch", "-C", "x", "--orphan", "y"],
                &["switch", "-C", "x", "main"],
                &["switch", "-"],
                &["switch", "-C", "side", "x"],
            ],
        );
    }
}

#[test]
fn switch_force_create_carries_changes_and_refuses_conflicts() {
    let mut pair = Pair::build("sha1");
    for dir in [&pair.git, &pair.pyn] {
        fs::write(dir.join("b"), "local\n").unwrap();
        fs::write(dir.join("loose"), "x\n").unwrap();
    }
    pair.same(&["switch", "-C", "side", "main"]);
    pair.same(&["switch", "-C", "elsewhere", "side"]);
    pair.same(&["switch", "-C", "main", "main~2"]);
    pair.same(&["switch", "-f", "-C", "main", "main~2"]);
    pair.same(&["switch", "-C", "main", "main"]);
}

#[test]
fn switch_force_create_from_a_detached_head() {
    both(
        "sha1",
        &[
            &["switch", "--detach", "HEAD~1"],
            &["commit", "--allow-empty", "-m", "d1"],
            &["switch", "-C", "kept"],
            &["switch", "--detach", "main"],
            &["commit", "--allow-empty", "-m", "d2"],
            &["switch", "-C", "side", "main"],
        ],
    );
}

#[test]
fn orphan_branches() {
    for format in ["sha1", "sha256"] {
        both(
            format,
            &[
                &["switch", "--orphan", "o1"],
                &["commit", "--allow-empty", "-m", "root"],
                &["switch", "main"],
                &["switch", "--orphan", "o2"],
                &["branch", "-a"],
                &["switch", "-"],
                &["switch", "--orphan", "o2"],
                &["switch", "--orphan", "o2", "side"],
                &["switch", "--orphan", "main"],
                &["switch", "--orphan", "bad..name"],
                &["switch", "--orphan", "x", "-c", "y"],
                &["switch", "--orphan", "x", "-d"],
                &["switch", "--orphan", "o3"],
                &["switch", "--orphan", "o4"],
            ],
        );
    }
}

#[test]
fn orphan_leaves_untracked_files_and_checks_local_changes() {
    let mut pair = Pair::build("sha1");
    for dir in [&pair.git, &pair.pyn] {
        fs::create_dir_all(dir.join("d")).unwrap();
        fs::write(dir.join("d/q"), "q\n").unwrap();
        fs::write(dir.join("untracked"), "u\n").unwrap();
    }
    pair.env.ok(&pair.git, &["add", "d"]);
    pair.env.ok(&pair.pyn, &["add", "d"]);
    pair.env.ok(&pair.git, &["commit", "-qm", "c4"]);
    pair.env.ok(&pair.pyn, &["commit", "-qm", "c4"]);
    for dir in [&pair.git, &pair.pyn] {
        fs::write(dir.join("a"), "changed\n").unwrap();
    }
    pair.same(&["switch", "--orphan", "o1"]);
    pair.same(&["switch", "-f", "--orphan", "o1"]);
    pair.same(&["switch", "-f", "main"]);
    pair.same(&["switch", "--orphan", "o2"]);
}

#[test]
fn orphan_from_a_detached_head() {
    both(
        "sha1",
        &[
            &["switch", "--detach", "HEAD~1"],
            &["commit", "--allow-empty", "-m", "d1"],
            &["switch", "--orphan", "o1"],
            &["switch", "main"],
        ],
    );
}

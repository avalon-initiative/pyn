//! `pyn reflog` and the `@{...}` revision forms on a git repository, compared with the real git on a twin
//! repository: the reflog lines every write leaves behind, what `HEAD@{n}`, `@{-n}`, `@{u}` and `@{push}` name, and
//! what `reflog show`, `expire` and `delete` print and leave. Needs `git` on the PATH (tests only).

mod common;
#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::copy_tree;

const BASE_DATE: i64 = 1_700_000_000;

struct Env {
    home: PathBuf,
    ssh: PathBuf,
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
            .env("GIT_SSH_COMMAND", &self.ssh)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .envs(Self::identity(date))
            .output()
            .expect("git is installed")
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.git_at(cwd, args, BASE_DATE)
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.git(cwd, args);
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
            .env("GIT_SSH_COMMAND", &self.ssh)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .envs(Self::identity(date))
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .output()
            .unwrap()
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Output {
        self.pyn_at(cwd, args, BASE_DATE)
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn write(dir: &Path, name: &str, body: &str) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, body).unwrap();
}

fn commit(env: &Env, dir: &Path, name: &str, body: &str, message: &str) {
    write(dir, name, body);
    env.ok(dir, &["add", "-A"]);
    env.ok(dir, &["commit", "-q", "-m", message]);
}

/// Every reflog file under `.git/logs`, with its lines.
fn logs(dir: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for entry in rd {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                if (!name.ends_with("/HEAD") || name == "HEAD") && name != "AUTO_MERGE" {
                    out.insert(name, fs::read_to_string(&path).unwrap());
                }
            }
        }
    }
    let root = dir.join(".git/logs");
    let mut out = BTreeMap::new();
    walk(&root, &root, &mut out);
    out
}

/// All refs; `refs/remotes/<name>/HEAD` is left out because git before 2.48 does not create it on a fetch.
fn refs(env: &Env, dir: &Path) -> String {
    text(&env.git(dir, &["for-each-ref"]).stdout)
        .lines()
        .filter(|l| !l.ends_with("/HEAD"))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// A git repository and an identical copy that `pyn` works in; a bare remote for each when `remote` is set.
struct Pair {
    _tmp: tempfile::TempDir,
    env: Env,
    git: PathBuf,
    pyn: PathBuf,
    step: i64,
    /// Compare reflog messages and ref names, not ids (the twins' remote URLs differ, so merge messages do).
    loose: bool,
}

impl Pair {
    fn build(format: &str, make: impl FnOnce(&Env, &Path)) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let env = Env {
            home: root.join("home"),
            ssh: gitsrv::ssh_shim(&root, false),
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
        make(&env, &git);
        let pyn = root.join("pyn");
        copy_tree(&git, &pyn);
        let _ = env.git(&git, &["update-index", "-q", "--refresh"]);
        Self {
            _tmp: tmp,
            env,
            git,
            pyn,
            step: 0,
            loose: false,
        }
    }

    /// Runs `args` in both with a clock that moves on each call; exit status, refs and every reflog must agree.
    fn same(&mut self, args: &[&str]) {
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
        let names = |r: String| -> String {
            r.lines()
                .map(|l| l.split_once(' ').map_or(l, |x| x.1).to_string() + "\n")
                .collect()
        };
        let (pr, gr) = (refs(&self.env, &self.pyn), refs(&self.env, &self.git));
        if self.loose {
            assert_eq!(names(pr), names(gr), "{what}: ref names");
        } else {
            assert_eq!(pr, gr, "{what}: refs");
        }
        let (gl, pl) = (logs(&self.git), logs(&self.pyn));
        assert_eq!(
            pl.keys().collect::<Vec<_>>(),
            gl.keys().collect::<Vec<_>>(),
            "{what}: which reflogs exist"
        );
        let strip = |text: &str| -> Vec<String> {
            text.lines()
                .map(|l| l.split_once('\t').map_or(l, |x| x.1).to_string())
                .collect()
        };
        for (name, g) in &gl {
            if self.loose {
                assert_eq!(
                    strip(&pl[name]),
                    strip(g),
                    "{what}: reflog {name} (pyn, then git)"
                );
            } else {
                assert_eq!(&pl[name], g, "{what}: reflog {name} (pyn, then git)");
            }
        }
    }
}

/// `main` with three commits, `side` at the first, `ahead` one past `main`, `diverge` one off the second.
fn history(env: &Env, dir: &Path) {
    commit(env, dir, "a", "one\n", "c1");
    env.ok(dir, &["branch", "side"]);
    commit(env, dir, "a", "two\n", "c2");
    env.ok(dir, &["branch", "diverge"]);
    commit(env, dir, "b", "b\n", "c3");
    env.ok(dir, &["switch", "-c", "ahead"]);
    commit(env, dir, "c", "c\n", "c4");
    env.ok(dir, &["switch", "diverge"]);
    commit(env, dir, "d", "d\n", "c5");
    env.ok(dir, &["switch", "main"]);
}

fn run(format: &str, make: impl FnOnce(&Env, &Path), steps: &[&[&str]]) {
    let mut pair = Pair::build(format, make);
    for step in steps {
        pair.same(step);
    }
}

#[test]
fn commits_and_amends() {
    for format in ["sha1", "sha256"] {
        run(
            format,
            history,
            &[
                &["commit", "--allow-empty", "-m", "x"],
                &["commit", "--amend", "--allow-empty", "-m", "y"],
                &["commit", "--allow-empty", "-m", "two\n\nlines"],
            ],
        );
    }
}

#[test]
fn the_first_commit_is_initial() {
    run(
        "sha1",
        |env, dir| {
            write(dir, "a", "x\n");
            env.ok(dir, &["add", "a"]);
        },
        &[
            &["commit", "-m", "first"],
            &["commit", "--allow-empty", "-m", "second"],
        ],
    );
}

#[test]
fn branches_and_switching() {
    for format in ["sha1", "sha256"] {
        run(
            format,
            history,
            &[
                &["branch", "b1"],
                &["branch", "b2", "HEAD~1"],
                &["branch", "-m", "b1", "b3"],
                &["checkout", "-b", "c"],
                &["switch", "main"],
                &["switch", "-"],
                &["switch", "--detach", "HEAD~1"],
                &["checkout", "main"],
                &["checkout", "-"],
                &["switch", "-c", "d", "side"],
                &["switch", "@{-1}"],
                &["checkout", "@{-2}"],
                &["checkout", "ahead"],
                &["branch", "-d", "b2"],
                &["branch", "-D", "b3"],
                &["branch", "-m", "d", "e"],
                &["branch", "-m", "e"],
            ],
        );
    }
}

#[test]
fn merging() {
    for format in ["sha1", "sha256"] {
        run(
            format,
            history,
            &[
                &["merge", "side"],
                &["merge", "diverge"],
                &["switch", "side"],
                &["merge", "--ff-only", "ahead"],
                &["merge", "--no-ff", "-m", "again", "main"],
                &["merge", "--abort"],
            ],
        );
    }
}

#[test]
fn a_conflicted_merge_continued() {
    let mut pair = Pair::build("sha1", |env, dir| {
        commit(env, dir, "a", "base\n", "c1");
        env.ok(dir, &["switch", "-c", "other"]);
        commit(env, dir, "a", "other\n", "c2");
        env.ok(dir, &["switch", "main"]);
        commit(env, dir, "a", "main\n", "c3");
    });
    pair.same(&["merge", "other"]);
    for dir in [pair.git.clone(), pair.pyn.clone()] {
        write(&dir, "a", "resolved\n");
    }
    pair.same(&["add", "a"]);
    pair.same(&["commit", "-m", "merged"]);
}

#[test]
fn resetting() {
    for format in ["sha1", "sha256"] {
        run(
            format,
            history,
            &[
                &["reset", "--hard", "HEAD~1"],
                &["reset", "--soft", "HEAD@{1}"],
                &["reset", "--mixed", "side"],
                &["reset", "--hard", "@{-1}"],
                &["switch", "--detach", "HEAD"],
                &["reset", "--hard", "HEAD~1"],
                &["reset", "--hard", "HEAD@{2}"],
            ],
        );
    }
}

#[test]
fn picking_and_reverting() {
    for format in ["sha1", "sha256"] {
        run(
            format,
            history,
            &[
                &["cherry-pick", "diverge"],
                &["revert", "--no-edit", "HEAD"],
                &["cherry-pick", "ahead"],
                &["revert", "--no-edit", "HEAD~1"],
                &["cherry-pick", "-n", "diverge"],
                &["reset", "--hard"],
                &["switch", "--detach", "HEAD"],
                &["cherry-pick", "diverge"],
            ],
        );
    }
}

#[test]
fn rebasing() {
    for format in ["sha1", "sha256"] {
        run(
            format,
            history,
            &[
                &["switch", "diverge"],
                &["rebase", "-i", "main"],
                &["rebase", "main"],
                &["rebase", "--onto", "side", "main"],
                &["switch", "ahead"],
                &["rebase", "--force-rebase", "main"],
            ],
        );
    }
}

#[test]
fn a_conflicted_rebase_continued_and_aborted() {
    let make = |env: &Env, dir: &Path| {
        commit(env, dir, "a", "base\n", "c1");
        env.ok(dir, &["switch", "-c", "other"]);
        commit(env, dir, "a", "other\n", "c2");
        env.ok(dir, &["switch", "main"]);
        commit(env, dir, "a", "main\n", "c3");
        env.ok(dir, &["switch", "other"]);
    };
    run("sha1", make, &[&["rebase", "main"], &["rebase", "--abort"]]);
    let mut pair = Pair::build("sha1", make);
    pair.same(&["rebase", "main"]);
    for dir in [pair.git.clone(), pair.pyn.clone()] {
        write(&dir, "a", "fixed\n");
        pair.env.ok(&dir, &["add", "a"]);
    }
    pair.same(&["rebase", "--continue"]);
}

#[test]
fn stashing_and_tagging() {
    for format in ["sha1", "sha256"] {
        if format == "sha256" {
            continue;
        }
        let mut pair = Pair::build(format, |env, dir| {
            history(env, dir);
            env.ok(dir, &["config", "core.logAllRefUpdates", "always"]);
        });
        for dir in [pair.git.clone(), pair.pyn.clone()] {
            write(&dir, "a", "dirty\n");
        }
        pair.same(&["stash"]);
        pair.same(&["stash", "pop"]);
        pair.same(&["tag", "t1"]);
        pair.same(&["tag", "-a", "-m", "m", "t2"]);
        pair.same(&["tag", "-d", "t1"]);
    }
}

fn ssh_url(bare: &Path) -> String {
    format!("ssh://git@example.invalid{}", bare.display())
}

/// A clone of a served bare repository that has moved on since, and has a local commit of its own.
fn with_remote(env: &Env, dir: &Path) {
    let root = dir.parent().unwrap();
    let bare = root.join("served.git");
    gitsrv::init(&bare, "sha1", true);
    let url = ssh_url(&bare);
    commit(env, dir, "a", "one\n", "c1");
    env.ok(dir, &["remote", "add", "origin", &url]);
    env.ok(dir, &["push", "-q", "-u", "origin", "main"]);
    let other = root.join("other");
    env.ok(root, &["clone", "-q", &url, "other"]);
    commit(env, &other, "o", "o\n", "remote1");
    env.ok(&other, &["push", "-q", "origin", "main"]);
    commit(env, dir, "l", "l\n", "local1");
}

#[test]
fn fetch_pull_and_push() {
    let mut pair = Pair::build("sha1", with_remote);
    let served = pair.git.parent().unwrap().join("served.git");
    let theirs = pair.git.parent().unwrap().join("served-pyn.git");
    copy_tree(&served, &theirs);
    pair.env.ok(
        &pair.pyn,
        &["config", "remote.origin.url", &ssh_url(&theirs)],
    );
    pair.loose = true;
    pair.same(&["fetch"]);
    pair.same(&["pull", "--no-rebase", "--no-edit"]);
    pair.same(&["reset", "--hard", "HEAD~2"]);
    pair.same(&["pull", "--ff-only"]);
    pair.same(&["commit", "--allow-empty", "-m", "ahead"]);
    pair.same(&["push", "origin", "main"]);
    pair.same(&["commit", "--allow-empty", "-m", "more"]);
    pair.same(&["pull", "--rebase"]);
    pair.same(&["push", "origin", "main"]);
    pair.same(&["switch", "-c", "topic"]);
    pair.same(&["push", "-u", "origin", "topic"]);
    pair.same(&["push", "origin", "--delete", "topic"]);
}

fn marks(env: &Env, dir: &Path) {
    history(env, dir);
    env.ok(dir, &["switch", "side"]);
    env.ok(dir, &["switch", "main"]);
    env.ok(dir, &["reset", "-q", "--hard", "HEAD~1"]);
    env.ok(dir, &["switch", "--detach", "ahead"]);
    env.ok(dir, &["switch", "main"]);
    let root = dir.parent().unwrap();
    let bare = root.join("up.git");
    let format = env.ok(dir, &["rev-parse", "--show-object-format"]);
    gitsrv::init(&bare, format.trim(), true);
    env.ok(dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
    env.ok(dir, &["push", "-q", "-u", "origin", "main", "side"]);
    env.ok(dir, &["branch", "--set-upstream-to=origin/side", "ahead"]);
    env.ok(dir, &["commit", "-q", "--allow-empty", "-m", "local"]);
    env.ok(dir, &["remote", "add", "fork", bare.to_str().unwrap()]);
    env.ok(dir, &["config", "branch.diverge.pushRemote", "fork"]);
    env.ok(dir, &["config", "branch.diverge.remote", "origin"]);
    env.ok(dir, &["config", "branch.diverge.merge", "refs/heads/main"]);
}

const MARKS: &[&str] = &[
    "HEAD@{0}",
    "HEAD@{1}",
    "HEAD@{2}",
    "HEAD@{3}",
    "HEAD@{9}",
    "HEAD@{20}",
    "@{0}",
    "@{1}",
    "@{2}",
    "@{-1}",
    "@{-2}",
    "@{-3}",
    "@{-9}",
    "@{-0}",
    "main@{0}",
    "main@{1}",
    "main@{3}",
    "main@{40}",
    "refs/heads/main@{1}",
    "side@{0}",
    "side@{1}",
    "side@{2}",
    "nosuch@{0}",
    "HEAD@{1}^",
    "HEAD@{1}~1",
    "@{-1}^",
    "@{-1}~1",
    "@{-2}^2",
    "main@{1}~",
    "@{u}",
    "@{U}",
    "@{upstream}",
    "@{UPSTREAM}",
    "main@{u}",
    "main@{upstream}",
    "ahead@{u}",
    "side@{u}",
    "diverge@{u}",
    "HEAD@{u}",
    "@{push}",
    "@{PUSH}",
    "main@{push}",
    "ahead@{push}",
    "diverge@{push}",
    "side@{push}",
    "origin/main@{0}",
    "origin/main@{1}",
    "origin/side@{0}",
    "HEAD@{}",
    "HEAD@{x}",
    "@{}",
    "@{-}",
    "@{-x}",
    "HEAD@{-1}",
    "main@{-1}",
    "@{u}^",
    "@{u}~1",
    "@{push}~1",
    "HEAD@",
    "@{",
    "HEAD@{1",
    "stash@{0}",
    "HEAD~1@{1}",
    "ORIG_HEAD@{0}",
    "@{1}@{1}",
    "@@{1}",
];

#[test]
fn reflog_revisions_name_what_git_names() {
    for format in ["sha1", "sha256"] {
        let pair = Pair::build(format, marks);
        for spec in MARKS {
            for cmd in [
                &["rev-parse", "--verify", "-q"][..],
                &["rev-parse", "--verify"],
            ] {
                let _ = cmd;
            }
            let g = pair
                .env
                .git(&pair.git, &["rev-parse", "--verify", "-q", spec]);
            let p = pair
                .env
                .pyn(&pair.pyn, &["show", "-s", "--format=%H", spec]);
            assert_eq!(
                p.status.success(),
                g.status.success(),
                "{format} {spec}: pyn said {}; git said {}",
                text(&p.stderr),
                text(&g.stderr)
            );
            if g.status.success() {
                assert_eq!(
                    text(&p.stdout).trim(),
                    text(&g.stdout).trim(),
                    "{format} {spec}"
                );
            }
        }
    }
}

#[test]
fn reflog_show_prints_git_lines() {
    for format in ["sha1", "sha256"] {
        let pair = Pair::build(format, marks);
        for args in [
            &["reflog"][..],
            &["reflog", "show"],
            &["reflog", "HEAD"],
            &["reflog", "show", "main"],
            &["reflog", "show", "refs/heads/main"],
            &["reflog", "show", "side"],
            &["reflog", "show", "origin/main"],
            &["reflog", "show", "ahead"],
            &["reflog", "-n", "2"],
            &["reflog", "show", "-n", "1", "main"],
            &["reflog", "show", "--max-count=3"],
            &["reflog", "show", "nosuch"],
            &["reflog", "show", "@"],
        ] {
            let g = pair.env.git(&pair.git, args);
            let p = pair.env.pyn(&pair.pyn, args);
            assert_eq!(
                p.status.code(),
                g.status.code(),
                "{args:?}: {}",
                text(&p.stderr)
            );
            assert_eq!(text(&p.stdout), text(&g.stdout), "{format} {args:?}");
        }
        let g = pair
            .env
            .git(&pair.git, &["reflog", "exists", "refs/heads/main"]);
        let p = pair
            .env
            .pyn(&pair.pyn, &["reflog", "exists", "refs/heads/main"]);
        assert_eq!(p.status.code(), g.status.code());
        let g = pair.env.git(&pair.git, &["reflog", "exists", "main"]);
        let p = pair.env.pyn(&pair.pyn, &["reflog", "exists", "main"]);
        assert_eq!(p.status.code(), g.status.code());
        assert_eq!(p.status.code(), Some(1));
    }
}

#[test]
fn expiring_and_deleting() {
    let cases: &[&[&str]] = &[
        &["reflog", "expire", "--all"],
        &["reflog", "expire", "--expire=1700000300", "--all"],
        &["reflog", "expire", "--expire=1700000300", "HEAD"],
        &[
            "reflog",
            "expire",
            "--expire=1700000300",
            "--verbose",
            "HEAD",
        ],
        &[
            "reflog",
            "expire",
            "--expire=1700000300",
            "--rewrite",
            "--all",
        ],
        &[
            "reflog",
            "expire",
            "--expire=1700000300",
            "--rewrite",
            "--updateref",
            "HEAD",
        ],
        &["reflog", "expire", "--expire=all", "--all"],
        &["reflog", "expire", "--expire=all", "main"],
        &["reflog", "expire", "--expire=never", "--all"],
        &[
            "reflog",
            "expire",
            "--expire=now",
            "--dry-run",
            "--verbose",
            "--all",
        ],
        &["reflog", "expire", "--expire=all", "--dry-run", "--all"],
        &[
            "reflog",
            "expire",
            "--expire-unreachable=now",
            "--expire=never",
            "--all",
        ],
        &[
            "reflog",
            "expire",
            "--expire-unreachable=now",
            "--expire=never",
            "--verbose",
            "HEAD",
        ],
        &[
            "reflog",
            "expire",
            "--expire-unreachable=now",
            "--expire=never",
            "refs/heads/main",
        ],
        &[
            "reflog",
            "expire",
            "--expire-unreachable=1700000300",
            "--expire=1700000200",
            "--all",
        ],
        &[
            "reflog",
            "expire",
            "--expire=1700000300",
            "--expire-unreachable=1700000400",
            "--all",
        ],
        &["reflog", "expire", "--expire=1.day.ago", "--all"],
        &["reflog", "expire", "--expire=2000-01-01", "--all"],
        &["reflog", "delete", "HEAD@{0}"],
        &["reflog", "delete", "HEAD@{1}", "HEAD@{3}"],
        &["reflog", "delete", "--rewrite", "HEAD@{1}"],
        &["reflog", "delete", "--rewrite", "--updateref", "HEAD@{0}"],
        &["reflog", "delete", "--updateref", "main@{0}"],
        &["reflog", "delete", "--dry-run", "--verbose", "HEAD@{1}"],
        &["reflog", "delete", "--verbose", "main@{1}", "side@{0}"],
        &["reflog", "delete", "HEAD@{99}"],
        &["reflog", "delete", "HEAD"],
        &["reflog", "delete", "nosuch@{0}"],
        &["reflog", "expire", "nosuch"],
    ];
    for args in cases {
        let pair = Pair::build("sha1", marks);
        let g = pair.env.git(&pair.git, args);
        let p = pair.env.pyn(&pair.pyn, args);
        assert_eq!(
            p.status.success(),
            g.status.success(),
            "{args:?}: pyn said {}; git said {}",
            text(&p.stderr),
            text(&g.stderr)
        );
        assert_eq!(
            refs(&pair.env, &pair.pyn),
            refs(&pair.env, &pair.git),
            "{args:?}: refs"
        );
        let (gl, pl) = (logs(&pair.git), logs(&pair.pyn));
        assert_eq!(
            pl.keys().collect::<Vec<_>>(),
            gl.keys().collect::<Vec<_>>(),
            "{args:?}: which reflogs exist"
        );
        for (name, g) in &gl {
            assert_eq!(&pl[name], g, "{args:?}: reflog {name} (pyn, then git)");
        }
        let lines = |o: &Output| {
            let mut v: Vec<String> = text(&o.stdout)
                .lines()
                .chain(text(&o.stderr).lines())
                .filter(|l| !l.starts_with("fatal") && !l.starts_with("error"))
                .map(str::to_owned)
                .collect();
            v.sort();
            v
        };
        assert_eq!(lines(&p), lines(&g), "{args:?}: what it printed");
    }
}

#[test]
fn reflog_list_names_the_logged_refs() {
    let pair = Pair::build("sha1", marks);
    let g = pair.env.git(&pair.git, &["reflog", "list"]);
    if !g.status.success() {
        return;
    }
    let p = pair.env.pyn(&pair.pyn, &["reflog", "list"]);
    let sorted = |o: &Output| {
        let mut v: Vec<String> = text(&o.stdout).lines().map(str::to_owned).collect();
        v.sort();
        v
    };
    assert_eq!(sorted(&p), sorted(&g));
}

#[test]
fn a_reflog_follows_the_path_to_an_unborn_branch() {
    let pair = Pair::build("sha1", |_, _| {});
    let p = pair.env.pyn(&pair.pyn, &["reflog"]);
    let g = pair.env.git(&pair.git, &["reflog"]);
    assert_eq!(p.status.code(), g.status.code());
    assert_eq!(text(&p.stdout), text(&g.stdout));
}

/// `main` and `other` both change `a`, so picking one onto the other conflicts.
fn conflicting(env: &Env, dir: &Path) {
    commit(env, dir, "a", "base\n", "c1");
    env.ok(dir, &["switch", "-c", "other"]);
    commit(env, dir, "a", "other\n", "c2");
    commit(env, dir, "b", "b\n", "c3");
    env.ok(dir, &["switch", "main"]);
    commit(env, dir, "a", "main\n", "c4");
}

fn resolve(pair: &Pair) {
    for dir in [&pair.git, &pair.pyn] {
        write(dir, "a", "resolved\n");
        pair.env.ok(dir, &["add", "a"]);
    }
}

#[test]
fn conflicted_picks_continued_skipped_and_aborted() {
    let mut pair = Pair::build("sha1", conflicting);
    pair.same(&["cherry-pick", "other~1", "other"]);
    resolve(&pair);
    pair.same(&["cherry-pick", "--continue"]);
    pair.same(&["reset", "--hard", "main"]);
    pair.same(&["cherry-pick", "other~1", "other"]);
    pair.same(&["cherry-pick", "--skip"]);
    pair.same(&["reset", "--hard", "main"]);
    pair.same(&["cherry-pick", "other~1"]);
    pair.same(&["cherry-pick", "--abort"]);
    pair.same(&["cherry-pick", "other~1"]);
    pair.same(&["cherry-pick", "--quit"]);
    pair.same(&["reset", "--hard", "main"]);
    pair.same(&["revert", "--no-edit", "HEAD~1"]);
    pair.same(&["revert", "--abort"]);
    pair.same(&["merge", "other"]);
    pair.same(&["merge", "--abort"]);
}

#[test]
fn stash_variants() {
    let mut pair = Pair::build("sha1", history);
    for dir in [pair.git.clone(), pair.pyn.clone()] {
        write(&dir, "a", "dirty\n");
        write(&dir, "u", "untracked\n");
    }
    pair.same(&["stash", "-u"]);
    pair.same(&["stash", "apply"]);
    pair.same(&["reset", "--hard"]);
    pair.same(&["stash", "drop"]);
}

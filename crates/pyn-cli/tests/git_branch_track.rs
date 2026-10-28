//! Upstream setup for a created branch (`branch`, `checkout -b`, `switch -c`, `--track`, `--no-track`, `branch.autoSetupMerge`,
//! `branch.autoSetupRebase`), compared with real git on twin repositories by resulting configuration and exit status class.
//! Needs `git` on the PATH (tests only).

#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

fn git(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git is installed")
}

fn pyn(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pyn"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TZ", "UTC")
        .env("PYN_CONFIG_DIR", home.join("pyn-config"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PYN_SERVER")
        .env_remove("PYN_REPO")
        .output()
        .unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = git(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Twin work trees that each have the remotes `origin` and `other`, both holding `main` and `topic`.
struct World {
    _tmp: TempDir,
    work: [PathBuf; 2],
    home: PathBuf,
}

impl World {
    fn new(topic_on: &[&str]) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let seed = root.join("seed");
        fs::create_dir_all(seed.join("d")).unwrap();
        ok(&seed, &home, &["init", "-q", "-b", "main"]);
        fs::write(seed.join("d/f.txt"), "one\n").unwrap();
        ok(&seed, &home, &["add", "."]);
        ok(&seed, &home, &["commit", "-q", "-m", "one"]);
        ok(&seed, &home, &["switch", "-q", "-c", "topic"]);
        fs::write(seed.join("t.txt"), "topic\n").unwrap();
        ok(&seed, &home, &["add", "."]);
        ok(&seed, &home, &["commit", "-q", "-m", "topic"]);
        ok(&seed, &home, &["switch", "-q", "main"]);
        let work = ["git", "pyn"].map(|n| root.join(n));
        for (i, w) in work.iter().enumerate() {
            fs::create_dir_all(w).unwrap();
            ok(w, &home, &["init", "-q", "-b", "main"]);
            for name in ["origin", "other"] {
                let bare = root.join(format!("{name}-{i}.git"));
                ok(
                    &root,
                    &home,
                    &["init", "-q", "--bare", bare.to_str().unwrap()],
                );
                ok(
                    &seed,
                    &home,
                    &["push", "-q", bare.to_str().unwrap(), "main"],
                );
                if topic_on.contains(&name) {
                    ok(
                        &seed,
                        &home,
                        &["push", "-q", bare.to_str().unwrap(), "topic"],
                    );
                }
                ok(w, &home, &["remote", "add", name, bare.to_str().unwrap()]);
            }
            ok(w, &home, &["fetch", "-q", "--all"]);
            ok(w, &home, &["reset", "-q", "--hard", "origin/main"]);
            ok(w, &home, &["branch", "feat"]);
            ok(w, &home, &["tag", "v1"]);
        }
        Self {
            _tmp: tmp,
            work,
            home,
        }
    }

    fn setup(&self, args: &[&str]) {
        for w in &self.work {
            ok(w, &self.home, args);
        }
    }

    /// Runs `args` with git and with pyn and compares the exit status class and the resulting refs and `branch.*` settings.
    fn same(&self, args: &[&str]) -> (Output, Output) {
        let g = git(&self.work[0], &self.home, args);
        let p = pyn(&self.work[1], &self.home, args);
        let ctx = format!(
            "{args:?}\ngit: {}\npyn: {}",
            String::from_utf8_lossy(&g.stderr),
            String::from_utf8_lossy(&p.stderr)
        );
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{ctx}"
        );
        assert_eq!(self.state(0), self.state(1), "{ctx}");
        (g, p)
    }

    /// Like `same` for a refusal: only the status class, as git may leave the branch it created behind.
    fn refused(&self, args: &[&str]) {
        let g = git(&self.work[0], &self.home, args);
        let p = pyn(&self.work[1], &self.home, args);
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            1,
            "{args:?}"
        );
        assert_eq!(
            oracle::status_class(p.status.code().unwrap()),
            1,
            "{args:?}"
        );
    }

    fn state(&self, i: usize) -> String {
        let (w, h) = (&self.work[i], &self.home);
        let mut config: Vec<String> = git_out(w, h, &["config", "--get-regexp", "^branch\\."])
            .lines()
            .map(str::to_owned)
            .collect();
        config.sort();
        [
            git_out(w, h, &["symbolic-ref", "-q", "HEAD"]),
            git_out(
                w,
                h,
                &[
                    "for-each-ref",
                    "refs/heads",
                    "--format=%(refname) %(objectname)",
                ],
            ),
            config.join("\n"),
            git_out(w, h, &["status", "--porcelain"]),
        ]
        .join("|")
    }
}

fn git_out(dir: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(dir, home, args).stdout).into_owned()
}

fn succeeds(w: &World, args: &[&str]) {
    let (_, p) = w.same(args);
    assert_eq!(
        p.status.code(),
        Some(0),
        "{args:?}: {}",
        String::from_utf8_lossy(&p.stderr)
    );
}

fn config_of(w: &World, branch: &str, key: &str) -> String {
    let name = format!("branch.{branch}.{key}");
    git_out(&w.work[1], &w.home, &["config", &name])
        .trim()
        .to_owned()
}

fn world() -> World {
    World::new(&["origin", "other"])
}

#[test]
fn creating_from_a_remote_branch_tracks_it_and_a_local_start_does_not() {
    for args in [
        &["branch", "n", "origin/topic"][..],
        &["branch", "n", "other/topic"][..],
        &["branch", "n", "refs/remotes/origin/topic"][..],
        &["branch", "n", "remotes/origin/main"][..],
        &["branch", "n", "feat"][..],
        &["branch", "n", "main"][..],
        &["branch", "n", "HEAD"][..],
        &["branch", "n", "v1"][..],
        &["branch", "n"][..],
        &["checkout", "-b", "n", "origin/topic"][..],
        &["checkout", "-b", "n"][..],
        &["checkout", "-b", "n", "feat"][..],
        &["switch", "-c", "n", "origin/topic"][..],
        &["switch", "-c", "n", "other/topic"][..],
        &["switch", "-c", "n", "feat"][..],
        &["checkout", "-B", "feat", "origin/topic"][..],
        &["switch", "-C", "feat", "origin/topic"][..],
        &["branch", "-f", "feat", "origin/topic"][..],
    ] {
        succeeds(&world(), args);
    }
    let w = world();
    succeeds(&w, &["branch", "n", "other/topic"]);
    assert_eq!(config_of(&w, "n", "remote"), "other");
    assert_eq!(config_of(&w, "n", "merge"), "refs/heads/topic");
}

#[test]
fn track_flags_override_the_setting() {
    for args in [
        &["branch", "-t", "n", "origin/topic"][..],
        &["branch", "--track", "n", "origin/topic"][..],
        &["branch", "--track=direct", "n", "origin/topic"][..],
        &["branch", "-t", "n", "feat"][..],
        &["branch", "-t", "n", "main"][..],
        &["branch", "-t", "n"][..],
        &["branch", "--no-track", "n", "origin/topic"][..],
        &["branch", "--no-track", "-t", "n", "origin/topic"][..],
        &["branch", "-t", "--no-track", "n", "origin/topic"][..],
        &[
            "branch",
            "--track=direct",
            "--no-track",
            "n",
            "origin/topic",
        ][..],
        &["branch", "-t", "--track=inherit", "n", "origin/topic"][..],
        &["branch", "--track=inherit", "n", "v1"][..],
        &["checkout", "-t", "-b", "n", "feat"][..],
        &["checkout", "--no-track", "-b", "n", "origin/topic"][..],
        &["switch", "-t", "-c", "n", "feat"][..],
        &["switch", "--no-track", "-c", "n", "origin/topic"][..],
        &["checkout", "-t", "-B", "feat", "main"][..],
        &["switch", "-t", "-C", "feat", "main"][..],
    ] {
        succeeds(&world(), args);
    }
    for pre in ["false", "inherit", "simple"] {
        let w = world();
        w.setup(&["config", "branch.autoSetupMerge", pre]);
        succeeds(&w, &["branch", "-t", "n", "origin/topic"]);
        assert_eq!(config_of(&w, "n", "remote"), "origin");
        let w = world();
        w.setup(&["config", "branch.autoSetupMerge", pre]);
        succeeds(&w, &["branch", "-t", "n", "feat"]);
        assert_eq!(config_of(&w, "n", "remote"), ".");
    }
    let w = world();
    w.setup(&["config", "branch.autoSetupMerge", "always"]);
    succeeds(&w, &["branch", "--no-track", "n", "origin/topic"]);
    assert_eq!(config_of(&w, "n", "remote"), "");
}

#[test]
fn track_on_checkout_and_switch_names_the_branch_from_the_remote_one() {
    for args in [
        &["checkout", "-t", "origin/topic"][..],
        &["checkout", "--track", "other/topic"][..],
        &["checkout", "-t", "refs/remotes/origin/topic"][..],
        &["checkout", "-t", "remotes/origin/topic"][..],
        &["checkout", "--no-track", "origin/topic"][..],
        &["checkout", "--track=inherit", "origin/topic"][..],
        &["switch", "-t", "origin/topic"][..],
        &["switch", "--track", "other/topic"][..],
        &["switch", "--no-track", "origin/topic"][..],
    ] {
        succeeds(&world(), args);
    }
    for args in [
        &["checkout", "-t", "topic"][..],
        &["checkout", "-t", "feat"][..],
        &["checkout", "-t"][..],
        &["checkout", "--no-track", "feat"][..],
        &["checkout", "-t", "origin/topic", "--detach"][..],
        &["checkout", "-t", "origin/nothing"][..],
        &["switch", "-t"][..],
        &["switch", "-t", "main"][..],
        &["switch", "-t", "--detach", "origin/topic"][..],
    ] {
        world().refused(args);
    }
}

#[test]
fn a_start_that_is_not_a_branch_is_refused_under_track() {
    let w = world();
    w.setup(&["update-ref", "refs/remotes/zzz/q", "HEAD"]);
    for args in [
        &["branch", "-t", "n", "v1"][..],
        &["branch", "-t", "n", "zzz/q"][..],
        &["branch", "-t", "n", "feat~0"][..],
        &["branch", "-t", "feat", "origin/topic"][..],
        &["checkout", "-t", "-b", "n", "v1"][..],
        &["switch", "-t", "-c", "n", "v1"][..],
    ] {
        w.refused(args);
    }
    let w = world();
    w.setup(&["switch", "-q", "--detach"]);
    w.refused(&["branch", "-t", "n", "HEAD"]);
    w.refused(&["branch", "-t", "n"]);
    succeeds(&w, &["branch", "n"]);
    let w = world();
    w.setup(&["switch", "-q", "--detach"]);
    w.setup(&["config", "branch.autoSetupMerge", "always"]);
    succeeds(&w, &["checkout", "-b", "n"]);
}

#[test]
fn auto_setup_merge_values_decide_for_each_kind_of_start() {
    let starts = ["origin/topic", "other/topic", "feat", "main", "HEAD", "v1"];
    for value in ["true", "false", "always", "inherit", "simple", "yes", "0"] {
        for start in starts {
            for args in [
                vec!["branch", "topic", start],
                vec!["branch", "n", start],
                vec!["checkout", "-b", "topic", start],
                vec!["switch", "-c", "n", start],
            ] {
                let w = world();
                w.setup(&["config", "branch.autoSetupMerge", value]);
                w.same(&args);
            }
        }
    }
    for value in ["", "always", "inherit"] {
        let w = world();
        if !value.is_empty() {
            w.setup(&["config", "branch.autoSetupMerge", value]);
        }
        succeeds(&w, &["branch", "n"]);
    }
}

#[test]
fn simple_tracks_only_a_remote_branch_of_the_same_name() {
    let w = world();
    w.setup(&["config", "branch.autoSetupMerge", "simple"]);
    succeeds(&w, &["branch", "topic", "origin/topic"]);
    assert_eq!(config_of(&w, "topic", "remote"), "origin");
    let w = world();
    w.setup(&["config", "branch.autoSetupMerge", "simple"]);
    succeeds(&w, &["branch", "n", "origin/topic"]);
    assert_eq!(config_of(&w, "n", "remote"), "");
    let w = world();
    w.setup(&["config", "branch.autoSetupMerge", "simple"]);
    w.setup(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/foo:refs/remotes/origin/topic",
    ]);
    succeeds(&w, &["branch", "topic", "origin/topic"]);
    succeeds(&w, &["branch", "foo", "origin/topic"]);
    assert_eq!(config_of(&w, "foo", "merge"), "refs/heads/foo");
}

#[test]
fn inherit_copies_the_start_branchs_upstream() {
    for value in ["inherit", "true"] {
        for args in [
            &["branch", "n", "feat"][..],
            &["branch", "--track=inherit", "n", "feat"][..],
            &["checkout", "-b", "n", "feat"][..],
            &["switch", "-c", "n", "feat"][..],
            &["switch", "-C", "n", "feat"][..],
            &["branch", "--track=inherit", "n", "origin/topic"][..],
            &["branch", "--track=inherit", "n", "main"][..],
            &["branch", "--no-track", "n", "feat"][..],
        ] {
            let w = world();
            w.setup(&["config", "branch.autoSetupMerge", value]);
            w.setup(&["branch", "-u", "origin/topic", "feat"]);
            succeeds(&w, args);
        }
    }
    let w = world();
    w.setup(&["branch", "-u", "main", "feat"]);
    succeeds(&w, &["branch", "--track=inherit", "n", "feat"]);
    assert_eq!(config_of(&w, "n", "remote"), ".");
    assert_eq!(config_of(&w, "n", "merge"), "refs/heads/main");
    let w = world();
    w.setup(&["branch", "-u", "origin/topic", "feat"]);
    w.setup(&["config", "--add", "branch.feat.merge", "refs/heads/main"]);
    succeeds(&w, &["branch", "--track=inherit", "n", "feat"]);
}

#[test]
fn auto_setup_rebase_writes_the_rebase_setting() {
    for value in ["never", "local", "remote", "always"] {
        for merge in ["true", "always", "inherit"] {
            for args in [
                &["branch", "n", "origin/topic"][..],
                &["branch", "n", "feat"][..],
                &["branch", "--track=inherit", "n", "feat"][..],
                &["checkout", "-b", "n", "origin/topic"][..],
                &["branch", "--no-track", "n", "origin/topic"][..],
            ] {
                let w = world();
                w.setup(&["config", "branch.autoSetupRebase", value]);
                w.setup(&["config", "branch.autoSetupMerge", merge]);
                w.setup(&["branch", "-u", "origin/topic", "feat"]);
                w.same(args);
            }
        }
    }
    let w = world();
    w.setup(&["config", "branch.autoSetupRebase", "remote"]);
    succeeds(&w, &["branch", "n", "origin/topic"]);
    assert_eq!(config_of(&w, "n", "rebase"), "true");
}

#[test]
fn an_existing_branch_section_is_updated_in_place() {
    let w = world();
    w.setup(&["branch", "n", "main"]);
    w.setup(&["config", "branch.n.rebase", "false"]);
    w.setup(&["config", "branch.n.merge", "refs/heads/old"]);
    succeeds(&w, &["branch", "-f", "n", "origin/topic"]);
    assert_eq!(config_of(&w, "n", "merge"), "refs/heads/topic");
    assert_eq!(config_of(&w, "n", "rebase"), "false");
    let w = world();
    w.setup(&["config", "branch.autoSetupRebase", "always"]);
    w.setup(&["config", "branch.autoSetupMerge", "always"]);
    succeeds(&w, &["branch", "-f", "feat", "feat"]);
    succeeds(&w, &["checkout", "-B", "feat"]);
}

#[test]
fn a_malformed_setting_is_refused_before_anything_is_created() {
    for (key, value) in [
        ("branch.autoSetupMerge", "bogus"),
        ("branch.autoSetupMerge", "ALWAYS"),
        ("branch.autoSetupRebase", "bogus"),
    ] {
        for args in [
            &["branch", "n", "origin/topic"][..],
            &["branch", "--no-track", "n", "origin/topic"][..],
            &["checkout", "-b", "n", "origin/topic"][..],
            &["switch", "-c", "n", "origin/topic"][..],
        ] {
            let w = world();
            w.setup(&["config", key, value]);
            let p = pyn(&w.work[1], &w.home, args);
            assert_eq!(
                oracle::status_class(p.status.code().unwrap()),
                1,
                "{args:?}"
            );
            let refs = git_out(
                &w.work[1],
                &w.home,
                &[
                    "-c",
                    "branch.autoSetupMerge=true",
                    "-c",
                    "branch.autoSetupRebase=never",
                    "for-each-ref",
                    "refs/heads/n",
                ],
            );
            assert!(refs.is_empty(), "{args:?} created {refs}");
        }
    }
}

#[test]
fn two_remotes_mapping_one_ref_are_ambiguous() {
    for pre in [None, Some("true"), Some("always"), Some("simple")] {
        for args in [
            &["branch", "topic", "origin/topic"][..],
            &["branch", "-t", "n", "origin/topic"][..],
            &["checkout", "-b", "n", "origin/topic"][..],
            &["switch", "-c", "n", "origin/topic"][..],
        ] {
            let w = world();
            w.setup(&[
                "config",
                "--add",
                "remote.other.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ]);
            if let Some(v) = pre {
                w.setup(&["config", "branch.autoSetupMerge", v]);
            }
            w.refused(args);
        }
    }
    let w = world();
    w.setup(&[
        "config",
        "--add",
        "remote.other.fetch",
        "+refs/heads/*:refs/remotes/origin/*",
    ]);
    w.setup(&["config", "branch.autoSetupMerge", "false"]);
    succeeds(&w, &["branch", "n", "origin/topic"]);
    w.refused(&["branch", "-t", "m", "origin/topic"]);
}

#[test]
fn a_guessed_branch_follows_the_setting() {
    for value in ["false", "true", "always", "inherit", "simple"] {
        for args in [&["checkout", "topic"][..], &["switch", "topic"][..]] {
            let w = World::new(&["origin"]);
            w.setup(&["config", "branch.autoSetupMerge", value]);
            succeeds(&w, args);
        }
    }
    for args in [&["checkout", "topic"][..], &["switch", "topic"][..]] {
        let w = World::new(&["origin"]);
        w.setup(&["config", "branch.autoSetupRebase", "remote"]);
        succeeds(&w, args);
        assert_eq!(config_of(&w, "topic", "rebase"), "true");
    }
    let w = World::new(&["origin"]);
    w.setup(&["config", "branch.autoSetupMerge", "bogus"]);
    w.refused(&["checkout", "topic"]);
}

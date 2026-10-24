//! Local-path and `file://` remotes: pyn serves upload-pack and receive-pack in process. These tests run
//! the same command with real git and with pyn against twin remotes and compare the remote's refs, the exit status
//! and the refusals by meaning. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "../../pyn-gittransport/tests/common/script.rs"]
mod script;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Out {
    code: i32,
    stderr: String,
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// Index 0 is the side real git works on, index 1 the side pyn works on.
struct Twin {
    _tmp: TempDir,
    root: PathBuf,
    remote: [PathBuf; 2],
    side: [PathBuf; 2],
    home: PathBuf,
}

impl Twin {
    /// Twin remotes holding `sample_work`; `bare` says whether they are bare or checked out on `main`.
    fn new(format: &str, bare: bool) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let seed = root.join("seed");
        sample_work(&seed, format);
        let names = ["git", "pyn"];
        let remote = names.map(|n| root.join(format!("remote-{n}")));
        let side = names.map(|n| root.join(format!("{n}-side")));
        for i in 0..2 {
            let args: &[&str] = if bare {
                &["clone", "-q", "--bare"]
            } else {
                &["clone", "-q"]
            };
            let mut full = args.to_vec();
            full.extend([seed.to_str().unwrap(), remote[i].to_str().unwrap()]);
            run(&root, &full);
            run(
                &root,
                &[
                    "clone",
                    "-q",
                    remote[i].to_str().unwrap(),
                    side[i].to_str().unwrap(),
                ],
            );
        }
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        Self {
            _tmp: tmp,
            root,
            remote,
            side,
            home,
        }
    }

    fn git_cmd(&self, dir: &Path, args: &[&str]) -> Out {
        out_of(
            git(dir)
                .args(args)
                .env("HOME", &self.home)
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap(),
        )
    }

    fn pyn_cmd(&self, dir: &Path, args: &[&str]) -> Out {
        out_of(
            Command::new(env!("CARGO_BIN_EXE_pyn"))
                .current_dir(dir)
                .args(args)
                .env_clear()
                .env("PATH", path_env())
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .output()
                .unwrap(),
        )
    }

    /// Sets a configuration value on both remotes.
    fn configure(&self, key: &str, value: &str) {
        for r in &self.remote {
            run(r, &["config", key, value]);
        }
    }

    /// Commits the same change on both sides.
    fn commit_both(&self, file: &str) {
        for s in &self.side {
            commit(s, file, file.as_bytes(), file);
        }
    }

    /// `args` with git and with pyn: the same status, the same refs on the remote, the same refusals.
    fn cmp(&self, args: &[&str]) -> (Out, Out) {
        let g = self.git_cmd(&self.side[0], args);
        let p = self.pyn_cmd(&self.side[1], args);
        let ctx = format!("{args:?}\ngit: {}\npyn: {}", g.stderr, p.stderr);
        assert_eq!(g.code, p.code, "{ctx}");
        assert_eq!(refs(&self.remote[0]), refs(&self.remote[1]), "{ctx}");
        assert_eq!(rows(&g.stderr), rows(&p.stderr), "{ctx}");
        assert_eq!(remote_errors(&g.stderr), remote_errors(&p.stderr), "{ctx}");
        for r in &self.remote {
            fsck(r);
        }
        (g, p)
    }
}

fn refs(dir: &Path) -> String {
    run(dir, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

/// The ref result rows of a push (` * [new branch] a -> a`) with their spacing normalised.
fn rows(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| {
            l.starts_with(' ') && l.contains("] ") || l.starts_with("   ") && l.contains("..")
        })
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// The server's refusals, without the wording of the long hints that git varies between versions.
fn remote_errors(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|l| l.starts_with("remote: error: refusing") || l.starts_with("remote: warning:"))
        .map(|l| l.trim_end().to_owned())
        .collect()
}

#[test]
fn push_to_a_bare_repository_creates_updates_and_deletes() {
    for format in FORMATS {
        let t = Twin::new(format, true);
        let (_, p) = t.cmp(&["push", "origin", "main:fresh"]);
        assert_eq!(p.code, 0, "{}", p.stderr);
        assert!(p.stderr.contains("* [new branch]"), "{}", p.stderr);
        t.commit_both("more.txt");
        let (_, p) = t.cmp(&["push", "origin", "main"]);
        assert!(p.stderr.contains(".."), "{}", p.stderr);
        t.cmp(&["push", "origin", "main"]);
        t.cmp(&["push", "origin", "--delete", "fresh"]);
        t.cmp(&["push", "origin", "feature", "v1", "v2"]);
        for s in &t.side {
            run(s, &["tag", "-a", "v3", "-m", "third"]);
        }
        t.cmp(&["push", "origin", "--tags"]);
    }
}

#[test]
fn a_rejected_non_fast_forward_and_a_forced_update_match_git() {
    for format in FORMATS {
        let t = Twin::new(format, true);
        for s in &t.side {
            run(s, &["reset", "-q", "--hard", "HEAD~1"]);
            commit(s, "other.txt", b"x\n", "other");
        }
        let (_, p) = t.cmp(&["push", "origin", "main"]);
        assert_eq!(p.code, 1, "{}", p.stderr);
        assert!(p.stderr.contains("[rejected]"), "{}", p.stderr);
        assert!(
            p.stderr.contains("failed to push some refs"),
            "{}",
            p.stderr
        );
        let (_, p) = t.cmp(&["push", "--force", "origin", "main"]);
        assert_eq!(p.code, 0, "{}", p.stderr);
        assert!(p.stderr.contains("+ "), "{}", p.stderr);
    }
}

#[test]
fn the_checked_out_branch_of_a_work_tree_repository_is_refused() {
    for format in FORMATS {
        let t = Twin::new(format, false);
        t.commit_both("more.txt");
        let (_, p) = t.cmp(&["push", "origin", "main"]);
        assert_eq!(p.code, 1, "{}", p.stderr);
        assert!(
            p.stderr
                .contains("remote: error: refusing to update checked out branch: refs/heads/main"),
            "{}",
            p.stderr
        );
        assert!(
            p.stderr
                .contains("! [remote rejected] main -> main (branch is currently checked out)"),
            "{}",
            p.stderr
        );
        let (_, p) = t.cmp(&["push", "origin", "--delete", "main"]);
        assert_eq!(p.code, 1, "{}", p.stderr);
        let (_, p) = t.cmp(&["push", "origin", "main:elsewhere"]);
        assert_eq!(p.code, 0, "{}", p.stderr);
        t.cmp(&["push", "origin", "--delete", "elsewhere"]);
    }
}

#[test]
fn deny_current_branch_settings_ignore_warn_and_refuse() {
    for value in ["ignore", "warn", "refuse", "false"] {
        let t = Twin::new("sha1", false);
        t.configure("receive.denyCurrentBranch", value);
        t.commit_both("more.txt");
        let (_, p) = t.cmp(&["push", "origin", "main"]);
        let allowed = matches!(value, "ignore" | "warn" | "false");
        assert_eq!(p.code, i32::from(!allowed), "{value}: {}", p.stderr);
    }
}

#[test]
fn deleting_the_current_branch_of_a_bare_repository_and_the_deny_settings() {
    let t = Twin::new("sha1", true);
    let (_, p) = t.cmp(&["push", "origin", "--delete", "main"]);
    assert_eq!(p.code, 1, "{}", p.stderr);
    assert!(
        p.stderr
            .contains("(deletion of the current branch prohibited)"),
        "{}",
        p.stderr
    );
    t.configure("receive.denyDeleteCurrent", "ignore");
    t.cmp(&["push", "origin", "--delete", "main"]);

    let t = Twin::new("sha1", true);
    t.configure("receive.denyDeletes", "true");
    let (_, p) = t.cmp(&["push", "origin", "--delete", "feature"]);
    assert!(p.stderr.contains("(deletion prohibited)"), "{}", p.stderr);

    let t = Twin::new("sha1", true);
    t.configure("receive.denyNonFastForwards", "true");
    for s in &t.side {
        run(s, &["reset", "-q", "--hard", "HEAD~1"]);
        commit(s, "other.txt", b"x\n", "other");
    }
    let (_, p) = t.cmp(&["push", "--force", "origin", "main"]);
    assert!(p.stderr.contains("(non-fast-forward)"), "{}", p.stderr);
}

#[test]
fn an_atomic_push_with_one_refusal_changes_nothing() {
    let t = Twin::new("sha1", false);
    t.commit_both("more.txt");
    let (_, p) = t.cmp(&["push", "--atomic", "origin", "main", "main:other"]);
    assert_eq!(p.code, 1, "{}", p.stderr);
    assert!(!refs(&t.remote[1]).contains("refs/heads/other"));
}

#[test]
fn fetch_and_clone_bring_new_commits_and_tags_and_the_log_matches() {
    for format in FORMATS {
        let t = Twin::new(format, true);
        for s in &t.side {
            run(s, &["tag", "-a", "v9", "-m", "nine"]);
            commit(s, "n.txt", b"n\n", "n");
            run(s, &["push", "-q", "origin", "main", "v9"]);
        }
        for i in 0..2 {
            commit(&t.side[i], "extra.txt", b"e\n", "extra");
            run(&t.side[i], &["push", "-q", "origin", "main"]);
        }
        let (git_dir, pyn_dir) = (t.root.join("c-git"), t.root.join("c-pyn"));
        let url = t.remote[1].display().to_string();
        assert_eq!(
            t.git_cmd(
                &t.root,
                &["clone", "-q", &t.remote[0].display().to_string(), "c-git"]
            )
            .code,
            0
        );
        let c = t.pyn_cmd(&t.root, &["clone", "-q", &format!("file://{url}"), "c-pyn"]);
        assert_eq!(c.code, 0, "{}", c.stderr);
        for args in [
            &["log", "--all", "--format=%H %s %D"][..],
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname) %(objecttype)",
            ][..],
            &["rev-list", "--objects", "--all"][..],
        ] {
            assert_eq!(run(&git_dir, args), run(&pyn_dir, args), "{args:?}");
        }
        fsck(&pyn_dir);
        assert_eq!(
            run(&pyn_dir, &["config", "remote.origin.url"]).trim(),
            format!("file://{url}")
        );
        // Two more commits and a tag arrive by fetch and pull.
        for s in &t.side {
            commit(s, "again.txt", b"a\n", "again");
            run(s, &["tag", "-a", "v10", "-m", "ten"]);
            run(s, &["push", "-q", "origin", "main", "v10"]);
        }
        let f = t.pyn_cmd(&pyn_dir, &["fetch", "origin"]);
        assert_eq!(f.code, 0, "{}", f.stderr);
        assert!(f.stderr.contains("[new tag]"), "{}", f.stderr);
        t.git_cmd(&git_dir, &["fetch", "origin"]);
        assert_eq!(
            run(
                &git_dir,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            ),
            run(
                &pyn_dir,
                &["for-each-ref", "--format=%(refname) %(objectname)"]
            )
        );
        for s in &t.side {
            commit(s, "last.txt", b"l\n", "last");
            run(s, &["push", "-q", "origin", "main"]);
        }
        let p = t.pyn_cmd(&pyn_dir, &["pull"]);
        assert_eq!(p.code, 0, "{}", p.stderr);
        assert_eq!(
            run(&pyn_dir, &["rev-parse", "HEAD"]).trim(),
            run(&t.remote[1], &["rev-parse", "main"]).trim()
        );
        fsck(&pyn_dir);
    }
}

#[test]
fn a_relative_source_is_recorded_as_an_absolute_path() {
    let t = Twin::new("sha1", true);
    let rel = format!("./{}", t.remote[1].file_name().unwrap().to_str().unwrap());
    let c = t.pyn_cmd(&t.root, &["clone", "-q", &rel, "rel"]);
    assert_eq!(c.code, 0, "{}", c.stderr);
    let url = run(&t.root.join("rel"), &["config", "remote.origin.url"]);
    assert_eq!(url.trim(), t.root.join(&rel).display().to_string());
    let bare_name = t.pyn_cmd(&t.root, &["clone", "-q", "remote-pyn", "named"]);
    assert_eq!(bare_name.code, 0, "{}", bare_name.stderr);
    assert_eq!(
        run(&t.root.join("named"), &["rev-parse", "HEAD"]),
        run(&t.remote[1], &["rev-parse", "HEAD"])
    );
}

#[test]
fn a_large_history_crosses_pack_and_sideband_packet_boundaries() {
    let t = Twin::new("sha1", true);
    let big: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    commit(&t.side[1], "big.bin", &big, "big");
    run(&t.side[1], &["push", "-q", "origin", "main"]);
    let c = t.pyn_cmd(
        &t.root,
        &["clone", "-q", &t.remote[1].display().to_string(), "again"],
    );
    assert_eq!(c.code, 0, "{}", c.stderr);
    assert_eq!(fs::read(t.root.join("again/big.bin")).unwrap(), big);
    fsck(&t.root.join("again"));
    let o = t.pyn_cmd(&t.side[1], &["push", "origin", "main:copy"]);
    assert_eq!(o.code, 0, "{}", o.stderr);
}

#[test]
fn something_that_is_not_a_readable_repository_is_refused() {
    let t = Twin::new("sha1", true);
    let plain = t.root.join("plain");
    fs::create_dir_all(&plain).unwrap();
    for url in [
        plain.display().to_string(),
        t.root.join("absent").display().to_string(),
        format!("file://{}", plain.display()),
    ] {
        for args in [vec!["ls-remote", &url], vec!["fetch", &url]] {
            let out = t.pyn_cmd(&t.side[1], &args);
            assert_eq!(out.code, 128, "{url}: {}", out.stderr);
            assert!(
                out.stderr
                    .contains("does not appear to be a git repository"),
                "{}",
                out.stderr
            );
        }
    }
    let objects = t.remote[1].join("objects");
    fs::set_permissions(&objects, fs::Permissions::from_mode(0o000)).unwrap();
    let unreadable = fs::read_dir(&objects).is_err();
    let out = t.pyn_cmd(
        &t.side[1],
        &["ls-remote", &t.remote[1].display().to_string()],
    );
    fs::set_permissions(&objects, fs::Permissions::from_mode(0o755)).unwrap();
    if unreadable {
        assert_eq!(out.code, 128, "{}", out.stderr);
        assert!(
            out.stderr.contains("cannot read the objects"),
            "{}",
            out.stderr
        );
    }
}

#[test]
fn what_cannot_be_run_or_honoured_is_refused_and_nothing_changes() {
    let t = Twin::new("sha1", true);
    t.commit_both("more.txt");
    let before = refs(&t.remote[1]);
    let hook = t.remote[1].join("hooks/pre-receive");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    script::write_script(&hook, "exit 0\n");
    let out = t.pyn_cmd(&t.side[1], &["push", "origin", "main"]);
    assert_ne!(out.code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("pre-receive"), "{}", out.stderr);
    assert_eq!(refs(&t.remote[1]), before);
    fs::remove_file(&hook).unwrap();
    for (key, value, cmd) in [
        ("receive.fsckObjects", "true", "push"),
        ("uploadpack.hideRefs", "refs/heads/feature", "fetch"),
        ("receive.hideRefs", "refs/heads/feature", "push"),
    ] {
        run(&t.remote[1], &["config", key, value]);
        let out = t.pyn_cmd(&t.side[1], &[cmd, "origin", "main"]);
        assert_ne!(out.code, 0, "{key}: {}", out.stderr);
        assert!(
            out.stderr.contains(&key.to_lowercase()),
            "{key}: {}",
            out.stderr
        );
        assert_eq!(refs(&t.remote[1]), before, "{key}");
        run(&t.remote[1], &["config", "--unset", key]);
    }
    let t = Twin::new("sha1", false);
    t.configure("receive.denyCurrentBranch", "updateInstead");
    t.commit_both("more.txt");
    let before = refs(&t.remote[1]);
    let out = t.pyn_cmd(&t.side[1], &["push", "origin", "main"]);
    assert_ne!(out.code, 0, "{}", out.stderr);
    assert_eq!(refs(&t.remote[1]), before);
}

#[test]
fn a_push_of_objects_the_remote_lacks_into_a_sha256_remote_with_a_mismatch_is_refused() {
    let sha1 = Twin::new("sha1", true);
    let sha256 = Twin::new("sha256", true);
    let out = sha1.pyn_cmd(
        &sha1.side[1],
        &["push", &sha256.remote[1].display().to_string(), "main"],
    );
    assert_eq!(out.code, 128, "{}", out.stderr);
    assert!(
        out.stderr.contains("mismatched algorithms"),
        "{}",
        out.stderr
    );
}

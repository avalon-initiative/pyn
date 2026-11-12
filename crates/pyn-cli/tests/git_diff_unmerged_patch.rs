//! The work tree patch of `pyn diff` for a path with both an `ours` and a `theirs` stage prints git's combined diff;
//! compared with real git on identical conflicted repositories. Real git is the oracle for the patch text only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Pair {
    _tmp: TempDir,
    oracle: PathBuf,
    ours: PathBuf,
}

/// Two repositories left in the middle of the same conflicted merge: `f` (both sides), `g` (deleted by theirs).
fn conflicted(format: &str) -> Pair {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (oracle, ours) = (root.join("oracle"), root.join("ours"));
    for repo in [&oracle, &ours] {
        init(repo, format, false);
        run(repo, &["config", "merge.conflictStyle", "diff3"]);
        fs::write(repo.join("g"), "g1\ng2\n").unwrap();
        commit(repo, "f", b"a\nb\nc\nd\ne\n", "base");
        run(repo, &["add", "g"]);
        run(repo, &["commit", "-q", "-m", "g"]);
        run(repo, &["checkout", "-q", "-b", "side"]);
        commit(repo, "f", b"a\nR\nc\nd\ne\n", "theirs");
        run(repo, &["rm", "-q", "g"]);
        run(repo, &["commit", "-q", "-m", "rm g"]);
        run(repo, &["checkout", "-q", "main"]);
        commit(repo, "f", b"a\nP\nc\nd\ne\n", "ours");
        commit(repo, "g", b"g1\nG\n", "g");
        let _ = git(repo).args(["merge", "-q", "side"]).output().unwrap();
    }
    Pair {
        _tmp: tmp,
        oracle,
        ours,
    }
}

fn pyn(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
        .current_dir(dir)
        .args(args)
        .env_clear()
        .env("PATH", path_env())
        .env("HOME", dir.join(".git"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn same(p: &Pair, args: &[&str]) {
    let want =
        String::from_utf8_lossy(&git(&p.oracle).args(args).output().unwrap().stdout).into_owned();
    assert!(want.contains("diff --cc f"), "{args:?}: {want}");
    assert_eq!(pyn(&p.ours, args), want, "{args:?}");
}

fn edit(p: &Pair, f: impl Fn(&Path)) {
    f(&p.oracle);
    f(&p.ours);
}

#[test]
fn the_marked_file_prints_a_combined_patch() {
    for format in FORMATS {
        let p = conflicted(format);
        same(&p, &["diff"]);
        same(&p, &["diff", "--full-index"]);
        same(&p, &["diff", "-U0"]);
        same(&p, &["diff", "--", "f"]);
    }
}

#[test]
fn a_resolved_by_hand_file_prints_the_remaining_differences() {
    for format in FORMATS {
        let p = conflicted(format);
        edit(&p, |d| {
            fs::write(d.join("f"), "a\nP\nc\nd\ne\nnew\n").unwrap()
        });
        same(&p, &["diff"]);
        edit(&p, |d| fs::write(d.join("f"), "a\nzz\nc\nd\ne\n").unwrap());
        same(&p, &["diff"]);
    }
}

#[test]
fn a_changed_mode_and_a_missing_file_keep_their_header() {
    let p = conflicted("sha1");
    edit(&p, |d| {
        fs::set_permissions(d.join("f"), fs::Permissions::from_mode(0o755)).unwrap()
    });
    same(&p, &["diff"]);
    edit(&p, |d| fs::remove_file(d.join("f")).unwrap());
    same(&p, &["diff"]);
}

#[test]
fn a_path_with_one_conflict_stage_still_prints_only_its_name() {
    let p = conflicted("sha1");
    let want = String::from_utf8_lossy(&git(&p.oracle).args(["diff"]).output().unwrap().stdout)
        .into_owned();
    assert!(want.contains("* Unmerged path g"), "{want}");
    assert_eq!(pyn(&p.ours, &["diff"]), want);
}

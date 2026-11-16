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

fn outcome(program: &mut Command) -> (String, Option<i32>) {
    let out = program.output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.code(),
    )
}

/// Masks the new id of single-parent raw rows: whether git prints the blob id or zeros for a work tree file differs by release.
fn mask_new_ids(out: &str) -> String {
    out.split_inclusive('\n')
        .map(|line| match line.strip_prefix(':') {
            Some(row) if !row.starts_with(':') => {
                let mut f: Vec<&str> = row.splitn(5, ' ').collect();
                if f.len() == 5 {
                    f[3] = "<id>";
                }
                format!(":{}", f.join(" "))
            }
            _ => line.to_string(),
        })
        .collect()
}

/// Output and exit status of git and pyn must agree, with the combined patch among them.
fn same_result(p: &Pair, args: &[&str]) {
    let mut oracle = git(&p.oracle);
    let want = outcome(oracle.args(args));
    let mut ours = Command::new(env!("CARGO_BIN_EXE_pyn"));
    ours.current_dir(&p.ours)
        .args(args)
        .env_clear()
        .env("PATH", path_env())
        .env("HOME", p.ours.join(".git"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let got = outcome(&mut ours);
    assert_eq!(
        (mask_new_ids(&got.0), got.1),
        (mask_new_ids(&want.0), want.1),
        "{args:?}"
    );
}

fn resolve_g(p: &Pair) {
    edit(p, |d| {
        run(d, &["rm", "-q", "-f", "g"]);
    });
}

const COMBINED_FORMS: &[&[&str]] = &[
    &["diff", "--stat", "-p"],
    &["diff", "--numstat", "-p"],
    &["diff", "--shortstat", "-p"],
    &["diff", "--dirstat", "-p"],
    &["diff", "--dirstat=lines", "-p"],
    &["diff", "--summary", "-p"],
    &["diff", "-p", "--stat", "--summary"],
    &["diff", "--exit-code"],
    &["diff", "--exit-code", "-p"],
    &["diff", "--quiet"],
    &["diff", "--exit-code", "--stat"],
    &["diff", "--exit-code", "--numstat"],
    &["diff", "--exit-code", "--name-only"],
    &["diff", "--exit-code", "--", "g"],
    &["diff", "--stat", "--summary"],
    &["diff", "--numstat", "--summary"],
    &["diff", "--stat"],
    &["diff", "--numstat"],
    &["diff", "--shortstat"],
    &["diff", "--dirstat"],
    &["diff", "--summary"],
    &["diff", "--check"],
    &["diff", "--quiet", "--check"],
    &["diff", "--check", "--exit-code"],
    &["diff", "--raw", "-p"],
    &["diff", "--patch-with-raw"],
    &["diff", "--raw", "-p", "-z"],
    &["diff", "--raw", "-p", "--abbrev=12"],
    &["diff", "--raw", "-p", "--exit-code"],
    &["diff", "--raw", "-p", "--", "f"],
    &["diff", "--raw"],
];

#[test]
fn statistics_next_to_a_patch_lead_with_the_combined_patch() {
    for format in FORMATS {
        let p = conflicted(format);
        for args in COMBINED_FORMS {
            same_result(&p, args);
        }
    }
}

#[test]
fn a_conflict_alone_exits_zero_with_exit_code_and_one_with_quiet() {
    let p = conflicted("sha1");
    resolve_g(&p);
    for args in COMBINED_FORMS {
        same_result(&p, args);
    }
}

#[test]
fn another_changed_file_keeps_its_rows_and_the_exit_status() {
    let p = conflicted("sha1");
    resolve_g(&p);
    edit(&p, |d| fs::write(d.join("h"), "new\n").unwrap());
    edit(&p, |d| {
        run(d, &["add", "-N", "h"]);
    });
    for args in COMBINED_FORMS {
        same_result(&p, args);
    }
}

#[test]
fn the_combined_patch_comes_before_every_other_patch() {
    let p = conflicted("sha1");
    edit(&p, |d| {
        fs::write(d.join("a0"), "x\n").unwrap();
        run(d, &["add", "-N", "a0"]);
    });
    for args in [&["diff"][..], &["diff", "--summary"], &["diff", "-U0"]] {
        same_result(&p, args);
    }
}

#[test]
fn a_hand_edited_conflict_is_checked_and_counted() {
    let p = conflicted("sha1");
    edit(&p, |d| {
        fs::write(d.join("f"), "a  \nP\nc\nd\ne\nnew\n").unwrap()
    });
    for args in COMBINED_FORMS {
        same_result(&p, args);
    }
}

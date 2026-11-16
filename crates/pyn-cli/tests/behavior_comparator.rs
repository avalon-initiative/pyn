//! Unit tests of the shared behavior comparator (`common/behavior.rs`).

#[path = "common/oracle.rs"]
mod oracle;

#[path = "common/behavior.rs"]
mod behavior;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use behavior::{Format, Ran, Snapshot, Stderr, assert_same_behavior, assert_same_state};
use tempfile::{TempDir, tempdir};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
}

fn repo() -> (TempDir, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    git(&dir, &["init", "-q", "-b", "main"]);
    fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "c1"]);
    (tmp, dir)
}

fn ran(code: i32, out: &str, err: &str) -> Ran {
    Ran {
        code,
        stdout: out.into(),
        stderr: err.into(),
    }
}

fn panic_text(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let err = std::panic::catch_unwind(f).unwrap_err();
    err.downcast_ref::<String>().cloned().unwrap_or_default()
}

#[test]
fn identical_repositories_match() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    assert!(Snapshot::of(&a).diff(&Snapshot::of(&b)).is_none());
}

#[test]
fn a_differing_ref_fails_readably() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    git(&b, &["branch", "extra"]);
    let msg = panic_text(|| assert_same_state("t", &a, &b));
    assert!(
        msg.contains("[refs] only in second: refs/heads/extra"),
        "{msg}"
    );
}

#[test]
fn a_differing_index_entry_fails() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    fs::write(b.join("n.txt"), "n\n").unwrap();
    git(&b, &["add", "n.txt"]);
    let msg = panic_text(|| assert_same_state("t", &a, &b));
    assert!(
        msg.contains("[index] only in second:") && msg.contains("n.txt"),
        "{msg}"
    );
}

#[test]
fn a_differing_file_mode_fails() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    fs::set_permissions(b.join("a.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    let msg = panic_text(|| assert_same_state("t", &a, &b));
    assert!(msg.contains("[work tree]") && msg.contains("exec"), "{msg}");
}

#[test]
fn differing_content_fails() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    fs::write(b.join("a.txt"), "two\n").unwrap();
    let msg = panic_text(|| assert_same_state("t", &a, &b));
    assert!(msg.contains("[work tree]") && msg.contains("two"), "{msg}");
}

#[test]
fn an_operation_in_progress_is_compared() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    fs::create_dir(b.join(".git/rebase-apply")).unwrap();
    let msg = panic_text(|| assert_same_state("t", &a, &b));
    assert!(
        msg.contains("[operation] only in second: rebase-apply"),
        "{msg}"
    );
}

#[test]
fn stderr_wording_and_human_stdout_do_not_matter() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    let want = ran(1, "Applied patch\n", "error: git says this\n");
    let got = ran(1, "done\n", "pyn: something else\n");
    assert_same_behavior("t", Format::Human, Stderr::Ignored, (&want, &a), (&got, &b));
}

#[test]
fn stderr_is_not_compared_by_default() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    let (silent, noisy) = (ran(0, "", ""), ran(0, "", "oops\n"));
    assert_same_behavior(
        "t",
        Format::Human,
        Stderr::Ignored,
        (&silent, &a),
        (&noisy, &b),
    );
    assert_same_behavior(
        "t",
        Format::Human,
        Stderr::Ignored,
        (&noisy, &a),
        (&silent, &b),
    );
}

#[test]
fn quiet_demands_silence_from_both() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    let (silent, noisy) = (ran(0, "", ""), ran(0, "", "oops\n"));
    assert_same_behavior(
        "t",
        Format::Human,
        Stderr::Quiet,
        (&silent, &a),
        (&silent, &b),
    );
    let msg = panic_text(|| {
        assert_same_behavior(
            "t",
            Format::Human,
            Stderr::Quiet,
            (&silent, &a),
            (&noisy, &b),
        )
    });
    assert!(msg.contains("stderr of t must be empty"), "{msg}");
}

#[test]
fn machine_stdout_is_compared_and_human_is_not() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    let want = ran(0, "1\t0\tf\n", "");
    let got = ran(0, "2\t0\tf\n", "");
    assert_same_behavior("t", Format::Human, Stderr::Ignored, (&want, &a), (&got, &b));
    let msg = panic_text(|| {
        assert_same_behavior(
            "t",
            Format::Machine,
            Stderr::Ignored,
            (&want, &a),
            (&got, &b),
        )
    });
    assert!(msg.contains("stdout of t"), "{msg}");
}

#[test]
fn stable_codes_are_exact_and_others_compare_by_class() {
    let (_a, a) = repo();
    let (_b, b) = repo();
    let same = |w: i32, g: i32| {
        assert_same_behavior(
            "t",
            Format::Human,
            Stderr::Ignored,
            (&ran(w, "", ""), &a),
            (&ran(g, "", ""), &b),
        )
    };
    same(3, 7);
    assert!(std::panic::catch_unwind(|| same(128, 1)).is_err());
    assert!(std::panic::catch_unwind(|| same(0, 1)).is_err());
}

#[test]
fn facts_are_found_in_human_output() {
    ran(0, "Applied patch to 'a.txt'  cleanly.\n", "")
        .has_facts(&["Applied patch to 'a.txt'"], "t");
}

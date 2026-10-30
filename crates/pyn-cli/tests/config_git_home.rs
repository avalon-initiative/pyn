//! `pyn config set|get|list` for pyn's own settings inside a git repository.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn pyn(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pyn"))
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env("PYN_CONFIG_DIR", home.join("cfg"))
        .env("PYN_USER", "alice")
        .output()
        .unwrap()
}

fn ok(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = pyn(cwd, home, args);
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn repo() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let (repo, home) = (base.join("repo"), base.join("home"));
    fs::create_dir_all(repo.join("sub")).unwrap();
    fs::create_dir_all(&home).unwrap();
    ok(&repo, &home, &["init-git"]);
    (tmp, repo, home)
}

#[test]
fn set_writes_the_repository_layer_and_get_reads_it_back() {
    let (_tmp, repo, home) = repo();
    ok(
        &repo.join("sub"),
        &home,
        &["config", "set", "editor", "ed1"],
    );
    let file = fs::read_to_string(repo.join(".pyn/local_only/config.toml")).unwrap();
    assert!(file.contains("editor = \"ed1\""), "{file}");
    assert_eq!(ok(&repo, &home, &["config", "get", "editor"]).trim(), "ed1");
    assert_eq!(
        ok(&repo, &home, &["config", "get", "--local", "editor"]).trim(),
        "ed1"
    );
    assert!(ok(&repo, &home, &["config", "list", "--local"]).contains("ed1"));
}

#[test]
fn local_flag_is_accepted_and_global_stays_user_wide() {
    let (_tmp, repo, home) = repo();
    ok(&repo, &home, &["config", "set", "--local", "editor", "ed2"]);
    ok(
        &repo,
        &home,
        &["config", "set", "--global", "editor", "ed3"],
    );
    assert_eq!(ok(&repo, &home, &["config", "get", "editor"]).trim(), "ed2");
    assert_eq!(
        ok(&repo, &home, &["config", "get", "--global", "editor"]).trim(),
        "ed3"
    );
    let out = pyn(
        &repo,
        &home,
        &["config", "set", "--local", "--global", "editor", "x"],
    );
    assert!(!out.status.success());
}

#[test]
fn outside_any_repository_set_still_asks_for_global() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let out = pyn(&base, &base, &["config", "set", "editor", "x"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--global"));
}

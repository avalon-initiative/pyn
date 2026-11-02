//! `pyn policy lint` on a git home: `lockable` against the exclusive paths of `.pyn/pyn.toml`. No server involved.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::tempdir;

fn pyn(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pyn"))
        .current_dir(dir)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TERM", "dumb")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap()
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn repo() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dir = root.join("w");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&dir).unwrap();
    assert!(pyn(&dir, &home, &["init-git"]).status.success());
    (tmp, dir, home)
}

#[test]
fn lists_both_kinds_of_disagreement() {
    let (_t, dir, home) = repo();
    write(&dir, ".gitattributes", "*.psd lockable\n*.map lockable\n");
    write(
        &dir,
        ".pyn/pyn.toml",
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"*.map\", \"*.bin\"]\n",
    );
    for f in ["a.psd", "b.map", "c.bin", "d.txt"] {
        write(&dir, f, "x");
    }
    assert!(pyn(&dir, &home, &["add", "-A"]).status.success());
    let o = pyn(&dir, &home, &["policy", "lint"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0].split_whitespace().next(), Some("MISMATCH"));
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[1].contains("lockable, shared") && lines[1].ends_with("a.psd"));
    assert!(lines[2].contains("exclusive, not lockable") && lines[2].ends_with("c.bin"));
}

#[test]
fn agreement_says_so() {
    let (_t, dir, home) = repo();
    write(&dir, ".gitattributes", "*.map lockable\n");
    write(
        &dir,
        ".pyn/pyn.toml",
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"*.map\"]\n",
    );
    write(&dir, "b.map", "x");
    write(&dir, "d.txt", "x");
    assert!(pyn(&dir, &home, &["add", "-A"]).status.success());
    let o = pyn(&dir, &home, &["policy", "lint"]);
    assert_eq!(
        String::from_utf8_lossy(&o.stdout),
        "lockable and exclusive agree\n"
    );
}

#[test]
fn a_missing_policy_is_an_error() {
    let (_t, dir, home) = repo();
    let o = pyn(&dir, &home, &["policy", "lint"]);
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("reading the policy"));
}

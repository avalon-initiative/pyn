//! The clear-text server warning: shown at login and when a server address is stored, once per address, never
//! for loopback; and the server address a user types is stored in its normal form.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const REMOTE: &str = "http://pyn-test.invalid:1";

fn pyn(cfg: &Path, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pyn"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", cfg)
        .env("PYN_CONFIG_DIR", cfg)
        .current_dir(cfg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn login(cfg: &Path, server: &str) -> String {
    let out = pyn(
        cfg,
        &["--server", server, "login", "alice", "--password-stdin"],
        "secret\n",
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn later_command(cfg: &Path, server: &str) -> Output {
    pyn(
        cfg,
        &["--server", server, "--token", "pyn_1_x", "repo", "list"],
        "",
    )
}

fn warns(stderr: &str) -> bool {
    stderr.contains("NOT using TLS")
}

#[test]
fn login_warns_and_later_commands_are_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let err = login(tmp.path(), REMOTE);
    assert!(warns(&err), "{err}");
    assert!(
        err.contains("deployment.md") && err.contains("read by anyone"),
        "{err}"
    );

    let first = later_command(tmp.path(), REMOTE);
    let second = later_command(tmp.path(), REMOTE);
    assert!(!warns(&String::from_utf8_lossy(&first.stderr)));
    assert!(!warns(&String::from_utf8_lossy(&second.stderr)));
    assert_eq!(first.status.code(), second.status.code());
    assert_eq!(first.stdout, second.stdout);
}

#[test]
fn a_new_login_or_a_new_address_warns_again() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(warns(&login(tmp.path(), REMOTE)));
    assert!(warns(&login(tmp.path(), REMOTE)));

    let other = "http://other-pyn-test.invalid:1";
    let err = String::from_utf8_lossy(&later_command(tmp.path(), other).stderr).into_owned();
    assert!(warns(&err), "{err}");
    let err = String::from_utf8_lossy(&later_command(tmp.path(), other).stderr).into_owned();
    assert!(!warns(&err), "{err}");
}

#[test]
fn a_command_that_was_never_warned_about_warns_once() {
    let tmp = tempfile::tempdir().unwrap();
    let first = String::from_utf8_lossy(&later_command(tmp.path(), REMOTE).stderr).into_owned();
    let second = String::from_utf8_lossy(&later_command(tmp.path(), REMOTE).stderr).into_owned();
    assert!(warns(&first) && !warns(&second));
}

#[test]
fn loopback_and_https_never_warn() {
    let tmp = tempfile::tempdir().unwrap();
    for server in [
        "http://localhost:1",
        "http://127.0.0.1:1",
        "http://[::1]:1",
        "https://pyn-test.invalid",
    ] {
        assert!(!warns(&login(tmp.path(), server)), "{server}");
        let err = String::from_utf8_lossy(&later_command(tmp.path(), server).stderr).into_owned();
        assert!(!warns(&err), "{server}");
    }
    let set = pyn(
        tmp.path(),
        &["config", "set", "--global", "server", "http://127.0.0.1:1"],
        "",
    );
    assert!(set.status.success() && !warns(&String::from_utf8_lossy(&set.stderr)));
}

#[test]
fn storing_a_server_warns_and_normalises_it() {
    let tmp = tempfile::tempdir().unwrap();
    let set = pyn(
        tmp.path(),
        &[
            "config",
            "set",
            "--global",
            "server",
            "http:192.168.7.239:8080",
        ],
        "",
    );
    assert!(set.status.success(), "{set:?}");
    let err = String::from_utf8_lossy(&set.stderr);
    assert!(
        warns(&err) && err.contains("http://192.168.7.239:8080"),
        "{err}"
    );
    let got = pyn(tmp.path(), &["config", "get", "server"], "");
    assert_eq!(
        String::from_utf8_lossy(&got.stdout).trim(),
        "http://192.168.7.239:8080"
    );

    let again = pyn(tmp.path(), &["config", "get", "server"], "");
    assert!(!warns(&String::from_utf8_lossy(&again.stderr)));
}

#[test]
fn a_garbage_server_is_refused_and_not_stored() {
    let tmp = tempfile::tempdir().unwrap();
    for bad in ["not a url", "example.com", "ftp://h"] {
        let set = pyn(
            tmp.path(),
            &["config", "set", "--global", "server", bad],
            "",
        );
        assert!(!set.status.success(), "{bad}");
        let err = String::from_utf8_lossy(&set.stderr);
        assert!(
            err.contains("not a server address") && err.contains("https://host"),
            "{err}"
        );
    }
    assert!(!tmp.path().join("config.toml").exists());
}

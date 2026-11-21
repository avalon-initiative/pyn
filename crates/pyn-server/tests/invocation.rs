//! The binary's own arguments, run for real: help and version print and exit, an unknown argument is refused, and none of them starts a server.

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Runs `pyn-server` with `args`; a run that is still going after ten seconds started a server, which is the failure.
fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .args(args)
        .env("PYN_ADDR", "127.0.0.1:0")
        .env_remove("PYN_DATABASE_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > Duration::from_secs(10) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("pyn-server {args:?} did not exit: it started a server");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn help_prints_the_usage_and_exits_without_starting_a_server() {
    for flag in ["--help", "-h"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}");
        let stdout = text(&out.stdout);
        assert!(stdout.contains("usage:"), "{flag}: {stdout}");
        assert!(stdout.contains("recover-admin"), "{flag}");
        assert!(stdout.contains("PYN_*"), "{flag}");
    }
}

#[test]
fn version_prints_the_version_and_exits() {
    for flag in ["--version", "-V"] {
        let out = run(&[flag]);
        assert!(out.status.success(), "{flag}");
        assert_eq!(
            text(&out.stdout).trim(),
            format!("pyn-server {}", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn an_unknown_argument_is_refused_with_the_usage() {
    for arg in ["--bogus", "bogus"] {
        let out = run(&[arg]);
        assert!(!out.status.success(), "{arg}");
        let stderr = text(&out.stderr);
        assert!(
            stderr.contains(&format!("unknown argument {arg}")),
            "{stderr}"
        );
        assert!(stderr.contains("usage:"), "{stderr}");
    }
}

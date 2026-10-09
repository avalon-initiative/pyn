//! The user configuration file: alternate path, layering and the non-interactive failure.

mod common;

use std::process::{Command, Output, Stdio};

const DEAD: &str = "http://127.0.0.1:1";

/// Runs `pyn` with only the variables given, stdin closed, and no terminal.
fn pyn(env: &common::Env, vars: &[(&str, &str)], args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pyn"));
    cmd.args(args)
        .current_dir(env.dir("cfg"))
        .env_clear()
        .env("PYN_USER", "alice")
        .stdin(Stdio::null());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

fn write(dir: &std::path::Path, name: &str, server: &str) -> std::path::PathBuf {
    let file = dir.join(name);
    std::fs::write(&file, format!("server = \"{server}\"\n")).unwrap();
    file
}

#[test]
fn alternate_file_is_read_and_layers_below_env_and_flag() {
    let env = common::start();
    let tmp = tempfile::tempdir().unwrap();
    let good = write(tmp.path(), "good.toml", &env.url);
    let bad = write(tmp.path(), "bad.toml", DEAD);
    let cfg = tmp.path().join("cfgdir");
    let dir = cfg.to_str().unwrap();

    let out = pyn(
        &env,
        &[
            ("PYN_CLI_CONFIG", good.to_str().unwrap()),
            ("PYN_CONFIG_DIR", dir),
        ],
        &["repo", "list"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let with_env = pyn(
        &env,
        &[
            ("PYN_CLI_CONFIG", bad.to_str().unwrap()),
            ("PYN_SERVER", &env.url),
            ("PYN_CONFIG_DIR", dir),
        ],
        &["repo", "list"],
    );
    assert!(with_env.status.success(), "env beats the file");

    let with_flag = pyn(
        &env,
        &[
            ("PYN_CLI_CONFIG", good.to_str().unwrap()),
            ("PYN_SERVER", &env.url),
            ("PYN_CONFIG_DIR", dir),
        ],
        &["--server", DEAD, "repo", "list"],
    );
    assert!(!with_flag.status.success(), "flag beats env and file");
}

#[test]
fn user_file_is_found_under_xdg_config_home() {
    let env = common::start();
    let xdg = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(xdg.path().join("pyn")).unwrap();
    write(&xdg.path().join("pyn"), "config.toml", &env.url);
    let out = pyn(
        &env,
        &[("XDG_CONFIG_HOME", xdg.path().to_str().unwrap())],
        &["repo", "list"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn non_interactive_run_without_a_server_fails_and_names_the_fix() {
    let env = common::start();
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("config.toml");
    let out = pyn(
        &env,
        &[("PYN_CLI_CONFIG", file.to_str().unwrap())],
        &["repo", "list"],
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no server is set") && err.contains("PYN_SERVER"),
        "{err}"
    );
    assert!(!file.exists());

    let init = pyn(
        &env,
        &[("PYN_CLI_CONFIG", file.to_str().unwrap())],
        &["config", "init"],
    );
    assert!(!init.status.success(), "init needs a terminal");
}

#[test]
fn config_set_global_follows_the_alternate_path() {
    let env = common::start();
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("mine.toml");
    let vars = [("PYN_CLI_CONFIG", file.to_str().unwrap())];
    let out = pyn(
        &env,
        &vars,
        &["config", "set", "--global", "server", &env.url],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(std::fs::read_to_string(&file).unwrap().contains(&env.url));
}

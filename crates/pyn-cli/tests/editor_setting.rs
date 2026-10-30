//! Where `pyn commit` finds its editor: each source in turn, in the documented order, with a scripted editor.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Env, REPO, start};

/// A script that writes `msg-<first argument>` over the file it is given as its last argument.
fn script(dir: &Path) -> String {
    let path = dir.join("ed.sh");
    fs::write(
        &path,
        "#!/bin/sh\nfor f; do :; done\nprintf 'msg-%s\\n' \"$1\" > \"$f\"\n",
    )
    .unwrap();
    fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path.display().to_string()
}

fn workspace() -> (Env, PathBuf, String) {
    let env = start();
    env.seed("alice", "a.txt", "one", None);
    let ws = env.dir("ws");
    let source = env.source(REPO);
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--local-store",
            source.as_str(),
            ws.to_str().unwrap(),
        ],
    );
    let ws = ws.canonicalize().unwrap();
    let ed = script(&env.dir("scripts"));
    (env, ws, ed)
}

fn commit(env: &Env, ws: &Path, n: usize, vars: &[(&str, &str)]) -> Output {
    fs::write(ws.join("a.txt"), format!("edit {n}")).unwrap();
    env.ok(ws, "alice", &["add", "a.txt"]);
    env.run_with_vars(ws, "alice", &["commit"], "UTC", vars)
}

fn subject(ws: &Path) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(ws.join(".pyn/local_only/store"))
        .args(["log", "-1", "--format=%s", "main"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn each_source_wins_over_the_ones_after_it() {
    let (env, ws, ed) = workspace();
    let cmd = |tag: &str| format!("{ed} {tag}");
    env.ok(
        &ws,
        "alice",
        &["config", "set", "--global", "editor", &cmd("user")],
    );
    env.ok(
        &ws,
        "alice",
        &["config", "set", "editor", &cmd("workspace")],
    );
    let (visual, editor) = (cmd("visual"), cmd("editor"));
    let (pyn_ed, git_ed) = (cmd("pyn-env"), cmd("git-env"));

    let steps: [(&[(&str, &str)], &str); 4] = [
        (
            &[("GIT_EDITOR", &git_ed), ("PYN_EDITOR", &pyn_ed)],
            "git-env",
        ),
        (&[("PYN_EDITOR", &pyn_ed), ("VISUAL", &visual)], "pyn-env"),
        (&[("VISUAL", &visual), ("EDITOR", &editor)], "workspace"),
        (&[], "workspace"),
    ];
    for (n, (vars, want)) in steps.iter().enumerate() {
        let out = commit(&env, &ws, n, vars);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(subject(&ws), format!("msg-{want}"));
    }
}

#[test]
fn user_setting_then_visual_then_editor() {
    let (env, ws, ed) = workspace();
    let cmd = |tag: &str| format!("{ed} {tag}");
    let (visual, editor) = (cmd("visual"), cmd("editor"));
    let out = commit(&env, &ws, 1, &[("VISUAL", &visual), ("EDITOR", &editor)]);
    assert!(out.status.success());
    assert_eq!(subject(&ws), "msg-visual");
    let out = commit(&env, &ws, 2, &[("EDITOR", &editor)]);
    assert!(out.status.success());
    assert_eq!(subject(&ws), "msg-editor");

    env.ok(
        &ws,
        "alice",
        &["config", "set", "--global", "editor", &cmd("user")],
    );
    let out = commit(&env, &ws, 3, &[("VISUAL", &visual)]);
    assert!(out.status.success());
    assert_eq!(subject(&ws), "msg-user");
    assert_eq!(
        env.ok(&ws, "alice", &["config", "get", "editor"]).trim(),
        cmd("user")
    );
}

#[test]
fn a_dumb_terminal_is_an_error_only_when_nothing_is_configured() {
    let (env, ws, ed) = workspace();
    let out = commit(&env, &ws, 1, &[("TERM", "dumb")]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no editor is configured"), "{err}");

    env.ok(
        &ws,
        "alice",
        &["config", "set", "editor", &format!("{ed} ws")],
    );
    let out = commit(&env, &ws, 2, &[("TERM", "dumb")]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(subject(&ws), "msg-ws");
}

#[test]
fn a_git_home_reads_core_editor_and_the_user_setting() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let ed = script(dir.path());
    let config = dir.path().join("cli.toml");
    let run = |args: &[&str], vars: &[(&str, &str)]| -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(&repo)
            .env("HOME", dir.path())
            .env("TZ", "UTC")
            .env("PYN_CLI_CONFIG", &config)
            .env_remove("PYN_SERVER")
            .env_remove("GIT_EDITOR")
            .env_remove("PYN_EDITOR")
            .env_remove("VISUAL")
            .env_remove("EDITOR")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.com")
            .envs(vars.iter().copied())
            .output()
            .unwrap()
    };
    let ok = |args: &[&str], vars: &[(&str, &str)]| {
        let out = run(args, vars);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    ok(&["init-git"], &[]);
    let log = |n: usize| ok(&["log", "-n", &n.to_string(), "--format=%s"], &[]);
    let edit = |n: usize, vars: &[(&str, &str)]| {
        fs::write(repo.join("f.txt"), format!("v{n}")).unwrap();
        ok(&["add", "f.txt"], &[]);
        ok(&["commit"], vars);
    };

    let visual = format!("{ed} visual");
    edit(1, &[("VISUAL", &visual)]);
    assert_eq!(log(1).trim(), "msg-visual");

    ok(&["config", "core.editor", &format!("{ed} core")], &[]);
    edit(2, &[("VISUAL", &visual)]);
    assert_eq!(log(1).trim(), "msg-core");

    ok(
        &["config", "set", "--global", "editor", &format!("{ed} user")],
        &[],
    );
    edit(3, &[("VISUAL", &visual)]);
    assert_eq!(log(1).trim(), "msg-user");
}

//! `pyn commit -F` and `--allow-empty` on a pyn server home, with and without the local store.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Env, REPO, start};

fn clone_with(env: &Env, name: &str, extra: &[&str]) -> PathBuf {
    let ws = env.dir(name);
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(ws.join(".pyn/local_only/store"))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn seeded(extra: &[&str]) -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Content/m.umap", "map", None);
    let ws = clone_with(&env, "ws", extra);
    (env, ws)
}

fn stage_edit(env: &Env, ws: &Path) {
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(ws, "alice", &["add", "Source/a.cpp"]);
}

#[test]
fn a_message_file_is_the_commit_message() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), "\n\nsubject  \n\n\nbody\n\n").unwrap();
    env.ok(&ws, "alice", &["commit", "-F", "msg.txt"]);
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%B", "main"]),
        "subject\n\nbody\n\n"
    );
}

#[test]
fn dash_reads_the_message_from_standard_input() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    let out = env.run_stdin(&ws, "alice", &["commit", "-F", "-"], "from stdin\n");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%s", "main"]),
        "from stdin\n"
    );
}

#[test]
fn a_missing_message_file_is_an_error_and_commits_nothing() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    let tip = git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["commit", "-F", "nope.txt"]);
    assert!(err.contains("could not read log file 'nope.txt'"), "{err}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), tip);
}

#[test]
fn an_empty_message_file_aborts_as_git_does() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), " \n\n").unwrap();
    let err = env.fails(&ws, "alice", &["commit", "-F", "msg.txt"]);
    assert!(
        err.contains("Aborting commit due to empty commit message."),
        "{err}"
    );
}

#[test]
fn a_message_file_and_dash_m_cannot_be_combined() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), "x\n").unwrap();
    let err = env.fails(&ws, "alice", &["commit", "-F", "msg.txt", "-m", "y"]);
    assert!(
        err.contains("'-m' and '-F' cannot be used together"),
        "{err}"
    );
}

#[test]
fn a_message_file_works_without_the_local_store() {
    let (env, ws) = seeded(&["--no-local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), "published message\n").unwrap();
    env.ok(&ws, "alice", &["commit", "-F", "msg.txt"]);
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    assert!(log.contains("published message"), "{log}");
}

#[test]
fn an_exclusive_path_still_needs_its_lock_with_a_message_file() {
    let (env, ws) = seeded(&["--local-store"]);
    std::fs::set_permissions(
        ws.join("Content/m.umap"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    std::fs::write(ws.join("Content/m.umap"), "edited").unwrap();
    std::fs::write(ws.join("msg.txt"), "x\n").unwrap();
    env.ok(&ws, "bob", &["lock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["add", "Content/m.umap"]);
    assert!(err.contains("locked by bob"), "{err}");
    let err = env.fails(&ws, "alice", &["commit", "-a", "-F", "msg.txt"]);
    assert!(err.contains("locked by bob"), "{err}");
}

#[test]
fn allow_empty_is_refused_because_the_server_refuses_empty_commits() {
    for extra in [&["--local-store"][..], &["--no-local-store"]] {
        let (env, ws) = seeded(extra);
        let out = env.run(&ws, "alice", &["commit", "--allow-empty", "-m", "x"]);
        assert_eq!(out.status.code(), Some(129));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("not supported on a pyn server"), "{err}");
    }
}

#[test]
fn quiet_prints_no_summary_but_still_commits() {
    for (extra, flag) in [
        (&["--local-store"][..], "-q"),
        (&["--local-store"], "--quiet"),
        (&["--no-local-store"], "-q"),
    ] {
        let (env, ws) = seeded(extra);
        stage_edit(&env, &ws);
        let out = env.run(&ws, "alice", &["commit", flag, "-m", "quietly"]);
        assert!(out.status.success(), "{out:?}");
        assert!(out.stdout.is_empty(), "{flag}: {out:?}");
        let log = env.ok(&ws, "alice", &["log", "--commits"]);
        assert!(log.contains("quietly"), "{log}");
    }
}

#[test]
fn quiet_covers_a_commit_of_named_paths() {
    let (env, ws) = seeded(&["--no-local-store"]);
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    let out = env.run(
        &ws,
        "alice",
        &["commit", "-q", "-m", "named", "Source/a.cpp"],
    );
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty(), "{out:?}");
}

#[test]
fn flags_that_change_nothing_here_are_accepted() {
    let (env, ws) = seeded(&["--local-store"]);
    for (i, flags) in [
        &["-n"][..],
        &["--no-verify"],
        &["--verify"],
        &["-v"],
        &["--status"],
        &["--no-status"],
        &["--no-edit"],
        &["-q", "-n", "--no-status", "--no-edit"],
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(ws.join("Source/a.cpp"), format!("edit {i}")).unwrap();
        env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
        let mut args = vec!["commit", "-m", "accepted"];
        args.extend(flags);
        env.ok(&ws, "alice", &args);
    }
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    assert_eq!(log.matches("accepted").count(), 8, "{log}");
}

#[test]
fn flags_that_need_git_are_still_refused_here() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    for flag in [
        &["--allow-empty-message"][..],
        &["--cleanup=strip"],
        &["--reset-author"],
        &["--trailer", "A: b"],
        &["-C", "HEAD"],
    ] {
        let mut args = vec!["commit", "-m", "x"];
        args.extend(flag);
        let out = env.run(&ws, "alice", &args);
        assert_eq!(out.status.code(), Some(129), "{flag:?}");
    }
}

#[test]
fn author_and_date_are_recorded_and_the_committer_stays_the_pusher() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    env.ok(
        &ws,
        "alice",
        &[
            "commit",
            "-m",
            "authored",
            "--author",
            "Pat Doe <pat@example.org>",
            "--date",
            "1700000000 +0200",
        ],
    );
    assert_eq!(
        git(
            &ws,
            &["log", "-1", "--format=%an|%ae|%at|%ai|%cn|%ce", "main"]
        ),
        "Pat Doe|pat@example.org|1700000000|2023-11-15 00:13:20 +0200|alice|alice@pyn.invalid\n"
    );
}

#[test]
fn date_alone_keeps_the_pusher_as_the_author() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    env.ok(
        &ws,
        "alice",
        &[
            "commit",
            "-m",
            "dated",
            "--date",
            "2023-11-14T22:13:20+00:00",
        ],
    );
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%an|%at", "main"]),
        "alice|1700000000\n"
    );
}

#[test]
fn a_bad_author_or_date_commits_nothing() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    let tip = git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["commit", "-m", "x", "--author", "nobody"]);
    assert!(err.contains("not 'Name <email>'"), "{err}");
    let err = env.fails(&ws, "alice", &["commit", "-m", "x", "--date", "yesterday"]);
    assert!(err.contains("invalid date format"), "{err}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), tip);
}

#[test]
fn author_and_date_survive_a_push_and_show_in_log_and_show() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    env.ok(
        &ws,
        "alice",
        &[
            "commit",
            "-m",
            "authored",
            "--author",
            "Pat Doe <pat@example.org>",
            "--date",
            "1700000000 +0200",
        ],
    );
    env.ok(&ws, "alice", &["push"]);
    let fmt = "--format=%an|%ae|%at|%cn";
    let want = "Pat Doe|pat@example.org|1700000000|alice\n";
    let other = clone_with(&env, "other", &["--no-local-store"]);
    assert_eq!(env.ok(&other, "alice", &["log", "-n", "1", fmt]), want);
    assert_eq!(env.ok(&other, "alice", &["show", "--no-patch", fmt]), want);
    let audit = env.ok(&other, "alice", &["audit"]);
    assert!(!audit.contains("Pat Doe"), "{audit}");
}

#[test]
fn an_exclusive_path_with_a_foreign_author_still_needs_the_lock() {
    let (env, ws) = seeded(&["--local-store"]);
    std::fs::set_permissions(
        ws.join("Content/m.umap"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    std::fs::write(ws.join("Content/m.umap"), "edited").unwrap();
    env.ok(&ws, "bob", &["lock", "Content/m.umap"]);
    let err = env.fails(
        &ws,
        "alice",
        &[
            "commit",
            "-a",
            "-m",
            "x",
            "--author",
            "Bob <bob@example.org>",
        ],
    );
    assert!(err.contains("locked by bob"), "{err}");
    env.ok(&ws, "bob", &["unlock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(
        &ws,
        "alice",
        &[
            "commit",
            "-a",
            "-m",
            "x",
            "--author",
            "Bob <bob@example.org>",
        ],
    );
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%an|%cn", "main"]),
        "Bob|alice\n"
    );
}

#[test]
fn author_and_date_are_refused_without_the_local_store() {
    let (env, ws) = seeded(&["--no-local-store"]);
    stage_edit(&env, &ws);
    for flags in [
        &["--author", "A <a@b.c>"][..],
        &["--date", "1700000000 +0000"],
    ] {
        let mut args = vec!["commit", "-m", "x"];
        args.extend(flags);
        let err = env.fails(&ws, "alice", &args);
        assert!(err.contains("needs the local store"), "{err}");
    }
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    assert!(!log.contains(" x"), "{log}");
}

#[test]
fn author_and_date_are_refused_on_a_path_commit() {
    let (env, ws) = seeded(&["--local-store"]);
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    let out = env.run(
        &ws,
        "alice",
        &["commit", "-m", "x", "--author", "A <a@b.c>", "Source/a.cpp"],
    );
    assert_eq!(out.status.code(), Some(129), "{out:?}");
}

#[test]
fn signing_flags_on_the_other_commit_commands_are_refused() {
    let (env, ws) = seeded(&["--local-store"]);
    for cmd in [
        &["merge", "-S", "main"][..],
        &["merge", "--no-gpg-sign", "main"],
        &["cherry-pick", "-S", "HEAD"],
        &["revert", "--gpg-sign=KEY", "HEAD"],
        &["rebase", "-S", "main"],
    ] {
        let err = env.fails(&ws, "alice", cmd);
        assert!(err.contains("gpg-sign"), "{cmd:?}: {err}");
    }
}

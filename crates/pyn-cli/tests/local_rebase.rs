//! `pyn rebase` over unpublished local commits in a workspace with `local_store` on, against a real server. Real git
//! reads the store as an oracle for semantics only.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Env, REPO, start};

fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(ws.join(".pyn/local_only/store"))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

fn write(ws: &Path, path: &str, text: &str) {
    let target = ws.join(path);
    if target.exists() {
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(target, text).unwrap();
}

fn commit_local(env: &Env, ws: &Path, path: &str, text: &str, message: &str) {
    write(ws, path, text);
    env.ok(ws, "alice", &["add", path]);
    env.ok(ws, "alice", &["commit", "-m", message]);
}

/// Three local commits on top of the fetched tip: `one` and `two` change `a.cpp` and `b.cpp`, `three` adds `c.cpp`.
fn with_local() -> (Env, PathBuf) {
    with_seeded(&[])
}

fn with_seeded(extra: &[(&str, &str)]) -> (Env, PathBuf) {
    let env = start();
    for (path, text) in extra {
        env.seed("alice", path, text, None);
    }
    env.seed("alice", "Source/a.cpp", "a0\n", None);
    env.seed("alice", "Source/b.cpp", "b0\n", None);
    env.seed("alice", "Content/m.umap", "map", None);
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
    commit_local(&env, &ws, "Source/a.cpp", "a1\n", "one");
    commit_local(&env, &ws, "Source/b.cpp", "b1\n", "two");
    commit_local(&env, &ws, "Source/c.cpp", "c1\n", "three");
    (env, ws)
}

fn subjects(ws: &Path) -> String {
    git(ws, &["log", "--format=%s", "origin/main..main"])
}

/// An editor that replaces the sheet or message with `body`.
fn editor(env: &Env, ws: &Path, vars: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    env.run_with_vars(ws, "alice", args, "UTC", vars)
}

fn script(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path.to_str().unwrap().to_string()
}

/// Rebases interactively with a sheet written by `sheet`, a function of the three commit ids (oldest first).
fn interactive(
    env: &Env,
    ws: &Path,
    sheet: impl Fn(&[String]) -> String,
    extra: &[&str],
) -> std::process::Output {
    let ids: Vec<String> = git(
        ws,
        &["log", "--reverse", "--format=%H", "origin/main..main"],
    )
    .lines()
    .map(str::to_string)
    .collect();
    let tools = env.dir("tools");
    let seq = script(
        &tools,
        "seq.sh",
        &format!("cat > \"$1\" <<'EOF'\n{}EOF", sheet(&ids)),
    );
    let msg = script(
        &tools,
        "msg.sh",
        "echo \"$(grep -v '^#' \"$1\" | grep -v '^$' | head -1) edited\" > \"$1\"",
    );
    let mut args = vec!["rebase", "-i"];
    args.extend(extra);
    editor(
        env,
        ws,
        &[
            ("GIT_SEQUENCE_EDITOR", seq.as_str()),
            ("GIT_EDITOR", msg.as_str()),
        ],
        &args,
    )
}

fn stdout(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn local_commits_are_reordered_and_the_history_stays_sound() {
    let (env, ws) = with_local();
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\npick {}\npick {}\n", i[2], i[0], i[1]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "two\none\nthree\n");
    assert_eq!(read(&ws, "Source/a.cpp"), "a1\n");
    git(&ws, &["fsck", "--strict"]);
    env.ok(&ws, "alice", &["push"]);
}

#[test]
fn squash_fixup_reword_and_drop_rewrite_the_messages_and_trees() {
    let (env, ws) = with_local();
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nfixup {}\nreword {}\n", i[0], i[1], i[2]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "three edited\none\n");
    assert_eq!(
        git(&ws, &["show", "main~1:Source/b.cpp"]),
        "b1\n",
        "the fixup brought its change into the first commit"
    );

    let (env, ws) = with_local();
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nsquash {}\ndrop {}\n", i[0], i[1], i[2]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "one edited\n");
    assert!(
        !ws.join("Source/c.cpp").exists(),
        "the dropped commit is gone"
    );
    assert_eq!(read(&ws, "Source/b.cpp"), "b1\n");
    git(&ws, &["fsck", "--strict"]);
}

#[test]
fn autosquash_moves_fixup_commits_under_their_target() {
    let (env, ws) = with_local();
    commit_local(&env, &ws, "Source/a.cpp", "a2\n", "fixup! one");
    let tools = env.dir("tools");
    let keep = script(&tools, "keep.sh", ":");
    let out = editor(
        &env,
        &ws,
        &[("GIT_SEQUENCE_EDITOR", keep.as_str())],
        &["rebase", "-i", "--autosquash"],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
    assert_eq!(git(&ws, &["show", "main~2:Source/a.cpp"]), "a2\n");
}

#[test]
fn a_range_with_a_published_commit_is_refused_by_name() {
    let (env, ws) = with_local();
    env.ok(&ws, "alice", &["push"]);
    commit_local(&env, &ws, "Source/a.cpp", "a9\n", "four");
    let published = git(&ws, &["rev-parse", "main~1"]).trim().to_string();
    let err = env.fails(&ws, "alice", &["rebase", "-i", "main~2"]);
    assert!(
        err.contains("already published") && err.contains(&published[..7]) && err.contains("three"),
        "{err}"
    );
    assert!(subjects(&ws).starts_with("four\n"));
    assert_eq!(read(&ws, "Source/a.cpp"), "a9\n");
}

#[test]
fn a_sheet_that_names_a_published_commit_is_refused_and_nothing_changes() {
    let (env, ws) = with_local();
    let published = git(&ws, &["rev-parse", "origin/main"]).trim().to_string();
    let before = git(&ws, &["rev-parse", "main"]);
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {published}\npick {}\n", i[0]),
        &[],
    );
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("already published"),
        "{}",
        stdout(&out)
    );
    assert_eq!(git(&ws, &["rev-parse", "main"]), before);
}

#[test]
fn plain_rebase_is_up_to_date_and_unbuilt_options_are_refused() {
    let (env, ws) = with_local();
    let out = env.ok(&ws, "alice", &["rebase"]);
    assert!(out.contains("up to date"), "{out}");
    for flag in ["--edit-todo", "--quit"] {
        let err = env.fails(&ws, "alice", &["rebase", flag]);
        assert!(err.contains("no rebase in progress"), "{err}");
    }
    for (flag, word) in [
        ("--rebase-merges", "--rebase-merges"),
        ("--root", "--root"),
        ("--fork-point", "--fork-point"),
        ("--update-refs", "--update-refs"),
    ] {
        let err = env.fails(&ws, "alice", &["rebase", flag]);
        assert!(err.contains(word) && err.contains("workspace"), "{err}");
    }
    let tools = env.dir("tools");
    for word in [
        "label onto",
        "reset onto",
        "merge topic",
        "update-ref refs/heads/x",
    ] {
        let ids = git(
            &ws,
            &["log", "--reverse", "--format=%H", "origin/main..main"],
        );
        let first = ids.lines().next().unwrap();
        let line = format!("pick {first}\n{word}\n");
        let seq = script(
            &tools,
            "seq.sh",
            &format!("cat > \"$1\" <<'EOF'\n{line}EOF"),
        );
        let out = editor(
            &env,
            &ws,
            &[("GIT_SEQUENCE_EDITOR", seq.as_str())],
            &["rebase", "-i"],
        );
        assert!(!out.status.success());
        assert!(stdout(&out).contains("not available"), "{}", stdout(&out));
    }
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
}

#[test]
fn a_conflict_stops_with_markers_and_continue_skip_and_abort_conclude_it() {
    let (env, ws) = with_local();
    commit_local(&env, &ws, "Source/a.cpp", "a2\n", "four");
    let sheet = |i: &[String]| {
        format!(
            "pick {}\npick {}\npick {}\npick {}\n",
            i[3], i[0], i[1], i[2]
        )
    };
    let out = interactive(&env, &ws, sheet, &[]);
    assert!(!out.status.success());
    let err = stdout(&out);
    assert!(
        err.contains("could not apply")
            && err.contains("one")
            && err.contains("pyn rebase --continue"),
        "{err}"
    );
    assert!(read(&ws, "Source/a.cpp").contains("<<<<<<<"));
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("rebasing"), "{status}");
    let refused = env.fails(&ws, "alice", &["pull"]);
    assert!(refused.contains("not concluded"), "{refused}");
    assert!(
        env.fails(&ws, "alice", &["rebase", "--continue"])
            .contains("unmerged")
    );

    write(&ws, "Source/a.cpp", "both\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(
        err.contains("could not apply") && err.contains("one"),
        "{err}"
    );
    write(&ws, "Source/a.cpp", "both again\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert!(out.contains("Successfully rebased"), "{out}");
    assert_eq!(subjects(&ws), "three\ntwo\none\nfour\n");
    git(&ws, &["fsck", "--strict"]);

    let (env, ws) = with_local();
    commit_local(&env, &ws, "Source/a.cpp", "a2\n", "four");
    let out = interactive(&env, &ws, sheet, &[]);
    assert!(!out.status.success());
    env.ok(&ws, "alice", &["rebase", "--skip"]);
    assert_eq!(subjects(&ws), "three\ntwo\none\n");

    let (env, ws) = with_local();
    commit_local(&env, &ws, "Source/a.cpp", "a2\n", "four");
    let before_abort = git(&ws, &["rev-parse", "main"]);
    let out = interactive(&env, &ws, sheet, &[]);
    assert!(!out.status.success());
    let out = env.ok(&ws, "alice", &["rebase", "--abort"]);
    assert!(out.contains("given up"), "{out}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), before_abort);
    assert_eq!(read(&ws, "Source/a.cpp"), "a2\n");
}

#[test]
fn a_squash_that_conflicts_concludes_with_the_combined_message() {
    let (env, ws) = with_local();
    commit_local(&env, &ws, "Source/a.cpp", "a2\n", "four");
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nsquash {}\n", i[1], i[3]),
        &[],
    );
    assert!(!out.status.success(), "{}", stdout(&out));
    write(&ws, "Source/a.cpp", "merged\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let tools = env.dir("tools");
    let msg = script(
        &tools,
        "joined.sh",
        "grep -v '^#' \"$1\" | grep -v '^$' | tr '\\n' '+' > \"$1.new\"; mv \"$1.new\" \"$1\"",
    );
    let out = editor(
        &env,
        &ws,
        &[("GIT_EDITOR", msg.as_str())],
        &["rebase", "--continue"],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "two+four+\n");
}

#[test]
fn an_exclusive_path_both_commits_changed_is_a_conflict_even_under_the_lock() {
    let (env, ws) = with_local();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(&env, &ws, "Content/m.umap", "map v1", "map one");
    commit_local(&env, &ws, "Content/m.umap", "map v2", "map two");
    let out = interactive(
        &env,
        &ws,
        |i| {
            format!(
                "pick {}\npick {}\npick {}\npick {}\npick {}\n",
                i[0], i[1], i[2], i[4], i[3]
            )
        },
        &[],
    );
    assert!(!out.status.success());
    let err = stdout(&out);
    assert!(
        err.contains("CONFLICT (exclusive): Content/m.umap") && err.contains("pyn lock"),
        "{err}"
    );
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let ours = ["checkout", "--theirs", "--", "Content/m.umap"];
    env.ok(&ws, "alice", &ours);
    env.ok(&ws, "alice", &["add", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(err.contains("pyn lock Content/m.umap"), "{err}");
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(
        err.contains("map one"),
        "the next commit conflicts too: {err}"
    );
    env.ok(&ws, "alice", &ours);
    env.ok(&ws, "alice", &["add", "Content/m.umap"]);
    env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert_eq!(read(&ws, "Content/m.umap"), "map v1");
    assert_eq!(subjects(&ws), "map one\nmap two\nthree\ntwo\none\n");
}

#[test]
fn staged_changes_and_an_unfetched_branch_stop_it_before_anything_changes() {
    let (env, ws) = with_local();
    write(&ws, "Source/b.cpp", "staged\n");
    env.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    let err = env.fails(&ws, "alice", &["rebase", "-i"]);
    assert!(err.contains("staged changes"), "{err}");
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(err.contains("no rebase in progress"), "{err}");
}

fn ids(ws: &Path) -> Vec<String> {
    git(
        ws,
        &["log", "--reverse", "--format=%H", "origin/main..main"],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

fn head(ws: &Path) -> String {
    git(ws, &["rev-parse", "main"]).trim().to_string()
}

#[test]
fn edit_stops_with_the_commit_made_and_continue_replays_the_rest() {
    let (env, ws) = with_local();
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nedit {}\npick {}\n", i[0], i[1], i[2]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("Stopped at"), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "two\none\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "b1\n");
    assert!(!ws.join("Source/c.cpp").exists());
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("stopped at edit"), "{status}");
    let refused = env.fails(&ws, "alice", &["pull"]);
    assert!(refused.contains("not concluded"), "{refused}");

    commit_local(&env, &ws, "Source/d.cpp", "d1\n", "extra");
    assert_eq!(subjects(&ws), "extra\ntwo\none\n");
    let out = env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert!(out.contains("Successfully rebased"), "{out}");
    assert_eq!(subjects(&ws), "three\nextra\ntwo\none\n");
    assert_eq!(read(&ws, "Source/c.cpp"), "c1\n");
    assert!(!env.ok(&ws, "alice", &["status"]).contains("rebasing"));
    git(&ws, &["fsck", "--strict"]);
    env.ok(&ws, "alice", &["push"]);
}

#[test]
fn continue_at_a_stop_refuses_staged_and_uncommitted_changes() {
    let (env, ws) = with_local();
    let out = interactive(
        &env,
        &ws,
        |i| format!("edit {}\npick {}\npick {}\n", i[0], i[1], i[2]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    write(&ws, "Source/a.cpp", "changed\n");
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(err.contains("uncommitted changes"), "{err}");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(err.contains("staged changes"), "{err}");
    env.ok(&ws, "alice", &["commit", "-m", "tweak"]);
    env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert_eq!(subjects(&ws), "three\ntwo\ntweak\none\n");
}

#[test]
fn abort_at_a_stop_puts_back_the_branch_and_the_files() {
    let (env, ws) = with_local();
    let before = head(&ws);
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nbreak\npick {}\npick {}\n", i[2], i[0], i[1]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "three\n");
    write(&ws, "Source/a.cpp", "scribble\n");
    let out = env.ok(&ws, "alice", &["rebase", "--abort"]);
    assert!(out.contains("given up"), "{out}");
    assert_eq!(head(&ws), before);
    assert_eq!(read(&ws, "Source/a.cpp"), "a1\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "b1\n");
    assert!(!env.ok(&ws, "alice", &["status"]).contains("rebasing"));
}

#[test]
fn break_stops_and_continue_and_skip_go_on() {
    let (env, ws) = with_local();
    let sheet = |i: &[String]| format!("pick {}\nbreak\npick {}\npick {}\n", i[0], i[1], i[2]);
    let out = interactive(&env, &ws, sheet, &[]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "one\n");
    env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert_eq!(subjects(&ws), "three\ntwo\none\n");

    let (env, ws) = with_local();
    let out = interactive(&env, &ws, sheet, &[]);
    assert!(out.status.success(), "{}", stdout(&out));
    write(&ws, "Source/a.cpp", "scribble\n");
    env.ok(&ws, "alice", &["rebase", "--skip"]);
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
    assert_eq!(read(&ws, "Source/a.cpp"), "a1\n");
}

#[test]
fn exec_runs_after_each_commit_with_the_working_tree_at_that_commit() {
    let (env, ws) = with_local();
    let log = env.dir("log").join("seen");
    let line = format!("cat Source/a.cpp Source/b.cpp >> {}", log.display());
    let out = env.ok(&ws, "alice", &["rebase", "--exec", &line]);
    assert!(out.contains("Successfully rebased"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        "a1\nb0\na1\nb1\na1\nb1\n"
    );
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
}

#[test]
fn a_failing_exec_stops_and_continue_goes_past_it() {
    let (env, ws) = with_local();
    let before = head(&ws);
    let err = env.fails(&ws, "alice", &["rebase", "-x", "test ! -f Source/c.cpp"]);
    assert!(err.contains("execution failed"), "{err}");
    assert_eq!(head(&ws), before, "the third commit was reached");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("stopped at exec")
    );
    let out = env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert!(out.contains("Successfully rebased"), "{out}");

    let (env, ws) = with_local();
    let err = env.fails(&ws, "alice", &["rebase", "-x", "echo x >> Source/a.cpp"]);
    assert!(err.contains("left changes"), "{err}");
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(err.contains("uncommitted changes"), "{err}");
    env.ok(&ws, "alice", &["checkout", "--", "Source/a.cpp"]);
    env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("stopped at exec")
    );
    env.ok(&ws, "alice", &["rebase", "--abort"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "a1\n");
}

#[test]
fn exec_and_break_lines_run_from_the_sheet() {
    let (env, ws) = with_local();
    let log = env.dir("log").join("seen");
    let line = format!(
        "if [ -f Source/c.cpp ]; then echo c; else echo none; fi >> {}",
        log.display()
    );
    let out = interactive(
        &env,
        &ws,
        |i| {
            format!(
                "pick {}\npick {}\nexec {line}\npick {}\nexec {line}\n",
                i[0], i[1], i[2]
            )
        },
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
    let seen = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(seen, "none\nc\n", "the first exec ran before c.cpp existed");
}

#[test]
fn edit_todo_changes_the_commands_still_to_do() {
    let (env, ws) = with_local();
    let all = ids(&ws);
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nbreak\npick {}\npick {}\n", i[0], i[1], i[2]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    let tools = env.dir("tools");
    let seq = script(
        &tools,
        "todo.sh",
        &format!(
            "cat > \"$1\" <<'EOF'\ndrop {}\npick {}\nEOF",
            all[1], all[2]
        ),
    );
    let out = editor(
        &env,
        &ws,
        &[("GIT_SEQUENCE_EDITOR", seq.as_str())],
        &["rebase", "--edit-todo"],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert_eq!(subjects(&ws), "three\none\n");
    assert!(!ws.join("Source/b.cpp").exists() || read(&ws, "Source/b.cpp") == "b0\n");

    let (env, ws) = with_local();
    let published = git(&ws, &["rev-parse", "origin/main"]).trim().to_string();
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nbreak\npick {}\n", i[0], i[1]),
        &[],
    );
    assert!(out.status.success());
    let seq = script(
        &env.dir("tools"),
        "todo.sh",
        &format!("echo 'pick {published}' > \"$1\""),
    );
    let out = editor(
        &env,
        &ws,
        &[("GIT_SEQUENCE_EDITOR", seq.as_str())],
        &["rebase", "--edit-todo"],
    );
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("already published"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn quit_forgets_the_rebase_and_leaves_the_branch_and_files() {
    let (env, ws) = with_local();
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nbreak\npick {}\npick {}\n", i[0], i[1], i[2]),
        &[],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    let out = env.ok(&ws, "alice", &["rebase", "--quit"]);
    assert!(out.contains("forgotten"), "{out}");
    assert_eq!(subjects(&ws), "one\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "b0\n");
    assert!(!env.ok(&ws, "alice", &["status"]).contains("rebasing"));
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(err.contains("no rebase in progress"), "{err}");
}

/// `with_local` over a published five-line file whose first line a fourth local commit changes.
fn with_lines() -> (Env, PathBuf) {
    let (env, ws) = with_seeded(&[("Source/m.cpp", "l1\nl2\nl3\nl4\nl5\n")]);
    commit_local(&env, &ws, "Source/m.cpp", "L1\nl2\nl3\nl4\nl5\n", "top");
    (env, ws)
}

#[test]
fn autostash_puts_uncommitted_changes_aside_and_back_over_the_result() {
    let (env, ws) = with_lines();
    let drop_top = |i: &[String]| {
        format!(
            "pick {}\npick {}\npick {}\ndrop {}\n",
            i[0], i[1], i[2], i[3]
        )
    };
    write(&ws, "Source/m.cpp", "L1\nl2\nl3\nl4\nX5\n");
    let out = interactive(&env, &ws, drop_top, &[]);
    assert!(!out.status.success());
    assert!(
        stdout(&out).contains("would be overwritten"),
        "{}",
        stdout(&out)
    );
    assert_eq!(subjects(&ws), "top\nthree\ntwo\none\n");
    assert_eq!(read(&ws, "Source/m.cpp"), "L1\nl2\nl3\nl4\nX5\n");

    let out = interactive(&env, &ws, drop_top, &["--autostash"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("Applied autostash"),
        "{}",
        stdout(&out)
    );
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
    assert_eq!(read(&ws, "Source/m.cpp"), "l1\nl2\nl3\nl4\nX5\n");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("Source/m.cpp"), "{status}");
}

#[test]
fn autostash_conflicts_leave_markers_and_a_refusal_puts_the_changes_back() {
    let (env, ws) = with_lines();
    write(&ws, "Source/m.cpp", "Z1\nl2\nl3\nl4\nl5\n");
    let out = interactive(
        &env,
        &ws,
        |i| {
            format!(
                "pick {}\npick {}\npick {}\ndrop {}\n",
                i[0], i[1], i[2], i[3]
            )
        },
        &["--autostash"],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("resulted in conflicts"),
        "{}",
        stdout(&out)
    );
    assert!(read(&ws, "Source/m.cpp").contains("<<<<<<<"));

    let (env, ws) = with_lines();
    write(&ws, "Source/m.cpp", "Z1\nl2\nl3\nl4\nl5\n");
    let published = git(&ws, &["rev-parse", "origin/main"]).trim().to_string();
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {published}\npick {}\n", i[0]),
        &["--autostash"],
    );
    assert!(!out.status.success());
    assert_eq!(read(&ws, "Source/m.cpp"), "Z1\nl2\nl3\nl4\nl5\n");
}

#[test]
fn autostash_survives_a_stop_and_returns_after_the_continue() {
    let (env, ws) = with_lines();
    write(&ws, "Source/m.cpp", "L1\nl2\nl3\nl4\nX5\n");
    let out = interactive(
        &env,
        &ws,
        |i| {
            format!(
                "pick {}\nbreak\npick {}\npick {}\ndrop {}\n",
                i[0], i[1], i[2], i[3]
            )
        },
        &["--autostash"],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    assert_eq!(read(&ws, "Source/m.cpp"), "l1\nl2\nl3\nl4\nl5\n");
    let out = stdout(&env.run(&ws, "alice", &["rebase", "--continue"]));
    assert!(out.contains("Applied autostash"), "{out}");
    assert_eq!(read(&ws, "Source/m.cpp"), "l1\nl2\nl3\nl4\nX5\n");
    assert_eq!(subjects(&ws), "three\ntwo\none\n");

    let (env, ws) = with_lines();
    write(&ws, "Source/m.cpp", "L1\nl2\nl3\nl4\nX5\n");
    let out = interactive(
        &env,
        &ws,
        |i| format!("pick {}\nbreak\npick {}\n", i[0], i[1]),
        &["--autostash"],
    );
    assert!(out.status.success(), "{}", stdout(&out));
    let out = stdout(&env.run(&ws, "alice", &["rebase", "--abort"]));
    assert!(out.contains("Applied autostash"), "{out}");
    assert_eq!(read(&ws, "Source/m.cpp"), "L1\nl2\nl3\nl4\nX5\n");
    assert_eq!(subjects(&ws), "top\nthree\ntwo\none\n");
}

#[test]
fn no_ff_replays_commits_that_would_stay() {
    let (env, ws) = with_local();
    let before = ids(&ws);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let out = env.ok(&ws, "alice", &["rebase", "--no-ff"]);
    assert!(out.contains("rebase forced"), "{out}");
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
    let after = ids(&ws);
    assert!(before.iter().zip(&after).all(|(a, b)| a != b), "{after:?}");
    git(&ws, &["fsck", "--strict"]);
    let out = env.ok(&ws, "alice", &["rebase"]);
    assert!(out.contains("up to date"), "{out}");
}

#[test]
fn the_branch_argument_names_the_current_branch_or_switches_first() {
    let (env, ws) = with_local();
    let out = env.ok(&ws, "alice", &["rebase", "origin/main", "main"]);
    assert!(out.contains("up to date"), "{out}");
    env.ok(&ws, "alice", &["branch", "topic"]);
    let err = env.fails(&ws, "alice", &["rebase", "origin/main", "topic"]);
    assert!(err.contains("not yet published"), "{err}");
    assert_eq!(subjects(&ws), "three\ntwo\none\n");
}

#[test]
fn an_edit_that_conflicts_stops_again_once_it_is_resolved() {
    let (env, ws) = with_local();
    commit_local(&env, &ws, "Source/a.cpp", "a2\n", "four");
    let out = interactive(
        &env,
        &ws,
        |i| {
            format!(
                "pick {}\nedit {}\npick {}\npick {}\n",
                i[3], i[0], i[1], i[2]
            )
        },
        &[],
    );
    assert!(!out.status.success());
    assert!(stdout(&out).contains("could not apply"), "{}", stdout(&out));
    write(&ws, "Source/a.cpp", "both\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let err = env.fails(&ws, "alice", &["rebase", "--continue"]);
    assert!(
        err.contains("could not apply") && err.contains("one"),
        "{err}"
    );
    write(&ws, "Source/a.cpp", "both again\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.run(&ws, "alice", &["rebase", "--continue"]);
    assert!(out.status.success(), "{}", stdout(&out));
    assert!(stdout(&out).contains("Stopped at"), "{}", stdout(&out));
    assert_eq!(subjects(&ws), "one\nfour\n");
    env.ok(&ws, "alice", &["rebase", "--continue"]);
    assert_eq!(subjects(&ws), "three\ntwo\none\nfour\n");
    git(&ws, &["fsck", "--strict"]);
}

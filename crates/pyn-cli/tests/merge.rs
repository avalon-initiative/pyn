//! `pyn merge` on a pyn server: fast-forward, merge commits, conflicts, resolution, abort and the guards around them.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, REPO, start};

fn clone(env: &Env, name: &str) -> PathBuf {
    let ws = env.dir(name);
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    ws.canonicalize().unwrap()
}

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

fn write(ws: &Path, path: &str, text: &str) {
    std::fs::write(ws.join(path), text).unwrap();
}

fn commit(env: &Env, ws: &Path, path: &str, text: &str) {
    write(ws, path, text);
    env.ok(
        ws,
        "alice",
        &["commit", path, "-m", &format!("edit {path}")],
    );
}

fn commit_msg(env: &Env, ws: &Path, path: &str, text: &str, message: &str) {
    write(ws, path, text);
    env.ok(ws, "alice", &["commit", path, "-m", message]);
}

/// Runs the command and returns what it printed on standard output and standard error, and its exit code.
fn run(env: &Env, ws: &Path, args: &[&str]) -> (Option<i32>, String, String) {
    let out = env.run(ws, "alice", args);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn commit_messages(env: &Env, ws: &Path) -> Vec<String> {
    env.ok(ws, "alice", &["log", "--commits"])
        .lines()
        .skip(1)
        .map(|l| {
            l.split("  ")
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .last()
                .unwrap_or("")
                .to_string()
        })
        .collect()
}

#[test]
fn a_fast_forward_brings_the_files_and_makes_no_commit() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "two");
    write(&ws, "Source/b.cpp", "new");
    env.ok(&ws, "alice", &["commit", "Source/b.cpp", "-m", "add b"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "one");

    let preview = env.ok(&ws, "alice", &["merge", "--preview", "feature"]);
    assert!(
        preview.contains("Fast-forward") && preview.contains("Source/b.cpp"),
        "{preview}"
    );
    assert_eq!(
        read(&ws, "Source/a.cpp"),
        "one",
        "a preview changes nothing"
    );

    let out = env.ok(&ws, "alice", &["merge", "feature"]);
    assert!(out.contains("Fast-forward"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "two");
    assert_eq!(read(&ws, "Source/b.cpp"), "new");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    assert_eq!(commit_messages(&env, &ws).len(), 3, "no merge commit");
    assert!(
        env.ok(&ws, "alice", &["merge", "feature"])
            .contains("Already up to date.")
    );

    let again = env.fails(&ws, "alice", &["merge", "--ff-only", "nope"]);
    assert!(
        again.contains("commit_not_found") || again.contains("not found"),
        "{again}"
    );
}

#[test]
fn diverged_branches_merge_into_a_commit_with_two_parents() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a\n", None);
    env.seed("alice", "Source/b.cpp", "b\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "a from feature\n");
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/b.cpp", "b from main\n");

    let out = env.ok(
        &ws,
        "alice",
        &["merge", "feature", "-m", "bring in feature"],
    );
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "a from feature\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "b from main\n");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    assert_eq!(commit_messages(&env, &ws)[0], "bring in feature");
    let show = env.ok(&ws, "alice", &["show"]);
    assert!(show.contains("bring in feature"), "{show}");
    env.ok(&ws, "alice", &["branch", "-d", "feature"]);
    assert_eq!(env.ok(&ws, "alice", &["verify"]), "");
}

#[test]
fn no_ff_makes_a_merge_commit_and_ff_only_refuses_one() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature\n");
    env.ok(&ws, "alice", &["switch", "main"]);
    let out = env.ok(&ws, "alice", &["merge", "--no-ff", "feature"]);
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "feature\n");
    assert_eq!(
        commit_messages(&env, &ws)[0],
        "Merge branch 'feature' into main"
    );

    env.ok(&ws, "alice", &["switch", "-c", "other"]);
    commit(&env, &ws, "Source/a.cpp", "other\n");
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/b.cpp", "b\n");
    let err = env.fails(&ws, "alice", &["merge", "--ff-only", "other"]);
    assert!(err.contains("not_fast_forward"), "{err}");
}

#[test]
fn strategies_need_a_git_repository() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature\n");
    env.ok(&ws, "alice", &["switch", "main"]);
    for args in [["-s", "ours"], ["-X", "theirs"]] {
        let mut full = vec!["merge"];
        full.extend(args);
        full.push("feature");
        let err = env.fails(&ws, "alice", &full);
        assert!(err.contains("need a git repository"), "{err}");
    }
    assert_eq!(read(&ws, "Source/a.cpp"), "a\n");
}

#[test]
fn text_changed_in_different_places_merges_without_conflict() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "1\n2\n3\n4\n5\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "1\n2\n3\n4\nfive\n");
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/a.cpp", "one\n2\n3\n4\n5\n");
    let (code, out, _) = run(&env, &ws, &["merge", "feature"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains("Auto-merging Source/a.cpp"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "one\n2\n3\n4\nfive\n");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn a_conflict_is_marked_in_the_file_resolved_by_adding_and_concluded_by_committing() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "1\n2\n3\n", None);
    env.seed("alice", "Source/b.cpp", "b\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "1\ntheirs\n3\n");
    write(&ws, "Source/c.cpp", "c\n");
    env.ok(&ws, "alice", &["commit", "Source/c.cpp", "-m", "add c"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/a.cpp", "1\nours\n3\n");

    let (code, out, _) = run(&env, &ws, &["merge", "feature"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("CONFLICT (content): Merge conflict in Source/a.cpp"),
        "{out}"
    );
    assert!(out.contains("Automatic merge failed"), "{out}");
    assert_eq!(
        read(&ws, "Source/a.cpp"),
        "1\n<<<<<<< HEAD\nours\n||||||| base\n2\n=======\ntheirs\n>>>>>>> feature\n3\n"
    );
    assert_eq!(
        read(&ws, "Source/c.cpp"),
        "c\n",
        "the clean part of the merge is applied"
    );

    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("You have unmerged paths"), "{status}");
    assert!(status.contains("unmerged (both modified)"), "{status}");
    assert!(status.contains("1 unmerged"), "{status}");

    let err = env.fails(&ws, "alice", &["commit", "-m", "too soon"]);
    assert!(
        err.contains("unmerged files") && err.contains("Source/a.cpp"),
        "{err}"
    );
    let err = env.fails(&ws, "alice", &["merge", "feature"]);
    assert!(err.contains("not concluded your merge"), "{err}");
    let err = env.fails(&ws, "alice", &["switch", "feature"]);
    assert!(err.contains("not concluded your merge"), "{err}");
    let err = env.fails(&ws, "alice", &["update"]);
    assert!(err.contains("not concluded your merge"), "{err}");
    let err = env.fails(&ws, "alice", &["commit", "Source/c.cpp", "-m", "x"]);
    assert!(
        err.contains("not concluded your merge") || err.contains("merge"),
        "{err}"
    );

    write(&ws, "Source/a.cpp", "1\nboth\n3\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("All conflicts are fixed"), "{status}");
    let out = env.ok(&ws, "alice", &["commit"]);
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "1\nboth\n3\n");
    assert_eq!(
        commit_messages(&env, &ws)[0],
        "Merge branch 'feature' into main"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    let other = clone(&env, "other");
    assert_eq!(read(&other, "Source/a.cpp"), "1\nboth\n3\n");
    assert_eq!(read(&other, "Source/c.cpp"), "c\n");
}

#[test]
fn aborting_puts_the_files_back_and_forgets_the_merge() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "1\n2\n3\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "1\ntheirs\n3\n");
    write(&ws, "Source/c.cpp", "c\n");
    env.ok(&ws, "alice", &["commit", "Source/c.cpp", "-m", "add c"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/a.cpp", "1\nours\n3\n");

    let (code, _, _) = run(&env, &ws, &["merge", "feature"]);
    assert_eq!(code, Some(1));
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["merge", "--abort"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "1\nours\n3\n");
    assert!(!ws.join("Source/c.cpp").exists());
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    let err = env.fails(&ws, "alice", &["merge", "--abort"]);
    assert!(err.contains("no merge to abort"), "{err}");
    let (code, _, _) = run(&env, &ws, &["merge", "feature"]);
    assert_eq!(code, Some(1), "a merge can start again");
}

#[test]
fn a_binary_conflict_is_the_whole_file_and_either_side_can_be_taken() {
    let env = start();
    let write_bin =
        |ws: &Path, bytes: &[u8]| std::fs::write(ws.join("Source/img.bin"), bytes).unwrap();
    env.seed("alice", "Source/img.bin", "base", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    write_bin(&ws, &[0, 1, 2, 9]);
    env.ok(&ws, "alice", &["commit", "Source/img.bin", "-m", "theirs"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    write_bin(&ws, &[0, 7, 2, 3]);
    env.ok(&ws, "alice", &["commit", "Source/img.bin", "-m", "ours"]);

    let (code, out, _) = run(&env, &ws, &["merge", "feature"]);
    assert_eq!(code, Some(1));
    assert!(out.contains("cannot merge binary files"), "{out}");
    assert_eq!(
        std::fs::read(ws.join("Source/img.bin")).unwrap(),
        [0, 7, 2, 3],
        "ours stays in the file"
    );
    let err = env.fails(&ws, "alice", &["commit"]);
    assert!(err.contains("unmerged"), "{err}");

    env.ok(
        &ws,
        "alice",
        &["checkout", "--theirs", "--", "Source/img.bin"],
    );
    assert_eq!(
        std::fs::read(ws.join("Source/img.bin")).unwrap(),
        [0, 1, 2, 9]
    );
    env.ok(&ws, "alice", &["add", "Source/img.bin"]);
    env.ok(&ws, "alice", &["commit"]);
    assert_eq!(
        std::fs::read(ws.join("Source/img.bin")).unwrap(),
        [0, 1, 2, 9]
    );
}

#[test]
fn a_merge_refuses_staged_changes_and_local_edits_to_files_it_touches() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a\n", None);
    env.seed("alice", "Source/b.cpp", "b\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature\n");
    env.ok(&ws, "alice", &["switch", "main"]);

    write(&ws, "Source/b.cpp", "edited\n");
    env.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    let err = env.fails(&ws, "alice", &["merge", "feature"]);
    assert!(err.contains("staged changes"), "{err}");
    env.ok(&ws, "alice", &["restore", "--staged", "Source/b.cpp"]);
    env.ok(&ws, "alice", &["merge", "feature"]);
    assert_eq!(
        read(&ws, "Source/b.cpp"),
        "edited\n",
        "an edit to an untouched file stays"
    );

    env.ok(&ws, "alice", &["switch", "-f", "-c", "again", "main"]);
    env.ok(&ws, "alice", &["switch", "feature"]);
    commit(&env, &ws, "Source/b.cpp", "from feature\n");
    env.ok(&ws, "alice", &["switch", "again"]);
    write(&ws, "Source/b.cpp", "local edit\n");
    let err = env.fails(&ws, "alice", &["merge", "feature"]);
    assert!(
        err.contains("would be overwritten by merge") && err.contains("Source/b.cpp"),
        "{err}"
    );
    assert_eq!(
        read(&ws, "Source/b.cpp"),
        "local edit\n",
        "nothing was changed"
    );
}

#[test]
fn no_commit_stops_a_clean_merge_before_the_commit() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a\n", None);
    env.seed("alice", "Source/b.cpp", "b\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature\n");
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/b.cpp", "main\n");
    let out = env.ok(&ws, "alice", &["merge", "--no-commit", "feature"]);
    assert!(out.contains("stopped before committing"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "feature\n");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("All conflicts are fixed") && status.contains("merged"),
        "{status}"
    );
    let out = env.ok(&ws, "alice", &["commit", "-m", "merged by hand"]);
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(commit_messages(&env, &ws)[0], "merged by hand");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn a_shared_file_that_became_exclusive_while_a_branch_edited_it_is_chosen_by_the_person() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "base\n", None);
    env.seed("alice", "Source/b.cpp", "b\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature edit\n");
    env.seed(
        "alice",
        ".pyn/pyn.toml",
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.cpp\"]\n",
        None,
    );

    let (code, out, _) = run(&env, &ws, &["merge", "main"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("CONFLICT (exclusive conversion)"), "{out}");
    assert!(out.contains("pyn lock --base 2 Source/a.cpp"), "{out}");
    assert_eq!(
        read(&ws, "Source/a.cpp"),
        "feature edit\n",
        "ours stays in the file"
    );

    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let err = env.fails(&ws, "alice", &["commit"]);
    assert!(
        err.contains("lock_required") && err.contains("pyn lock --base 2 Source/a.cpp"),
        "{err}"
    );

    env.ok(&ws, "alice", &["lock", "--base", "2", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["commit"]);
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "feature edit\n");
    let history = env.ok(&ws, "alice", &["log", "Source/a.cpp"]);
    assert!(history.contains("exclusive"), "{history}");
    let locks = env.ok(&ws, "alice", &["locks"]);
    assert!(
        !locks.contains("Source/a.cpp"),
        "the lock goes with the revision: {locks}"
    );
}

#[test]
fn a_policy_change_reports_the_stranded_branches_and_a_later_merge_tells_the_story() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "base\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature edit\n");

    let policy = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.cpp\"]\n";
    write(&ws, "proposed.toml", policy);
    let preview = env.ok(&ws, "alice", &["policy", "preview", "proposed.toml"]);
    assert!(
        preview.contains("would strand") && preview.contains("feature") && preview.contains("r2"),
        "{preview}"
    );

    let dir = env.dir("seed");
    let file = dir.join("upload");
    std::fs::write(&file, policy).unwrap();
    let out = env.ok(
        &dir,
        "alice",
        &[
            "commit",
            ".pyn/pyn.toml",
            "--file",
            file.to_str().unwrap(),
            "-m",
            "a.cpp is exclusive",
        ],
    );
    assert!(
        out.contains("can no longer merge")
            && out.contains("feature")
            && out.contains("Source/a.cpp"),
        "{out}"
    );
    let audit = env.ok(&ws, "alice", &["audit", "--action", "version_stranded"]);
    assert!(
        audit.contains("feature") && audit.contains("stranded"),
        "{audit}"
    );

    let (code, out, _) = run(&env, &ws, &["merge", "main"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("became exclusive at r3")
            && out.contains("version r2")
            && out.contains("stranded"),
        "{out}"
    );
}

#[test]
fn taking_the_newest_version_of_a_converted_file_needs_no_lock() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "base\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature edit\n");
    env.seed(
        "alice",
        ".pyn/pyn.toml",
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.cpp\"]\n",
        None,
    );
    let (code, _, _) = run(&env, &ws, &["merge", "main"]);
    assert_eq!(code, Some(1));
    env.ok(
        &ws,
        "alice",
        &["checkout", "--theirs", "--", "Source/a.cpp"],
    );
    assert_eq!(read(&ws, "Source/a.cpp"), "base\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(
        &ws,
        "alice",
        &["commit", "-m", "keep the converted version"],
    );
    assert_eq!(read(&ws, "Source/a.cpp"), "base\n");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn an_exclusive_file_takes_the_later_revision_and_stays_read_only_without_its_lock() {
    let env = start();
    env.seed("alice", "Content/m.umap", "v1", None);
    env.seed("alice", "Source/a.cpp", "a\n", None);
    let ws = clone(&env, "ws");
    assert!(
        std::fs::metadata(ws.join("Content/m.umap"))
            .unwrap()
            .permissions()
            .readonly()
    );
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit(&env, &ws, "Content/m.umap", "v2");
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/a.cpp", "main\n");

    let preview = env.ok(&ws, "alice", &["merge", "--preview", "feature"]);
    assert!(preview.contains("Content/m.umap"), "{preview}");
    let out = env.ok(&ws, "alice", &["merge", "feature"]);
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(read(&ws, "Content/m.umap"), "v2");
    assert!(
        std::fs::metadata(ws.join("Content/m.umap"))
            .unwrap()
            .permissions()
            .readonly()
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    let locks = env.ok(&ws, "alice", &["locks"]);
    assert!(!locks.contains("Content/m.umap"), "{locks}");
}

#[test]
fn exclusive_files_each_branch_changed_are_chosen_by_the_person_under_the_lock() {
    let env = start();
    env.seed("alice", "Content/m.umap", "one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit(&env, &ws, "Content/m.umap", "feature value");
    env.ok(&ws, "alice", &["switch", "main"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit(&env, &ws, "Content/m.umap", "main value");

    let (code, out, _) = run(&env, &ws, &["merge", "feature"]);
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("Merge conflict in Content/m.umap"), "{out}");
    assert!(out.contains("pyn lock --base 3 Content/m.umap"), "{out}");
    assert_eq!(read(&ws, "Content/m.umap"), "main value", "ours stays");

    env.ok(&ws, "alice", &["add", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["commit"]);
    assert!(err.contains("lock_required"), "{err}");
    env.ok(&ws, "alice", &["lock", "--base", "3", "Content/m.umap"]);
    let out = env.ok(&ws, "alice", &["commit"]);
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(read(&ws, "Content/m.umap"), "main value");
    assert!(env.ok(&ws, "alice", &["verify"]).is_empty());
}

#[test]
fn a_merge_that_only_moves_an_exclusive_path_to_a_later_revision_leaves_a_current_base() {
    let env = start();
    env.seed("alice", "Content/m.umap", "one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit(&env, &ws, "Content/m.umap", "same");
    env.ok(&ws, "alice", &["switch", "feature"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_msg(&env, &ws, "Content/m.umap", "same", "feature same");
    env.ok(&ws, "alice", &["switch", "main"]);

    env.ok(&ws, "alice", &["merge", "--no-ff", "feature"]);
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit(&env, &ws, "Content/m.umap", "after");
    assert!(env.ok(&ws, "alice", &["verify"]).is_empty());
}

#[test]
fn a_merge_that_only_moves_a_shared_path_to_a_later_revision_commits_on_a_current_base() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one\n", None);
    env.seed("alice", "Source/b.cpp", "b\n", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    commit(&env, &ws, "Source/a.cpp", "same\n");
    env.ok(&ws, "alice", &["switch", "feature"]);
    commit_msg(&env, &ws, "Source/a.cpp", "same\n", "feature same");
    commit(&env, &ws, "Source/b.cpp", "feature\n");
    env.ok(&ws, "alice", &["switch", "main"]);

    env.ok(&ws, "alice", &["merge", "--no-ff", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "after\n");
    assert!(env.ok(&ws, "alice", &["verify"]).is_empty());
}

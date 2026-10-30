//! `pyn commit` without a message (the editor), `-a` with deletions and `-u` for untracked files, on a pyn server home
//! (with and without the local store) and on a git home.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Env, REPO, start};
use tempfile::{TempDir, tempdir};

fn clone_with(env: &Env, extra: &[&str]) -> PathBuf {
    let ws = env.dir("ws");
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn seeded(extra: &[&str]) -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "bee", None);
    env.seed("alice", "Content/m.umap", "map", None);
    let ws = clone_with(&env, extra);
    (env, ws)
}

fn store_git(ws: &Path, args: &[&str]) -> String {
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

fn tree(ws: &Path) -> Vec<String> {
    store_git(ws, &["ls-tree", "-r", "--name-only", "main"])
        .lines()
        .map(str::to_string)
        .collect()
}

/// An editor script that keeps what it was given in `seen.txt` and writes `reply` over the file.
fn editor(dir: &Path, reply: &str) -> String {
    let script = dir.join("editor.sh");
    let seen = dir.join("seen.txt");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\ncp \"$1\" '{}'\nprintf '%s' '{}' > \"$1\"\n",
            seen.display(),
            reply
        ),
    )
    .unwrap();
    std::os::unix::fs::PermissionsExt::set_mode(
        &mut fs::metadata(&script).unwrap().permissions(),
        0o755,
    );
    fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    script.display().to_string()
}

fn with_editor(env: &Env, ws: &Path, editor: &str, args: &[&str]) -> std::process::Output {
    env.run_with_vars(
        ws,
        "alice",
        args,
        "UTC",
        &[
            ("GIT_EDITOR", editor),
            ("EDITOR", editor),
            ("VISUAL", editor),
        ],
    )
}

#[test]
fn no_message_opens_the_editor_and_strips_comment_lines() {
    let (env, ws) = seeded(&["--local-store"]);
    fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    fs::write(ws.join("new.txt"), "n").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp", "new.txt"]);
    let scripts = env.dir("scripts");
    let script = editor(&scripts, "subject\n\nbody\n# dropped\n");
    let out = with_editor(&env, &ws, &script, &["commit"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = fs::read_to_string(scripts.join("seen.txt")).unwrap();
    assert!(
        seen.starts_with("\n# Please enter the commit message"),
        "{seen}"
    );
    assert!(seen.contains("#\tmodified:   Source/a.cpp"), "{seen}");
    assert!(seen.contains("#\tnew file:   new.txt"), "{seen}");
    assert_eq!(
        store_git(&ws, &["log", "-1", "--format=%B", "main"]),
        "subject\n\nbody\n\n"
    );
    assert!(!ws.join(".pyn/local_only/COMMIT_EDITMSG").exists());
}

#[test]
fn an_empty_editor_message_aborts_and_keeps_the_staged_changes() {
    let (env, ws) = seeded(&["--local-store"]);
    fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let before = store_git(&ws, &["rev-parse", "main"]);
    let out = with_editor(&env, &ws, ":", &["commit"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Aborting commit due to empty commit message."),
        "{err}"
    );
    assert_eq!(store_git(&ws, &["rev-parse", "main"]), before);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("a.cpp"), "{status}");
}

#[test]
fn nothing_staged_does_not_open_the_editor() {
    let (env, ws) = seeded(&["--local-store"]);
    let scripts = env.dir("scripts");
    let script = editor(&scripts, "x\n");
    let out = with_editor(&env, &ws, &script, &["commit"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!scripts.join("seen.txt").exists());
}

#[test]
fn dash_m_is_taken_as_given_and_never_opens_the_editor() {
    let (env, ws) = seeded(&["--local-store"]);
    fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let scripts = env.dir("scripts");
    let script = editor(&scripts, "not used\n");
    let out = with_editor(
        &env,
        &ws,
        &script,
        &["commit", "-m", "# heading", "-m", "para"],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!scripts.join("seen.txt").exists());
    assert_eq!(
        store_git(&ws, &["log", "-1", "--format=%B", "main"]),
        "# heading\n\npara\n\n"
    );
}

#[test]
fn path_commits_still_need_a_message() {
    let (env, ws) = seeded(&["--local-store"]);
    let err = env.fails(&ws, "alice", &["commit", "Source/a.cpp"]);
    assert!(err.contains("--message"), "{err}");
}

#[test]
fn all_with_a_message_stages_modifications_and_deletions() {
    for extra in [&["--local-store"][..], &["--no-local-store"]] {
        let env = start();
        env.seed("alice", "Source/a.cpp", "one", None);
        env.seed("alice", "Source/b.cpp", "bee", None);
        let ws = clone_with(&env, extra);
        fs::write(ws.join("Source/a.cpp"), "two").unwrap();
        fs::remove_file(ws.join("Source/b.cpp")).unwrap();
        fs::write(ws.join("new.txt"), "n").unwrap();
        env.ok(&ws, "alice", &["commit", "-am", "tracked only"]);
        if extra[0] == "--local-store" {
            assert_eq!(tree(&ws), ["Source/a.cpp"]);
            assert_eq!(store_git(&ws, &["show", "main:Source/a.cpp"]), "two");
        } else {
            let files = env.ok(&ws, "alice", &["ls"]);
            assert!(!files.contains("b.cpp"), "{files}");
        }
        let status = env.ok(&ws, "alice", &["status"]);
        assert!(status.contains("new.txt"), "{status}");
    }
}

#[test]
fn untracked_with_all_stages_new_modified_and_deleted_but_not_ignored() {
    for extra in [&["--local-store"][..], &["--no-local-store"]] {
        let env = start();
        env.seed("alice", "Source/a.cpp", "one", None);
        env.seed("alice", "Source/b.cpp", "bee", None);
        let ws = clone_with(&env, extra);
        fs::write(ws.join("Source/a.cpp"), "two").unwrap();
        fs::remove_file(ws.join("Source/b.cpp")).unwrap();
        fs::write(ws.join("new.txt"), "n").unwrap();
        fs::create_dir_all(ws.join("sub")).unwrap();
        fs::write(ws.join("sub/deep.txt"), "d").unwrap();
        fs::write(ws.join("skip.log"), "l").unwrap();
        fs::write(ws.join(".pyn/ignore"), "*.log\n").unwrap();
        env.ok(&ws, "alice", &["commit", "-uam", "everything"]);
        if extra[0] == "--local-store" {
            assert_eq!(
                tree(&ws),
                [".pyn/ignore", "Source/a.cpp", "new.txt", "sub/deep.txt"]
            );
        } else {
            let top = env.ok(&ws, "alice", &["ls"]);
            assert!(top.contains("new.txt") && top.contains("sub/"), "{top}");
            assert!(!top.contains("skip.log"), "{top}");
            let source = env.ok(&ws, "alice", &["ls", "Source"]);
            assert!(!source.contains("b.cpp"), "{source}");
        }
    }
}

#[test]
fn untracked_alone_leaves_tracked_changes_unstaged_and_flag_order_is_free() {
    let (env, ws) = seeded(&["--local-store"]);
    fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    fs::write(ws.join("new.txt"), "n").unwrap();
    env.ok(&ws, "alice", &["commit", "-m", "just new", "-u"]);
    assert_eq!(
        tree(&ws),
        ["Content/m.umap", "Source/a.cpp", "Source/b.cpp", "new.txt"]
    );
    assert_eq!(store_git(&ws, &["show", "main:Source/a.cpp"]), "one");
    fs::write(ws.join("more.txt"), "m").unwrap();
    env.ok(
        &ws,
        "alice",
        &["commit", "--untracked", "--all", "--message", "rest"],
    );
    assert_eq!(store_git(&ws, &["show", "main:Source/a.cpp"]), "two");
    assert!(tree(&ws).contains(&"more.txt".to_string()));
}

#[test]
fn untracked_and_all_still_need_the_lock_of_an_exclusive_path() {
    let (env, ws) = seeded(&["--local-store"]);
    std::fs::set_permissions(
        ws.join("Content/m.umap"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    fs::write(ws.join("Content/m.umap"), "edited").unwrap();
    fs::write(ws.join("new.txt"), "n").unwrap();
    env.ok(&ws, "bob", &["lock", "Content/m.umap"]);
    let before = store_git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["commit", "-uam", "x"]);
    assert!(err.contains("locked by bob"), "{err}");
    assert_eq!(store_git(&ws, &["rev-parse", "main"]), before);
}

#[test]
fn paths_cannot_be_combined_with_the_staging_flags() {
    let (env, ws) = seeded(&["--local-store"]);
    let err = env.fails(&ws, "alice", &["commit", "-u", "-m", "x", "Source/a.cpp"]);
    assert!(err.contains("name no paths"), "{err}");
}

const EMPTY_MESSAGES: &[&[&str]] = &[
    &["-m", ""],
    &["-m", "   "],
    &["-m", "", "-m", ""],
    &["-m", "# only a comment"],
    &["-F", "empty.txt"],
    &["-F", "comments.txt"],
];

#[test]
fn an_empty_message_is_refused_on_a_pyn_server() {
    for extra in [&["--local-store"][..], &["--no-local-store"]] {
        let (env, ws) = seeded(extra);
        fs::write(ws.join("empty.txt"), "").unwrap();
        fs::write(ws.join("comments.txt"), "# a\n  \n# b\n").unwrap();
        fs::write(ws.join("Source/a.cpp"), "two").unwrap();
        env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
        let before =
            (extra[0] == "--local-store").then(|| store_git(&ws, &["rev-list", "--count", "main"]));
        for args in EMPTY_MESSAGES {
            let mut full = vec!["commit"];
            full.extend(*args);
            let out = env.run(&ws, "alice", &full);
            assert_eq!(out.status.code(), Some(1), "{args:?}");
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                err.contains("Aborting commit due to empty commit message."),
                "{args:?}: {err}"
            );
        }
        let status = env.ok(&ws, "alice", &["status"]);
        assert!(status.contains("a.cpp"), "{status}");
        if extra[0] == "--local-store" {
            assert_eq!(
                Some(store_git(&ws, &["rev-list", "--count", "main"])),
                before
            );
        }
    }
}

struct Rig {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = T\n\temail = t@example.com\n",
        )
        .unwrap();
        let work = root.join("work");
        fs::create_dir_all(&work).unwrap();
        let rig = Rig {
            _tmp: tmp,
            work,
            home,
        };
        rig.ok(&["init-git"]);
        rig
    }

    fn pyn(&self, args: &[&str], vars: &[(&str, &str)]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.work)
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(vars.iter().copied())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.pyn(args, &[]);
        assert!(
            out.status.success(),
            "pyn {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn files(&self) -> Vec<String> {
        let out = Command::new("git")
            .current_dir(&self.work)
            .args(["ls-tree", "-r", "--name-only", "HEAD"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn seed(&self) {
        fs::write(self.work.join("a.txt"), "one\n").unwrap();
        fs::write(self.work.join("b.txt"), "bee\n").unwrap();
        self.ok(&["add", "."]);
        self.ok(&["commit", "-m", "base"]);
    }
}

#[test]
fn git_home_all_with_a_message_commits_deletions() {
    let rig = Rig::new();
    rig.seed();
    fs::write(rig.work.join("a.txt"), "two\n").unwrap();
    fs::remove_file(rig.work.join("b.txt")).unwrap();
    fs::write(rig.work.join("new.txt"), "n\n").unwrap();
    rig.ok(&["commit", "-am", "tracked"]);
    assert_eq!(rig.files(), ["a.txt"]);
}

#[test]
fn git_home_untracked_stages_new_files_and_honours_gitignore() {
    let rig = Rig::new();
    rig.seed();
    fs::write(rig.work.join(".gitignore"), "*.log\n").unwrap();
    rig.ok(&["add", ".gitignore"]);
    rig.ok(&["commit", "-m", "ignore"]);
    fs::write(rig.work.join("a.txt"), "two\n").unwrap();
    fs::remove_file(rig.work.join("b.txt")).unwrap();
    fs::create_dir_all(rig.work.join("sub")).unwrap();
    fs::write(rig.work.join("sub/deep.txt"), "d\n").unwrap();
    fs::write(rig.work.join("skip.log"), "l\n").unwrap();
    rig.ok(&["commit", "-u", "-m", "only new"]);
    assert_eq!(
        rig.files(),
        [".gitignore", "a.txt", "b.txt", "sub/deep.txt"]
    );
    fs::write(rig.work.join("more.txt"), "m\n").unwrap();
    rig.ok(&["commit", "-uam", "all of it"]);
    assert_eq!(
        rig.files(),
        [".gitignore", "a.txt", "more.txt", "sub/deep.txt"]
    );
}

#[test]
fn an_empty_message_is_refused_on_a_git_home_unless_allowed() {
    let rig = Rig::new();
    rig.seed();
    fs::write(rig.work.join("empty.txt"), "").unwrap();
    fs::write(rig.work.join("comments.txt"), "# a\n\n").unwrap();
    for args in EMPTY_MESSAGES {
        fs::write(rig.work.join("a.txt"), "two\n").unwrap();
        rig.ok(&["add", "a.txt"]);
        let mut full = vec!["commit"];
        full.extend(*args);
        let out = rig.pyn(&full, &[]);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("Aborting commit due to empty commit message."),
            "{args:?}: {err}"
        );
    }
    let count = |rig: &Rig| rig.ok(&["log"]).matches("commit ").count();
    assert_eq!(count(&rig), 1);
    rig.ok(&["commit", "--allow-empty-message", "-m", ""]);
    assert_eq!(count(&rig), 2);
}

impl Rig {
    fn editor(&self, reply: &str) -> (String, PathBuf) {
        let seen = self.work.join("../seen.txt");
        let script = self.work.join("../editor.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncp \"$1\" '{}'\nprintf '%s' '{}' > \"$1\"\n",
                seen.display(),
                reply
            ),
        )
        .unwrap();
        fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        (script.display().to_string(), seen)
    }

    fn with_editor(&self, editor: &str, args: &[&str]) -> std::process::Output {
        self.pyn(
            args,
            &[
                ("GIT_EDITOR", editor),
                ("EDITOR", editor),
                ("VISUAL", editor),
            ],
        )
    }

    fn message(&self) -> String {
        let out = Command::new("git")
            .current_dir(&self.work)
            .args(["log", "-1", "--format=%B"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.home)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

#[test]
fn git_home_no_message_opens_the_editor_like_git() {
    let rig = Rig::new();
    rig.seed();
    fs::write(rig.work.join("a.txt"), "two\n").unwrap();
    fs::remove_file(rig.work.join("b.txt")).unwrap();
    fs::write(rig.work.join("new.txt"), "n\n").unwrap();
    let (editor, seen) = rig.editor("subject\n\nbody\n# dropped\n");
    let out = rig.with_editor(&editor, &["commit", "-uas"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = fs::read_to_string(seen).unwrap();
    assert!(
        seen.starts_with(
            "\n\nSigned-off-by: T <t@example.com>\n\n# Please enter the commit message"
        ),
        "{seen}"
    );
    assert!(seen.contains("# On branch main"), "{seen}");
    assert!(seen.contains("#\tmodified:   a.txt"), "{seen}");
    assert!(seen.contains("#\tdeleted:    b.txt"), "{seen}");
    assert!(seen.contains("#\tnew file:   new.txt"), "{seen}");
    assert_eq!(rig.message(), "subject\n\nbody\n\n");
}

#[test]
fn git_home_empty_editor_message_aborts_and_nothing_staged_skips_the_editor() {
    let rig = Rig::new();
    rig.seed();
    let (editor, seen) = rig.editor("# only\n");
    let out = rig.with_editor(&editor, &["commit"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!seen.exists());
    fs::write(rig.work.join("a.txt"), "two\n").unwrap();
    let out = rig.with_editor(&editor, &["commit", "-a"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("Aborting commit due to empty commit message."),
        "{err}"
    );
    assert_eq!(rig.message(), "base\n\n");
}

#[test]
fn git_home_hooks_see_the_template_and_the_edited_message() {
    let rig = Rig::new();
    rig.seed();
    let hooks = rig.work.join(".git/hooks");
    let log = rig.work.join("../hooks.log");
    for name in ["prepare-commit-msg", "commit-msg"] {
        let path = hooks.join(name);
        fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"== $0\" >> '{0}'\ncat \"$1\" >> '{0}'\n{1}",
                log.display(),
                if name == "commit-msg" {
                    "echo hooked >> \"$1\"\n"
                } else {
                    ""
                }
            ),
        )
        .unwrap();
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    fs::write(rig.work.join("a.txt"), "two\n").unwrap();
    let (editor, _) = rig.editor("edited\n");
    let out = rig.with_editor(&editor, &["commit", "-a"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = fs::read_to_string(log).unwrap();
    let (prepare, commit_msg) = seen.split_once("== .git/hooks/commit-msg").unwrap();
    assert!(
        prepare.contains("# Please enter the commit message"),
        "{seen}"
    );
    assert!(
        commit_msg.contains("edited") && !commit_msg.contains("# Please"),
        "{seen}"
    );
    assert_eq!(rig.message(), "edited\nhooked\n\n");
}

/// The commit message and tree that pyn and real git make from the same state and editor.
#[test]
fn git_home_editor_commit_matches_real_git() {
    let git_commit = |args: &[&str], reply: &str| -> (String, String) {
        let rig = Rig::new();
        rig.seed();
        fs::write(rig.work.join("a.txt"), "two\n").unwrap();
        fs::write(rig.work.join("new.txt"), "n\n").unwrap();
        let (editor, _) = rig.editor(reply);
        let out = Command::new("git")
            .current_dir(&rig.work)
            .args(["commit"])
            .args(args)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &rig.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", &editor)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tree = Command::new("git")
            .current_dir(&rig.work)
            .args(["log", "-1", "--format=%T"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &rig.home)
            .output()
            .unwrap();
        (
            rig.message(),
            String::from_utf8_lossy(&tree.stdout).into_owned(),
        )
    };
    let pyn_commit = |args: &[&str], reply: &str| -> (String, String) {
        let rig = Rig::new();
        rig.seed();
        fs::write(rig.work.join("a.txt"), "two\n").unwrap();
        fs::write(rig.work.join("new.txt"), "n\n").unwrap();
        let (editor, _) = rig.editor(reply);
        let mut full = vec!["commit"];
        full.extend(args);
        let out = rig.with_editor(&editor, &full);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let tree = Command::new("git")
            .current_dir(&rig.work)
            .args(["log", "-1", "--format=%T"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &rig.home)
            .output()
            .unwrap();
        (
            rig.message(),
            String::from_utf8_lossy(&tree.stdout).into_owned(),
        )
    };
    for reply in ["subject\n\nbody\n# gone\n", "\n\n  spaced   \n\n\nx\n"] {
        let (git_message, git_tree) = git_commit(&["-a"], reply);
        let (pyn_message, pyn_tree) = pyn_commit(&["-a"], reply);
        assert_eq!(pyn_message, git_message, "{reply:?}");
        assert_eq!(pyn_tree, git_tree, "{reply:?}");
    }
}

//! Refusals that must change nothing, what pyn will not write (converted line endings, sparse checkouts), messages,
//! settings (`core.filemode`, `core.symlinks`, `core.logAllRefUpdates`) and histories the merge cannot handle.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

use common::gitfix::*;
use pyn_git::Repository;
use pyn_object::{Algorithm, Commit, Kind, ObjectId, Signature};

fn config_append(repo: &Repo, text: &str) {
    let path = repo.main.join(".git/config");
    let old = fs::read_to_string(&path).unwrap();
    fs::write(path, format!("{old}{text}")).unwrap();
}

fn head_id(repo: &Repo) -> String {
    fs::read_to_string(repo.main.join(".git/refs/heads/main"))
        .unwrap()
        .trim()
        .to_string()
}

fn write(repo: &Repo, path: &str, text: &str) {
    let file = repo.main.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

/// Everything that changed under `.git` and the work tree between two snapshots.
fn changed(before: &std::collections::BTreeMap<String, String>, repo: &Repo) -> Vec<String> {
    let after = snapshot(&repo.main);
    let mut keys: Vec<String> = after
        .keys()
        .chain(before.keys())
        .filter(|k| after.get(*k) != before.get(*k))
        .cloned()
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

struct Refusal {
    fixture: &'static str,
    prelude: &'static [&'static [&'static str]],
    command: &'static [&'static str],
    /// What git itself writes while refusing.
    allowed: &'static [&'static str],
}

const REFUSALS: &[Refusal] = &[
    Refusal {
        fixture: "main",
        prelude: &[&["branch", "old", "HEAD~3"]],
        command: &["switch", "old"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[&["branch", "old", "HEAD~3"]],
        command: &["checkout", "old"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["switch", "feature"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["switch", "nope"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["switch", "HEAD~1"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["switch", "-c", "feature"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["switch", "-c", "bad..name"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["switch", "-"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["checkout", "-b", "feature"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["checkout", "--", "nope.txt"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["restore", "nope.txt"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["restore", "--source", "nope", "README.md"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["branch", "feature"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["branch", "-d", "main"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["branch", "-D", "feature"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["branch", "-m", "feature"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["branch", "-m", "nope", "other"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["branch", "x", "nope"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["tag", "light"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["tag", "-a", "t"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["tag", "-d", "nope"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["stash", "pop"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["stash", "drop"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["stash", "apply", "stash@{0}"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["stash", "push", "--", "nope.txt"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["stash", "branch", "topic"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["stash", "show"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["stash", "store", "nope"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["merge", "--abort"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["merge", "--continue"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["merge", "nope"],
        allowed: &[],
    },
    Refusal {
        fixture: "main",
        prelude: &[],
        command: &["merge", "--preview", "feature"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[],
        command: &["merge", "--ff-only", "clean"],
        allowed: &[".git/ORIG_HEAD"],
    },
    Refusal {
        fixture: "merge",
        prelude: &[&["merge", "conflict"]],
        command: &["merge", "clean"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[&["merge", "conflict"]],
        command: &["switch", "clean"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[&["merge", "conflict"]],
        command: &["checkout", "clean"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[&["merge", "conflict"]],
        command: &["stash"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[&["merge", "conflict"]],
        command: &["commit", "-m", "x"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[&["merge", "conflict"]],
        command: &["restore", "a.txt"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[&["merge", "conflict"]],
        command: &["checkout", "--", "a.txt"],
        allowed: &[],
    },
    Refusal {
        fixture: "merge",
        prelude: &[],
        command: &["merge", "moddel", "--no-ff", "--ff-only"],
        allowed: &[".git/ORIG_HEAD"],
    },
];

#[test]
fn a_refused_command_changes_neither_the_git_directory_nor_the_work_tree() {
    for format in FORMATS {
        for r in REFUSALS {
            let repo = if r.fixture == "merge" {
                Repo::merge(format)
            } else {
                Repo::new(format)
            };
            for step in r.prelude {
                repo.run_in(&repo.main, step);
            }
            let before = snapshot(&repo.main);
            let out = repo.run_in(&repo.main, r.command);
            assert!(
                !out.status.success(),
                "{format}: pyn {:?} should have been refused: {}",
                r.command,
                String::from_utf8_lossy(&out.stdout)
            );
            let moved = changed(&before, &repo);
            let unexpected: Vec<&String> = moved
                .iter()
                .filter(|k| !r.allowed.iter().any(|a| k.starts_with(a)))
                .collect();
            assert!(
                unexpected.is_empty(),
                "{format}: pyn {:?} changed {unexpected:?}\n{}",
                r.command,
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[test]
fn a_local_change_that_a_switch_would_overwrite_is_listed_and_kept() {
    let repo = Repo::new("sha1");
    repo.ok(&["branch", "old", "HEAD~3"]);
    let out = repo.run_in(&repo.main, &["switch", "old"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(
            "error: Your local changes to the following files would be overwritten by checkout:"
        ) && err.contains("\tREADME.md\n")
            && err.contains("Please commit your changes or stash them before you switch branches.")
            && err.contains("Aborting"),
        "{err}"
    );
    assert_eq!(
        fs::read_to_string(repo.main.join("README.md")).unwrap(),
        "hello\nworld\nmore\n"
    );
}

#[test]
fn untracked_files_in_the_way_are_listed() {
    let repo = Repo::merge("sha1");
    write(&repo, "clean-only.txt", "mine\n");
    let out = repo.run_in(&repo.main, &["switch", "clean"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("The following untracked working tree files would be overwritten by checkout:\n\tclean-only.txt\n"),
        "{err}"
    );
    repo.ok(&["switch", "-f", "clean"]);
    assert_eq!(
        fs::read_to_string(repo.main.join("clean-only.txt")).unwrap(),
        "clean\n"
    );
}

#[test]
fn an_ignored_file_in_the_way_is_overwritten_as_git_does() {
    let repo = Repo::merge("sha1");
    write(&repo, ".git/info/exclude", "clean-only.txt\n");
    write(&repo, "clean-only.txt", "ignored\n");
    repo.ok(&["switch", "clean"]);
    assert_eq!(
        fs::read_to_string(repo.main.join("clean-only.txt")).unwrap(),
        "clean\n"
    );
}

#[test]
fn a_folder_with_untracked_files_is_never_replaced_by_a_file() {
    let repo = Repo::merge("sha1");
    repo.ok(&["switch", "-c", "p"]);
    fs::remove_dir_all(repo.main.join("dir")).unwrap();
    write(&repo, "dir", "now a file\n");
    repo.ok(&["add", "-A"]);
    repo.ok(&["commit", "-m", "folder to file"]);
    repo.ok(&["switch", "main"]);
    write(&repo, "dir/mine.txt", "mine\n");
    let out = repo.run_in(&repo.main, &["switch", "p"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(repo.main.join("dir/mine.txt").exists());
}

#[test]
fn files_become_folders_and_back_when_switching() {
    let repo = Repo::merge("sha1");
    repo.ok(&["switch", "-c", "p"]);
    fs::remove_dir_all(repo.main.join("dir")).unwrap();
    write(&repo, "dir", "now a file\n");
    repo.ok(&["add", "-A"]);
    repo.ok(&["commit", "-m", "folder to file"]);
    repo.ok(&["switch", "main"]);
    assert!(repo.main.join("dir/x.txt").is_file());
    repo.ok(&["switch", "p"]);
    assert!(repo.main.join("dir").is_file());
    repo.ok(&["switch", "main"]);
    assert!(repo.main.join("dir/x.txt").is_file());
}

#[test]
fn the_executable_bit_and_symlinks_are_written_and_core_filemode_is_honored() {
    for format in FORMATS {
        let repo = Repo::merge(format);
        repo.ok(&["switch", "clean"]);
        let mode = |p: &str| {
            fs::metadata(repo.main.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o111
        };
        assert_ne!(mode("b.txt"), 0, "the clean branch made b.txt executable");
        assert_eq!(
            fs::read_link(repo.main.join("link")).unwrap().to_str(),
            Some("b.txt")
        );
        assert_ne!(mode("run.sh"), 0);
        repo.ok(&["switch", "main"]);
        assert_eq!(mode("b.txt"), 0);
        assert_eq!(
            fs::read_link(repo.main.join("link")).unwrap().to_str(),
            Some("a.txt")
        );
    }
    let repo = Repo::merge("sha1");
    config_append(&repo, "[core]\n\tsymlinks = false\n");
    repo.ok(&["restore", "--source=old", "link"]);
    let meta = fs::symlink_metadata(repo.main.join("link")).unwrap();
    assert!(
        meta.is_file(),
        "core.symlinks=false writes the link as a file"
    );
    assert_eq!(fs::read_to_string(repo.main.join("link")).unwrap(), "a.txt");

    let repo = Repo::merge("sha1");
    config_append(&repo, "[core]\n\tfilemode = false\n");
    fs::set_permissions(repo.main.join("a.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    let status = repo.ok(&["status"]);
    assert!(
        status.contains("nothing to commit"),
        "a mode change is not a change: {status}"
    );
    repo.ok(&["switch", "clean"]);
    assert_eq!(
        fs::read_to_string(repo.main.join("a.txt"))
            .unwrap()
            .lines()
            .nth(8),
        Some("line nine clean")
    );
}

#[test]
fn filters_and_encodings_stop_every_write_before_anything_changes() {
    for attributes in [
        "a.txt filter=custom\n",
        "a.txt working-tree-encoding=UTF-16\n",
    ] {
        let repo = Repo::merge("sha1");
        write(&repo, ".gitattributes", attributes);
        let before = snapshot(&repo.main);
        let out = repo.run_in(&repo.main, &["merge", "clean"]);
        assert_eq!(
            out.status.code(),
            Some(128),
            "{attributes}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            changed(&before, &repo),
            Vec::<String>::new(),
            "{attributes}"
        );
        let out = repo.run_in(&repo.main, &["restore", "--source=old", "a.txt"]);
        assert_eq!(out.status.code(), Some(128), "{attributes}");
    }
    let repo = Repo::merge("sha1");
    write(
        &repo,
        ".gitattributes",
        "*.txt eol=lf\nclean-only.txt -text\n*.bin binary\n",
    );
    repo.ok(&["switch", "clean"]);
    let repo = Repo::merge("sha1");
    config_append(&repo, "[core]\n\tautocrlf = true\n");
    write(&repo, ".gitattributes", "*.txt -text\n");
    repo.ok(&["switch", "clean"]);
}

#[test]
fn a_git_lfs_filter_is_written_as_a_pointer_with_a_warning() {
    let repo = Repo::merge("sha1");
    write(
        &repo,
        ".gitattributes",
        "*.txt filter=lfs diff=lfs merge=lfs -text\n",
    );
    let out = repo.run_in(&repo.main, &["switch", "clean"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("Git LFS"));
}

#[test]
fn a_sparse_checkout_is_refused() {
    let repo = Repo::merge("sha1");
    let path = repo.main.join(".git/index");
    let mut index = pyn_git::Index::read(&path, Algorithm::Sha1).unwrap();
    index.entries[0].skip_worktree = true;
    index.write(&path, Algorithm::Sha1).unwrap();
    let before = snapshot(&repo.main);
    let (code, err) = repo.fails(&[
        "restore",
        "--ignore-skip-worktree-bits",
        "--source",
        "HEAD",
        ".",
    ]);
    assert_eq!(code, 128);
    assert!(err.contains("sparse"), "{err}");
    assert_eq!(snapshot(&repo.main), before);
}

#[test]
fn reflogs_follow_core_logallrefupdates() {
    let repo = Repo::merge("sha1");
    config_append(&repo, "[core]\n\tlogallrefupdates = false\n");
    repo.ok(&["switch", "-c", "x"]);
    repo.ok(&["tag", "t"]);
    assert!(
        !repo.main.join(".git/logs").exists(),
        "false creates no reflog"
    );

    let repo = Repo::merge("sha1");
    config_append(&repo, "[core]\n\tlogallrefupdates = always\n");
    repo.ok(&["tag", "-a", "-m", "m", "t"]);
    repo.ok(&["switch", "-c", "x"]);
    assert!(repo.main.join(".git/logs/refs/heads/x").is_file());

    let repo = Repo::merge("sha1");
    repo.ok(&["tag", "t"]);
    assert!(
        !repo.main.join(".git/logs/refs/tags/t").exists(),
        "tags have no reflog by default"
    );
    repo.ok(&["switch", "-c", "x"]);
    repo.ok(&["switch", "main"]);
    let head = fs::read_to_string(repo.main.join(".git/logs/HEAD")).unwrap();
    assert!(
        head.contains("checkout: moving from main to x")
            && head.contains("checkout: moving from x to main")
    );
    repo.ok(&["branch", "-D", "x"]);
    assert!(
        !repo.main.join(".git/logs/refs/heads/x").exists(),
        "deleting a branch deletes its reflog"
    );
}

#[test]
fn renaming_and_deleting_a_branch_moves_and_drops_its_configuration() {
    let repo = Repo::merge("sha1");
    config_append(
        &repo,
        "[branch \"clean\"]\n\tremote = origin\n\tmerge = refs/heads/clean\n[branch \"conflict\"]\n\tremote = origin\n",
    );
    repo.ok(&["branch", "-m", "clean", "tidy"]);
    let config = fs::read_to_string(repo.main.join(".git/config")).unwrap();
    assert!(
        config.contains("[branch \"tidy\"]\n\tremote = origin\n\tmerge = refs/heads/clean\n"),
        "{config}"
    );
    assert!(!config.contains("[branch \"clean\"]"));
    repo.ok(&["branch", "-D", "conflict"]);
    let config = fs::read_to_string(repo.main.join(".git/config")).unwrap();
    assert!(
        !config.contains("conflict") && config.contains("tidy"),
        "{config}"
    );
}

#[test]
fn branch_messages_follow_git() {
    let repo = Repo::merge("sha1");
    let out = repo.ok(&["branch", "-D", "ff"]);
    assert!(out.starts_with("Deleted branch ff (was "), "{out}");
    repo.ok(&["branch", "keep", "old"]);
    let (code, err) = repo.fails(&["branch", "-d", "clean"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("the branch 'clean' is not fully merged"),
        "{err}"
    );
    let (code, err) = repo.fails(&["branch", "-d", "main"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("cannot delete branch 'main' used by worktree at"),
        "{err}"
    );
    let out = repo.run_in(&repo.main, &["switch", "-c", "next"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Switched to a new branch 'next'"));
    let out = repo.run_in(&repo.main, &["switch", "main"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Switched to branch 'main'"));
    let out = repo.run_in(&repo.main, &["switch", "main"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("Already on 'main'"));
    let out = repo.run_in(&repo.main, &["checkout", "--detach", "old"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !err.contains("You are in 'detached HEAD' state") && err.contains("HEAD is now at "),
        "{err}"
    );
    repo.ok(&["switch", "main"]);
    let out = repo.run_in(&repo.main, &["checkout", "old^0"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("You are in 'detached HEAD' state"), "{err}");
    assert_eq!(repo.ok(&["branch", "--show-current"]), "");
    assert!(repo.ok(&["branch"]).starts_with("* (HEAD detached at "));
    repo.ok(&["switch", "main"]);
    assert_eq!(repo.ok(&["branch", "--show-current"]), "main\n");
}

#[test]
fn local_changes_are_listed_after_a_switch_that_carries_them() {
    let repo = Repo::new("sha1");
    repo.ok(&["branch", "other"]);
    let out = repo.ok(&["switch", "other"]);
    assert!(
        out.lines().any(|l| l == "M\tREADME.md") && out.lines().any(|l| l == "A\tstaged.txt"),
        "{out}"
    );
}

fn orphan_commit(repo: &Repo) -> ObjectId {
    let git = Repository::open(&repo.main).unwrap();
    let who = Signature {
        name: b"Orphan".to_vec(),
        email: b"orphan@example.com".to_vec(),
        time: 1_700_000_000,
        offset_minutes: 0,
    };
    let db = git.objects().unwrap();
    let tree = db.write(Kind::Tree, b"").unwrap();
    let commit = Commit {
        tree,
        parents: vec![],
        author: who.clone(),
        committer: who,
        extra_headers: vec![],
        message: b"unrelated\n".to_vec(),
    };
    db.write(Kind::Commit, &commit.encode(git.algorithm()).unwrap())
        .unwrap()
}

#[test]
fn histories_without_a_common_start_are_refused() {
    for format in FORMATS {
        let repo = Repo::merge(format);
        let id = orphan_commit(&repo);
        repo.ok(&["branch", "unrelated", &id.to_hex()]);
        let before = snapshot(&repo.main);
        let (code, err) = repo.fails(&["merge", "unrelated"]);
        assert_eq!(code, 128);
        assert!(
            err.contains("refusing to merge unrelated histories"),
            "{err}"
        );
        let moved = changed(&before, &repo);
        assert!(moved.iter().all(|p| p == ".git/ORIG_HEAD"), "{moved:?}");
    }
}

#[test]
fn a_binary_file_changed_on_both_sides_is_a_conflict_that_keeps_our_version() {
    let repo = Repo::merge("sha1");
    repo.ok(&["switch", "-c", "p", "old"]);
    fs::write(repo.main.join("blob.bin"), b"base\0one").unwrap();
    repo.ok(&["add", "blob.bin"]);
    repo.ok(&["commit", "-m", "base bin"]);
    repo.ok(&["switch", "-c", "q"]);
    fs::write(repo.main.join("blob.bin"), b"base\0theirs").unwrap();
    repo.ok(&["commit", "-a", "-m", "theirs"]);
    repo.ok(&["switch", "p"]);
    fs::write(repo.main.join("blob.bin"), b"base\0ours").unwrap();
    repo.ok(&["commit", "-a", "-m", "ours"]);
    let out = repo.run_in(&repo.main, &["merge", "q"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("warning: Cannot merge binary files: blob.bin (HEAD vs. q)"),
        "{text}"
    );
    assert_eq!(fs::read(repo.main.join("blob.bin")).unwrap(), b"base\0ours");
    let listing = fs::read_to_string(repo.main.join(".git/MERGE_MSG")).unwrap();
    assert!(listing.contains("#\tblob.bin"));
    repo.ok(&["merge", "--abort"]);
    assert_eq!(fs::read(repo.main.join("blob.bin")).unwrap(), b"base\0ours");
}

#[test]
fn a_symlink_changed_on_both_sides_is_a_conflict() {
    let repo = Repo::merge("sha1");
    repo.ok(&["switch", "-c", "p", "old"]);
    fs::remove_file(repo.main.join("link")).unwrap();
    std::os::unix::fs::symlink("b.txt", repo.main.join("link")).unwrap();
    repo.ok(&["commit", "-a", "-m", "p link"]);
    repo.ok(&["switch", "-c", "q", "old"]);
    fs::remove_file(repo.main.join("link")).unwrap();
    std::os::unix::fs::symlink("del.txt", repo.main.join("link")).unwrap();
    repo.ok(&["commit", "-a", "-m", "q link"]);
    let out = repo.run_in(&repo.main, &["merge", "p"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("CONFLICT (content): Merge conflict in link")
    );
    assert_eq!(
        fs::read_link(repo.main.join("link")).unwrap().to_str(),
        Some("del.txt")
    );
}

#[test]
fn commit_after_a_merge_uses_the_merge_message_without_its_comment_lines() {
    let repo = Repo::merge("sha1");
    repo.run_in(&repo.main, &["merge", "conflict"]);
    write(&repo, "a.txt", "fine\n");
    repo.ok(&["add", "a.txt"]);
    repo.ok(&["commit"]);
    let git = Repository::open(&repo.main).unwrap();
    let head = git.refs().resolve("HEAD").unwrap().unwrap();
    let text = String::from_utf8(git.objects().unwrap().read(&head).unwrap().data).unwrap();
    assert!(text.ends_with("\n\nMerge branch 'conflict'\n"), "{text}");
    assert!(!repo.main.join(".git/MERGE_HEAD").exists());
    assert!(!repo.main.join(".git/MERGE_MSG").exists());
    let (code, err) = repo.fails(&["commit"]);
    assert_eq!(code, 129, "{err}");
}

#[test]
fn merge_continue_finishes_what_merge_started() {
    let repo = Repo::merge("sha1");
    repo.run_in(&repo.main, &["merge", "--no-commit", "clean"]);
    assert!(repo.main.join(".git/MERGE_HEAD").exists());
    repo.ok(&["merge", "--continue"]);
    assert!(!repo.main.join(".git/MERGE_HEAD").exists());
    let log = repo.ok(&["log", "-n", "1"]);
    assert!(
        log.contains("Merge branch 'clean'") && log.contains("Merge: "),
        "{log}"
    );
}

#[test]
fn the_merge_message_names_the_branch_and_where_it_goes() {
    let repo = Repo::merge("sha1");
    repo.ok(&["switch", "-c", "topic"]);
    repo.ok(&["merge", "clean"]);
    let log = repo.ok(&["log", "-n", "1"]);
    assert!(log.contains("Merge branch 'clean' into topic"), "{log}");
}

#[test]
fn stash_round_trips_on_a_sha256_repository() {
    let repo = Repo::new("sha256");
    let before = tree_dump(&repo.main);
    let out = repo.ok(&["stash", "-u"]);
    assert!(
        out.starts_with("Saved working directory and index state WIP on main: "),
        "{out}"
    );
    assert_ne!(tree_dump(&repo.main), before);
    assert!(
        repo.ok(&["stash", "list"])
            .starts_with("stash@{0}: WIP on main: ")
    );
    repo.ok(&["stash", "pop"]);
    assert_eq!(tree_dump(&repo.main), before);
    assert_eq!(repo.ok(&["stash", "list"]), "");
    assert!(!repo.main.join(".git/refs/stash").exists());
}

#[test]
fn stash_keeps_ignored_files_and_leaves_nothing_to_stash_alone() {
    let repo = Repo::new("sha1");
    repo.ok(&["stash", "-u"]);
    assert!(repo.main.join("debug.out").exists(), "ignored files stay");
    assert!(!repo.main.join("new.txt").exists());
    let out = repo.ok(&["stash"]);
    assert_eq!(out.trim(), "No local changes to save");
    let clean = Repo::merge("sha1");
    assert_eq!(clean.ok(&["stash"]).trim(), "No local changes to save");
}

#[test]
fn hooks_switched_off_are_not_run_and_one_warning_names_them() {
    let repo = Repo::merge("sha1");
    config_append(&repo, "[pyn]\n\thooks = false\n");
    let hooks = repo.main.join(".git/hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(hooks.join("post-checkout"), "#!/bin/sh\ntouch hook-ran\n").unwrap();
    fs::set_permissions(
        hooks.join("post-checkout"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let out = repo.run_in(&repo.main, &["switch", "clean"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        err.matches("pyn does not run git hooks").count(),
        1,
        "{err}"
    );
    assert!(!repo.main.join("hook-ran").exists());
}

#[test]
fn an_empty_repository_can_branch_and_switch_but_not_stash_or_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    fs::create_dir_all(root.join(".git/objects")).unwrap();
    fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/trunk\n").unwrap();
    fs::write(
        root.join(".git/config"),
        "[core]\n\trepositoryformatversion = 0\n",
    )
    .unwrap();
    let repo = Repo::new("sha1");
    let run = |args: &[&str]| repo.run_in(&root, args);
    assert!(run(&["branch"]).status.success());
    assert_eq!(
        String::from_utf8_lossy(&run(&["branch", "--show-current"]).stdout),
        "trunk\n"
    );
    assert_eq!(run(&["branch", "x"]).status.code(), Some(128));
    assert_eq!(run(&["stash"]).status.code(), Some(128));
    assert_eq!(run(&["tag", "t"]).status.code(), Some(128));
    let out = run(&["switch", "-c", "other"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(root.join(".git/HEAD")).unwrap(),
        "ref: refs/heads/other\n"
    );
    assert!(
        !root.join(".git/refs/heads/other").exists(),
        "an unborn branch has no ref yet"
    );
}

#[test]
fn a_detached_head_commits_nothing_and_merges_into_itself() {
    let repo = Repo::merge("sha1");
    repo.ok(&["switch", "--detach", "main"]);
    repo.ok(&["merge", "clean"]);
    let git = Repository::open(&repo.main).unwrap();
    assert!(
        fs::read_to_string(repo.main.join(".git/HEAD"))
            .unwrap()
            .len()
            > 40
    );
    let head = git.refs().resolve("HEAD").unwrap().unwrap();
    assert_ne!(head.to_hex(), head_id(&repo));
    let log = repo.ok(&["log", "-n", "1"]);
    assert!(log.contains("Merge branch 'clean' into HEAD"), "{log}");
}

#[test]
fn the_conflict_files_are_diff3_with_the_branch_names() {
    let repo = Repo::merge("sha1");
    repo.run_in(&repo.main, &["merge", "conflict"]);
    let text = fs::read_to_string(repo.main.join("a.txt")).unwrap();
    assert!(
        text.contains("<<<<<<< HEAD\nline two ours\n||||||| ")
            && text.contains("=======\nline two theirs\n>>>>>>> conflict\n"),
        "{text}"
    );
}

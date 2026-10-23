//! A repository that lives in git: `status`, `diff`, `log` and `show` compared with what git printed for the same
//! repositories (fixtures built once with the real git binary, see `fixtures/git/regenerate.sh`; the tests never run git),
//! the refusals, `init`, and that nothing git sees is ever changed.

mod common;

use std::fs;

use common::gitfix::*;

#[test]
fn status_reports_what_git_reports() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let out = repo.ok(&["status"]);
        assert!(out.starts_with("On branch main\n"), "{out}");
        assert_eq!(
            porcelain(&out),
            sorted_lines(&repo.expected("status.txt")),
            "{format}: {out}"
        );
        assert!(
            out.ends_with("2 staged, 4 modified, 6 untracked\n"),
            "{format}: {out}"
        );
    }
}

#[test]
fn status_in_a_linked_worktree_uses_its_own_head_and_index() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let out = repo.ok_in(&repo.linked, &["status"]);
        assert!(out.starts_with("On branch feature\n"), "{out}");
        assert_eq!(
            porcelain(&out),
            sorted_lines(&repo.expected("linked/status.txt")),
            "{format}: {out}"
        );
        assert_eq!(
            repo.ok_in(&repo.linked, &["diff"]),
            repo.expected("linked/diff.txt")
        );
        assert!(
            repo.ok_in(&repo.linked, &["log", "-n", "1"])
                .contains("feature work")
        );
    }
}

#[test]
fn status_of_a_clean_work_tree_says_so() {
    let repo = Repo::new("sha1");
    fs::write(repo.linked.join("feature.txt"), "feature\n").unwrap();
    fs::remove_file(repo.linked.join("wt-only.txt")).unwrap();
    let out = repo.ok_in(&repo.linked, &["status"]);
    assert_eq!(
        out,
        "On branch feature\nnothing to commit, working tree clean\n"
    );
}

#[test]
fn log_matches_git() {
    for format in FORMATS {
        let repo = Repo::new(format);
        assert_eq!(repo.ok(&["log"]), repo.expected("log.txt"), "{format}");
        assert_eq!(repo.ok(&["log", "-n", "2"]), repo.expected("log-n2.txt"));
        assert_eq!(
            repo.ok(&["log", "--max-count", "2"]),
            repo.expected("log-n2.txt")
        );
        assert_eq!(
            repo.ok(&["log", "README.md"]),
            repo.expected("log-readme.txt")
        );
        assert_eq!(
            repo.ok(&["log", "feature"]),
            repo.expected("log-feature.txt")
        );
        assert_eq!(
            repo.ok(&["history", "-n", "2"]),
            repo.expected("log-n2.txt")
        );
    }
}

#[test]
fn log_commits_lists_one_row_per_commit_newest_first() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let ids: Vec<String> = repo
            .expected("commits.txt")
            .lines()
            .map(str::to_string)
            .collect();
        let out = repo.ok(&["log", "--commits"]);
        let rows: Vec<Vec<&str>> = out
            .lines()
            .skip(1)
            .map(|l| l.split("  ").collect())
            .collect();
        assert_eq!(rows.len(), ids.len(), "{out}");
        for (row, id) in rows.iter().zip(&ids) {
            assert_eq!(row[0].trim(), &id[..10], "{out}");
        }
        assert!(out.lines().next().unwrap().starts_with("COMMIT"));
        assert!(out.contains("Merge branch 'feature'") && out.contains("initial commit"));
    }
}

#[test]
fn show_matches_git() {
    for format in FORMATS {
        let repo = Repo::new(format);
        for (args, file) in [
            (vec!["show"], "show-head.txt"),
            (vec!["show", "HEAD"], "show-head.txt"),
            (vec!["show", "HEAD~1"], "show-third.txt"),
            (vec!["show", "HEAD^2"], "show-feature.txt"),
            (vec!["show", "v1~2"], "show-tag-step.txt"),
            (vec!["show", "HEAD~3"], "show-initial.txt"),
            (vec!["show", "--stat", "HEAD~1"], "show-stat-third.txt"),
        ] {
            assert_eq!(repo.ok(&args), repo.expected(file), "{format} {args:?}");
        }
    }
}

#[test]
fn show_takes_ids_and_abbreviations() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let ids = repo.expected("commits.txt");
        let third = ids.lines().nth(1).unwrap();
        for name in [third, &third[..8], &third[..4]] {
            assert_eq!(repo.ok(&["show", name]), repo.expected("show-third.txt"));
        }
    }
}

#[test]
fn diff_matches_git() {
    for format in FORMATS {
        let repo = Repo::new(format);
        for (args, file) in [
            (vec!["diff"], "diff.txt"),
            (vec!["diff", "--stat"], "diff-stat.txt"),
            (vec!["diff", "--name-status"], "diff-name-status.txt"),
            (vec!["diff", "HEAD"], "diff-head.txt"),
            (vec!["diff", "HEAD~3", "HEAD~2"], "diff-commits.txt"),
            (
                vec!["diff", "HEAD~3..HEAD~2", "--", "src"],
                "diff-range-path.txt",
            ),
            (vec!["diff", "-U1", "HEAD~3", "HEAD~2"], "diff-u1.txt"),
            (
                vec!["diff", "--stat", "HEAD~2", "HEAD"],
                "diff-stat-commits.txt",
            ),
            (
                vec!["diff", "light", "v1", "--name-status"],
                "diff-commits-name-status.txt",
            ),
            (
                vec!["diff", "--name-only", "HEAD"],
                "diff-name-only-head.txt",
            ),
        ] {
            let got = repo.ok(&args);
            let want = repo.expected(file);
            assert_eq!(got, want, "{format} {args:?}");
        }
    }
}

#[test]
fn diff_exit_codes_follow_git() {
    let repo = Repo::new("sha1");
    let out = repo.run_in(&repo.main, &["diff", "--exit-code"]);
    assert_eq!(out.status.code(), Some(1));
    let out = repo.run_in(&repo.main, &["diff", "-q"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let out = repo.run_in(&repo.main, &["diff", "--exit-code", "HEAD", "HEAD"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
}

#[test]
fn paths_are_taken_from_where_you_stand() {
    let repo = Repo::new("sha1");
    let out = repo.ok_in(&repo.main.join("src"), &["diff", "HEAD", "--", "lib.rs"]);
    assert!(
        out.starts_with("diff --git a/src/lib.rs b/src/lib.rs\n"),
        "{out}"
    );
    let out = repo.ok_in(&repo.main.join("src"), &["log", "lib.rs"]);
    assert!(
        out.contains("initial commit") && out.contains("third commit"),
        "{out}"
    );
    assert!(!out.contains("feature work"), "{out}");
}

#[test]
fn a_name_that_is_a_revision_and_a_file_is_ambiguous() {
    let repo = Repo::new("sha1");
    fs::write(repo.main.join("feature"), "x").unwrap();
    let (code, err) = repo.fails(&["diff", "feature"]);
    assert_eq!(code, 128);
    assert!(err.contains("ambiguous argument 'feature'"), "{err}");
    let out = repo.ok(&["diff", "feature", "--", "README.md"]);
    assert!(out.contains("README.md"), "{out}");
}

#[test]
fn diff_refuses_an_argument_that_is_neither_a_revision_nor_a_path() {
    let repo = Repo::new("sha1");
    for (args, named) in [
        (vec!["diff", "nosuchrev"], "ambiguous argument 'nosuchrev'"),
        (
            vec!["diff", "nosuchpath"],
            "ambiguous argument 'nosuchpath'",
        ),
        (
            vec!["diff", "HEAD", "nosuchrev"],
            "ambiguous argument 'nosuchrev'",
        ),
        (
            vec!["diff", "--cached", "nosuchrev"],
            "ambiguous argument 'nosuchrev'",
        ),
        (
            vec!["diff", "nosuchrev..HEAD"],
            "ambiguous argument 'nosuchrev..HEAD'",
        ),
        (
            vec!["diff", "README.md", "nosuchx"],
            "nosuchx: no such path",
        ),
        (vec!["diff", "nosuchrev", "--"], "bad revision 'nosuchrev'"),
        (
            vec!["diff", "HEAD", "nosuchrev", "--"],
            "bad revision 'nosuchrev'",
        ),
    ] {
        let out = repo.run_in(&repo.main, &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.starts_with("fatal: ") && err.contains(named),
            "{args:?}: {err}"
        );
    }
}

#[test]
fn diff_still_takes_every_valid_form_of_argument() {
    let repo = Repo::new("sha1");
    for args in [
        vec!["diff", "HEAD", "HEAD~1"],
        vec!["diff", "HEAD~3..HEAD~2"],
        vec!["diff", "--cached", "HEAD"],
        vec!["diff", "README.md"],
        vec!["diff", "HEAD", "README.md"],
        vec!["diff", "HEAD", "--", "README.md"],
        vec!["diff", "--", "nosuchpath"],
        vec!["diff", "HEAD", "--", "nosuchpath"],
    ] {
        let out = repo.run_in(&repo.main, &args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = repo.ok_in(&repo.main.join("src"), &["diff", "HEAD", "lib.rs"]);
    assert!(
        out.starts_with("diff --git a/src/lib.rs b/src/lib.rs\n"),
        "{out}"
    );
}

#[test]
fn diff_names_a_deleted_file_after_the_separator() {
    let repo = Repo::new("sha1");
    fs::remove_file(repo.main.join("src/lib.rs")).unwrap();
    let out = repo.ok(&["diff", "--", "src/lib.rs"]);
    assert!(out.contains("deleted file mode"), "{out}");
    let out = repo.ok(&["diff", "HEAD", "--", "src/lib.rs"]);
    assert!(out.contains("deleted file mode"), "{out}");
    let (code, err) = repo.fails(&["diff", "src/lib.rs"]);
    assert_eq!(code, 128);
    assert!(err.contains("ambiguous argument 'src/lib.rs'"), "{err}");
}

#[test]
fn ignore_rules_come_from_every_source() {
    let repo = Repo::new("sha1");
    let before = repo.ok(&["status"]);
    assert!(!before.contains("private") && !before.contains("debug.out"));

    fs::create_dir_all(repo.home.join(".config/git")).unwrap();
    fs::write(repo.home.join(".config/git/ignore"), "*.bak\n!keep.bak\n").unwrap();
    fs::write(repo.main.join("x.bak"), "x").unwrap();
    fs::write(repo.main.join("keep.bak"), "x").unwrap();
    let out = repo.ok(&["status"]);
    assert!(!out.contains("x.bak") && out.contains("keep.bak"), "{out}");

    fs::remove_file(repo.home.join(".config/git/ignore")).unwrap();
    fs::write(repo.home.join("my-excludes"), "*.bak\n").unwrap();
    let config = repo.main.join(".git/config");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        format!("{text}[core]\n\texcludesFile = ~/my-excludes\n"),
    )
    .unwrap();
    let out = repo.ok(&["status"]);
    assert!(!out.contains(".bak"), "{out}");

    fs::write(repo.main.join(".git/info/exclude"), "private/\nnew.txt\n").unwrap();
    assert!(!repo.ok(&["status"]).contains("new.txt"));
}

#[test]
fn nested_gitignore_files_and_negations_apply() {
    let repo = Repo::new("sha1");
    fs::write(
        repo.main.join("src/.gitignore"),
        "*.gen\n!keep.gen\n/top\nsub/\n!sub/saved\n",
    )
    .unwrap();
    fs::create_dir_all(repo.main.join("src/sub")).unwrap();
    fs::create_dir_all(repo.main.join("src/x")).unwrap();
    for f in [
        "src/a.gen",
        "src/keep.gen",
        "src/top",
        "src/x/top",
        "src/x/b.gen",
        "src/c.txt",
        "src/sub/f.txt",
        "src/sub/saved",
    ] {
        fs::write(repo.main.join(f), "x").unwrap();
    }
    let rows = porcelain(&repo.ok(&["status"]));
    let shown: Vec<&str> = rows
        .iter()
        .filter(|r| r.starts_with("??") && r.contains("src/"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        shown,
        [
            "?? src/c.txt",
            "?? src/cache.tmp",
            "?? src/important.tmp",
            "?? src/keep.gen",
            "?? src/x/"
        ]
    );
}

/// Every invocation that only reads; each must succeed and leave the repository as it was.
const READ_ONLY: &[&[&str]] = &[
    &["status"],
    &["status", "-s"],
    &["status", "--short"],
    &["status", "--porcelain"],
    &["status", "-b"],
    &["status", "-sb"],
    &["status", "--porcelain", "-b"],
    &["status", "README.md"],
    &["status", "-s", "src"],
    &["diff"],
    &["diff", "HEAD"],
    &["diff", "HEAD~3", "HEAD~2", "--stat"],
    &["diff", "HEAD~3..HEAD~2"],
    &["diff", "--cached"],
    &["diff", "--staged", "HEAD~1"],
    &["diff", "--stat"],
    &["diff", "--shortstat", "HEAD~3", "HEAD"],
    &["diff", "--name-only", "HEAD~3", "HEAD"],
    &["diff", "--name-status", "HEAD~3", "HEAD"],
    &["diff", "-U1", "HEAD~3", "HEAD"],
    &["diff", "--", "README.md"],
    &["log"],
    &["log", "--commits"],
    &["log", "README.md"],
    &["log", "-n", "2"],
    &["log", "--oneline"],
    &["log", "--format=%H %s"],
    &["log", "--pretty=full"],
    &["log", "--date=iso"],
    &["log", "--stat"],
    &["log", "-p"],
    &["log", "--name-status"],
    &["log", "--reverse", "--skip", "1"],
    &["log", "--author", "Staging"],
    &["log", "--grep", "a", "-i"],
    &["log", "HEAD~3..HEAD"],
    &["log", "^HEAD~2", "HEAD"],
    &["log", "--", "README.md"],
    &["show"],
    &["show", "HEAD~3"],
    &["show", "--stat"],
    &["show", "--oneline", "HEAD"],
    &["show", "--format=%h", "HEAD~1"],
    &["files"],
    &["ls"],
    &["ls", "src"],
    &["branch"],
    &["branch", "-v"],
    &["branch", "-a"],
    &["branch", "-r"],
    &["branch", "-a", "-v"],
    &["branch", "--list"],
    &["branch", "--list", "m*"],
    &["branch", "--show-current"],
    &["tag"],
    &["tag", "-l"],
    &["tag", "-l", "v*"],
    &["stash", "list"],
    &["stash", "show"],
    &["remote"],
    &["remote", "-v"],
    &["remote", "get-url", "origin"],
    &["reflog"],
    &["reflog", "show"],
    &["reflog", "show", "HEAD"],
    &["clean", "-n"],
    &["clean", "-nd"],
    &["clean", "-nx"],
    &["clean", "-nX"],
    &["clean", "--dry-run", "src"],
    &["add", "-n", "README.md"],
    &["add", "--dry-run", "."],
    &["rm", "-n", "--cached", "README.md"],
    &["mv", "-n", "README.md", "MOVED.md"],
    &["config", "list"],
    &["config", "list", "--global"],
    &["config", "get", "server"],
];

/// Invocations that fail or print nothing useful; the repository must still not change.
const READ_ONLY_FAILING: &[&[&str]] = &[
    &["lock", "a"],
    &["audit"],
    &["verify"],
    &["commit", "-m", "x", "README.md"],
    &["restore", "README.md", "1"],
    &["status", "--revision", "HEAD"],
    &["log", "--filter", "*.rs"],
    &["show", "nonsense"],
    &["diff", "nonsense"],
    &["diff", "--cached", "nonsense"],
    &["stash", "show", "-p", "stash@{3}"],
    &["reflog", "show", "nonsense"],
    &["branch", "--contains", "HEAD"],
    &["branch", "-vv"],
    &["branch", "-a", "main"],
    &["remote", "show", "origin"],
    &["reflog", "exists", "HEAD"],
    &["remote", "get-url", "nonsense"],
    &["clean"],
    &["push", "-n"],
    &["push", "--dry-run", "origin"],
    &["rm", "-n", "README.md"],
    &["whoami"],
    &["get", "README.md"],
    &["locks"],
    &["summary"],
];

#[test]
fn every_command_leaves_the_git_directory_and_work_tree_untouched() {
    for format in FORMATS {
        let repo = Repo::new(format);
        // The fixture has a tag; add a remote and a stash so those listings read real data.
        repo.ok(&["remote", "add", "origin", "http://127.0.0.1:9/seed.git"]);
        fs::write(repo.main.join("README.md"), "seeded\n").unwrap();
        repo.ok(&["stash"]);
        let before = snapshot(&repo.main);
        let linked_before = snapshot(&repo.linked);
        let unchanged = |args: &[&str], name: &str| {
            assert_eq!(
                snapshot(&repo.main),
                before,
                "{format}: pyn {args:?} in {name} changed the main work tree or .git"
            );
            assert_eq!(
                snapshot(&repo.linked),
                linked_before,
                "{format}: pyn {args:?} in {name} changed the linked work tree"
            );
        };
        for (cwd, name) in [(&repo.main, "main"), (&repo.linked, "linked")] {
            for args in READ_ONLY {
                let out = repo.run_in(cwd, args);
                // the linked worktree has its own, shorter history
                assert!(
                    name == "linked" || out.status.success(),
                    "{format}: pyn {args:?} in {name}: {}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                unchanged(args, name);
            }
            for args in READ_ONLY_FAILING {
                repo.run_in(cwd, args);
                unchanged(args, name);
            }
        }
    }
}

#[test]
fn init_hides_pyn_from_git_and_changes_nothing_else() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let before = snapshot(&repo.main);
        let status_before = repo.ok(&["status"]);
        let out = repo.ok(&["init"]);
        assert!(out.contains("added .pyn/"), "{out}");
        let after = snapshot(&repo.main);
        let changed: Vec<&String> = after
            .keys()
            .filter(|k| before.get(*k) != after.get(*k))
            .chain(before.keys().filter(|k| !after.contains_key(*k)))
            .collect();
        assert_eq!(
            changed,
            [
                ".git/info/exclude",
                ".pyn",
                ".pyn/local_only",
                ".pyn/local_only/repo.toml"
            ],
            "{format}: {changed:?}"
        );
        assert_eq!(
            fs::read_to_string(repo.main.join(".git/info/exclude")).unwrap(),
            "private/\n.pyn/\n"
        );
        assert_eq!(
            repo.ok(&["status"]),
            status_before,
            "the .pyn folder is invisible to git"
        );

        let again = snapshot(&repo.main);
        let out = repo.ok(&["init"]);
        assert!(out.contains("already excluded"), "{out}");
        assert_eq!(snapshot(&repo.main), again, "init is idempotent");
    }
}

#[test]
fn init_adds_a_missing_newline_and_creates_the_exclude_file() {
    let repo = Repo::new("sha1");
    let exclude = repo.main.join(".git/info/exclude");
    fs::write(&exclude, "private/").unwrap();
    repo.ok(&["init"]);
    assert_eq!(fs::read_to_string(&exclude).unwrap(), "private/\n.pyn/\n");

    fs::remove_dir_all(repo.main.join(".git/info")).unwrap();
    fs::remove_dir_all(repo.main.join(".pyn")).unwrap();
    repo.ok(&["init"]);
    assert_eq!(fs::read_to_string(&exclude).unwrap(), ".pyn/\n");
}

#[test]
fn init_leaves_an_existing_exclusion_alone() {
    for existing in [".pyn\n", "/.pyn/\n", "# mine\n.pyn/\n"] {
        let repo = Repo::new("sha1");
        let exclude = repo.main.join(".git/info/exclude");
        fs::write(&exclude, existing).unwrap();
        assert!(repo.ok(&["init"]).contains("already excluded"));
        assert_eq!(fs::read_to_string(&exclude).unwrap(), existing);
    }
}

#[test]
fn init_in_a_linked_worktree_writes_the_shared_exclude_file() {
    let repo = Repo::new("sha1");
    repo.ok_in(&repo.linked, &["init"]);
    assert!(repo.linked.join(".pyn/local_only").is_dir());
    assert!(!repo.main.join(".pyn").exists());
    let exclude = fs::read_to_string(repo.main.join(".git/info/exclude")).unwrap();
    assert!(exclude.contains(".pyn/"), "{exclude}");
    assert!(
        !repo.linked.join(".git").is_dir(),
        "the linked .git stays a file"
    );
}

#[test]
fn init_follows_a_gitdir_file_to_a_separate_git_directory() {
    let repo = Repo::new("sha1");
    let moved = repo.main.parent().unwrap().join("elsewhere/store.git");
    fs::create_dir_all(moved.parent().unwrap()).unwrap();
    fs::rename(repo.main.join(".git"), &moved).unwrap();
    fs::write(repo.main.join(".git"), "gitdir: ../elsewhere/store.git\n").unwrap();
    repo.ok(&["init"]);
    assert!(
        fs::read_to_string(moved.join("info/exclude"))
            .unwrap()
            .contains(".pyn/")
    );
    assert!(repo.main.join(".pyn/local_only").is_dir());
    assert!(repo.ok(&["status"]).contains("README.md"));
}

#[test]
fn init_outside_a_git_repository_says_it_is_not_supported_yet() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = Repo::new("sha1");
    let out = repo.run_in(tmp.path(), &["init"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("'init' is a git command pyn does not support yet")
    );
}

#[test]
fn repository_commands_refuse_and_name_the_pyn_server() {
    let repo = Repo::new("sha1");
    let before = snapshot(&repo.main);
    for args in [
        vec!["lock", "README.md"],
        vec!["unlock", "README.md"],
        vec!["locks"],
        vec!["locks", "--mine"],
        vec!["policy", "preview", "x.toml"],
        vec!["audit"],
        vec!["verify"],
        vec!["summary"],
    ] {
        let (code, err) = repo.fails(&args);
        assert_eq!(code, 128, "{args:?}: {err}");
        assert!(
            err.starts_with(&format!("fatal: `pyn {}` needs a pyn server, and this repository is a git repository; `pyn convert` moves it to a pyn server", args[0])),
            "{args:?}: {err}"
        );
    }
    assert_eq!(snapshot(&repo.main), before);
}

#[test]
fn commands_a_git_home_will_gain_name_the_issue() {
    let repo = Repo::new("sha1");
    {
        let (args, issue) = (vec!["update"], "#255");
        let out = repo.run_in(&repo.main, &args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {err}");
        assert!(
            err.starts_with(&format!(
                "pyn: '{}' is not supported on a git repository yet",
                args.iter()
                    .find(|a| !a.starts_with('-') && !a.starts_with("http"))
                    .unwrap()
            )),
            "{args:?}: {err}"
        );
        assert!(err.contains(issue), "{args:?}: {err}");
    }
    let out = repo.run_in(&repo.main, &["convert"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("#95"));
    let out = repo.run_in(&repo.main, &["frobnicate"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("'frobnicate' is not a pyn command"));
}

#[test]
fn a_pyn_toml_in_a_git_repository_is_ignored() {
    let repo = Repo::new("sha1");
    fs::create_dir_all(repo.main.join(".pyn")).unwrap();
    fs::write(
        repo.main.join(".pyn/pyn.toml"),
        "[meta]\ndefault = \"exclusive\"\n",
    )
    .unwrap();
    let (code, err) = repo.fails(&["lock", "README.md"]);
    assert_eq!(code, 128);
    assert!(err.contains("needs a pyn server"), "{err}");
    assert!(repo.ok(&["status"]).contains("README.md"));
    let files = repo.ok(&["files"]);
    assert!(
        files.lines().skip(1).all(|l| l.starts_with("shared")),
        "{files}"
    );
}

#[test]
fn files_and_ls_list_the_index_and_head() {
    for format in FORMATS {
        let repo = Repo::new(format);
        let files = repo.ok(&["files"]);
        assert!(
            files.contains("src/lib.rs") && files.contains("staged.txt"),
            "{files}"
        );
        assert!(!files.contains("new.txt"));
        let ls = repo.ok(&["ls"]);
        assert!(ls.contains("src/") && ls.contains("README.md"), "{ls}");
        assert!(!ls.contains("staged.txt"), "ls shows HEAD");
        assert!(repo.ok(&["ls", "src"]).contains("lib.rs"));
    }
}

#[test]
fn an_empty_repository_has_no_log_or_show_but_a_status() {
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
    fs::write(root.join("a.txt"), "a").unwrap();
    let repo = Repo::new("sha1");
    let out = repo.ok_in(&root, &["status"]);
    assert!(
        out.starts_with("On branch trunk (no commits yet)\n"),
        "{out}"
    );
    assert!(out.contains("untracked") && out.contains("a.txt"));
    let out = repo.run_in(&root, &["log"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("your current branch 'trunk' does not have any commits yet")
    );
    assert!(repo.run_in(&root, &["show"]).status.code() == Some(128));
}

#[test]
fn a_detached_head_is_named_by_its_abbreviation() {
    let repo = Repo::new("sha1");
    let ids = repo.expected("commits.txt");
    let id = ids.lines().nth(1).unwrap();
    fs::write(repo.main.join(".git/HEAD"), format!("{id}\n")).unwrap();
    let out = repo.ok(&["status"]);
    assert!(
        out.starts_with(&format!("HEAD detached at {}\n", &id[..7])),
        "{out}"
    );
}

#[test]
fn a_clone_made_inside_a_git_repository_is_still_a_pyn_workspace() {
    let env = common::start();
    env.seed("alice", "a.txt", "one", None);
    let repo = Repo::new("sha1");
    let source = env.source(common::REPO);
    env.ok(&repo.main, "alice", &["clone", &source, "clones/game"]);
    let clone = repo.main.join("clones/game");
    assert!(
        env.ok(&clone, "alice", &["status"])
            .contains("everything is up to date")
    );
    assert!(env.ok(&clone, "alice", &["whoami"]).contains("alice"));
    let out = repo.ok(&["status"]);
    assert!(
        out.starts_with("On branch main\n") && out.contains("clones/"),
        "{out}"
    );
}

#[test]
fn init_records_the_git_marker_and_a_pyn_marker_turns_the_home_to_pyn() {
    let repo = Repo::new("sha1");
    repo.ok(&["init"]);
    let file = repo.main.join(".pyn/local_only/repo.toml");
    assert_eq!(fs::read_to_string(&file).unwrap(), "repo_tool = \"git\"\n");
    repo.ok(&["status"]);

    fs::write(&file, "repo_tool = \"pyn\"\n").unwrap();
    let (_, err) = repo.fails(&["log"]);
    assert!(!err.contains("not available on a git repository"), "{err}");
    assert!(
        repo.run_in(&repo.main, &["init"]).status.code() == Some(1),
        "a pyn home refuses init"
    );

    fs::write(&file, "repo_tool = \"svn\"\n").unwrap();
    let (code, err) = repo.fails(&["status"]);
    assert!(
        code != 0 && err.contains("unknown repo_tool \"svn\""),
        "{err}"
    );
}

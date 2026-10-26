//! `pyn submodule` and `clone --recurse-submodules` on a git repository, against real git: a repository pyn builds
//! is accepted by git (`submodule status`, `fsck`, `status`) and one git builds is operated on by pyn.
//! Needs `git` on the PATH (tests only).

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

/// Two sibling repositories to be nested, `lib` (which nests `deep`) and `deep`, under a temporary folder.
struct World {
    _tmp: TempDir,
    root: PathBuf,
    format: &'static str,
}

impl World {
    fn new(format: &'static str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let deep = root.join("deep");
        init(&deep, format, false);
        commit(&deep, "d", b"deep\n", "deep one");
        let lib = root.join("lib");
        init(&lib, format, false);
        commit(&lib, "f", b"one\n", "one");
        run(&lib, &["tag", "v1"]);
        run(&lib, &["submodule", "add", "-q", "../deep", "deep"]);
        run(&lib, &["commit", "-qm", "nest deep"]);
        commit(&lib, "f", b"two\n", "two");
        Self {
            _tmp: tmp,
            root,
            format,
        }
    }

    fn lib(&self) -> PathBuf {
        self.root.join("lib")
    }

    /// A superproject with one commit and no remote.
    fn sup(&self, name: &str) -> PathBuf {
        let sup = self.root.join(name);
        init(&sup, self.format, false);
        commit(&sup, "s", b"s\n", "s");
        sup
    }

    fn pyn(&self, dir: &Path, args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(dir)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("TZ", "UTC")
            .output()
            .unwrap()
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let out = self.pyn(dir, args);
        assert!(
            out.status.success(),
            "pyn {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn fails(&self, dir: &Path, args: &[&str], code: i32) -> String {
        let out = self.pyn(dir, args);
        assert_eq!(out.status.code(), Some(code), "pyn {args:?}");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    /// Everything git makes of the repository: it must be consistent and its work tree clean.
    fn accepted(&self, dir: &Path) {
        fsck(dir);
        assert_eq!(run(dir, &["status", "--porcelain"]), "");
    }

    fn norm(&self, text: &str) -> String {
        text.replace(self.root.to_str().unwrap(), "ROOT")
    }
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap()
}

fn each(check: impl Fn(&World)) {
    for format in FORMATS {
        check(&World::new(format));
    }
}

#[test]
fn add_leaves_what_git_leaves() {
    each(|w| {
        let (a, b) = (w.sup("by_git"), w.sup("by_pyn"));
        run(&a, &["submodule", "add", "-q", "../lib", "sub"]);
        w.ok(&b, &["submodule", "add", "../lib", "sub"]);
        for file in [
            ".gitmodules",
            "sub/.git",
            ".git/modules/sub/HEAD",
            ".git/modules/sub/config",
        ] {
            assert_eq!(
                w.norm(&read(a.join(file))).replace("by_git", "X"),
                w.norm(&read(b.join(file))).replace("by_pyn", "X"),
                "{file}"
            );
        }
        let section = |dir: &Path| {
            run(dir, &["config", "--get-regexp", "^submodule\\."])
                .replace(dir.to_str().unwrap(), "SUP")
                .replace(w.root.join("lib").to_str().unwrap(), "LIB")
        };
        assert_eq!(section(&a), section(&b));
        assert_eq!(
            run(&a, &["status", "--porcelain"]),
            run(&b, &["status", "--porcelain"])
        );
        run(&b, &["commit", "-qm", "add sub"]);
        w.accepted(&b);
        assert_eq!(
            run(&b, &["submodule", "status"]),
            w.ok(&b, &["submodule", "status"])
        );
        assert!(read(b.join("sub/f")).starts_with("two"));
    });
}

#[test]
fn add_records_the_branch_and_an_existing_repository() {
    each(|w| {
        let sup = w.sup("sup");
        w.ok(&sup, &["submodule", "add", "-b", "main", "../lib", "sub"]);
        assert!(read(sup.join(".gitmodules")).contains("branch = main"));
        let local = sup.join("local");
        run(&sup, &["clone", "-q", w.lib().to_str().unwrap(), "local"]);
        let out = w.ok(&sup, &["submodule", "add", "../lib", "local"]);
        assert!(out.contains("Adding existing repo at 'local' to the index"));
        run(&sup, &["commit", "-qm", "add"]);
        assert!(local.join(".git").is_dir());
        w.ok(&sup, &["submodule", "absorbgitdirs"]);
        assert!(read(local.join(".git")).starts_with("gitdir: ../.git/modules/local"));
        w.accepted(&sup);
        assert_eq!(
            run(&sup, &["submodule", "status"]),
            w.ok(&sup, &["submodule", "status"])
        );
    });
}

#[test]
fn add_refuses_what_git_refuses() {
    each(|w| {
        let sup = w.sup("sup");
        w.ok(&sup, &["submodule", "add", "../lib", "sub"]);
        let err = w.fails(&sup, &["submodule", "add", "../lib", "sub"], 128);
        assert!(err.contains("'sub' already exists in the index"), "{err}");
        let err = w.fails(&sup, &["submodule", "add", "../missing", "other"], 128);
        assert!(err.contains("clone of"), "{err}");
        assert!(!sup.join("other").exists());
        assert!(!sup.join(".git/modules/other").exists());
    });
}

/// A superproject git made with `lib` as `sub` (and `lib`'s own `deep` inside it), committed.
fn made_by_git(w: &World, name: &str) -> PathBuf {
    let sup = w.sup(name);
    run(&sup, &["submodule", "add", "-q", "../lib", "sub"]);
    run(&sup, &["commit", "-qm", "add sub"]);
    sup
}

#[test]
fn status_matches_git_in_every_state() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        let same = |args: &[&str]| {
            assert_eq!(w.ok(&sup, args), run(&sup, args), "{args:?}");
        };
        same(&["submodule", "status"]);
        run(
            &sup,
            &["submodule", "update", "-q", "--init", "--recursive"],
        );
        same(&["submodule", "status"]);
        same(&["submodule", "status", "--recursive"]);
        same(&["submodule", "status", "--cached"]);
        let sub = sup.join("sub");
        commit(&sub, "f", b"three\n", "three");
        same(&["submodule", "status"]);
        same(&["submodule", "status", "--cached"]);
        same(&["submodule", "status", "--recursive", "sub"]);
        run(&sup, &["submodule", "deinit", "-q", "-f", "--all"]);
        same(&["submodule", "status"]);
    });
}

#[test]
fn init_and_update_follow_git() {
    each(|w| {
        let by_git = made_by_git(w, "by_git");
        let by_pyn = made_by_git(w, "by_pyn");
        run(&by_git, &["submodule", "deinit", "-q", "-f", "--all"]);
        run(&by_pyn, &["submodule", "deinit", "-q", "-f", "--all"]);
        run(&by_git, &["submodule", "init", "-q"]);
        let err = w.pyn(&by_pyn, &["submodule", "init"]);
        assert!(err.status.success());
        assert!(
            String::from_utf8_lossy(&err.stderr).contains("registered for path 'sub'"),
            "{}",
            String::from_utf8_lossy(&err.stderr)
        );
        let config = |d: &Path| {
            w.norm(&run(d, &["config", "--get-regexp", "^submodule\\."]))
                .replace("by_git", "X")
                .replace("by_pyn", "X")
        };
        assert_eq!(config(&by_git), config(&by_pyn));
        run(&by_git, &["submodule", "update", "-q", "--recursive"]);
        let out = w.ok(&by_pyn, &["submodule", "update", "--recursive"]);
        assert!(out.contains("Submodule path 'sub': checked out '"), "{out}");
        for dir in [&by_git, &by_pyn] {
            assert!(!dir.join("sub/deep/d").exists());
            assert!(read(dir.join("sub/f")).starts_with("two"));
        }
        w.ok(&by_pyn, &["submodule", "update", "--init", "--recursive"]);
        run(
            &by_git,
            &["submodule", "update", "-q", "--init", "--recursive"],
        );
        assert_eq!(read(by_pyn.join("sub/deep/d")), "deep\n");
        w.accepted(&by_pyn);
        assert_eq!(
            run(&by_pyn, &["submodule", "status", "--recursive"]),
            w.ok(&by_pyn, &["submodule", "status", "--recursive"])
        );
        let again = w.ok(&by_pyn, &["submodule", "update", "--recursive"]);
        assert_eq!(again, "");
    });
}

#[test]
fn update_moves_to_the_recorded_commit_and_fetches_it() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        w.ok(&sup, &["submodule", "update", "--init"]);
        let third = commit(&w.lib(), "f", b"three\n", "three");
        let record = format!("160000,{third},sub");
        run(&sup, &["update-index", "--cacheinfo", &record]);
        run(&sup, &["commit", "-qm", "bump"]);
        let out = w.ok(&sup, &["submodule", "update"]);
        assert!(out.contains(&third), "{out}");
        assert_eq!(run(&sup.join("sub"), &["rev-parse", "HEAD"]).trim(), third);
        run(&sup.join("sub"), &["checkout", "-q", "HEAD~1"]);
        let out = w.ok(&sup, &["submodule", "update"]);
        assert!(out.contains(&third), "{out}");
        w.accepted(&sup);
    });
}

#[test]
fn update_remote_merge_and_rebase() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        w.ok(&sup, &["submodule", "update", "--init"]);
        let tip = commit(&w.lib(), "g", b"g\n", "remote work");
        let out = w.ok(&sup, &["submodule", "update", "--remote"]);
        assert!(out.contains(&format!("checked out '{tip}'")), "{out}");
        assert_eq!(
            w.ok(&sup, &["submodule", "status"]).as_bytes()[0],
            b'+',
            "the superproject still records the old commit"
        );
        run(
            &sup,
            &["submodule", "update", "-q", "--checkout", "--force"],
        );
        let sub = sup.join("sub");
        run(&sub, &["switch", "-q", "main"]);
        commit(&sub, "local", b"l\n", "local work");
        let out = w.ok(&sup, &["submodule", "update", "--remote", "--merge"]);
        assert!(out.contains(&format!("merged in '{tip}'")), "{out}");
        assert!(sub.join("g").exists() && sub.join("local").exists());
        run(&sup, &["-C", "sub", "reset", "-q", "--hard", "HEAD~1"]);
        commit(&sub, "local2", b"l\n", "more local work");
        let out = w.ok(&sup, &["submodule", "update", "--remote", "--rebase"]);
        assert!(out.contains(&format!("rebased into '{tip}'")), "{out}");
        assert_eq!(
            run(&sub, &["rev-list", "--count", &format!("{tip}..HEAD")]).trim(),
            "2"
        );
        fsck(&sub);
    });
}

#[test]
fn foreach_runs_where_git_runs() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        w.ok(&sup, &["submodule", "update", "--init", "--recursive"]);
        let script = r#"echo "$name:$sm_path:$displaypath:$sha1:$(basename "$toplevel"):$(basename "$PWD")""#;
        for args in [
            vec!["submodule", "foreach", script],
            vec!["submodule", "foreach", "--recursive", script],
            vec!["submodule", "--quiet", "foreach", "--recursive", script],
        ] {
            assert_eq!(w.ok(&sup, &args), run(&sup, &args), "{args:?}");
        }
        let err = w.fails(&sup, &["submodule", "foreach", "exit 3"], 128);
        assert!(
            err.contains("run_command returned non-zero status for sub"),
            "{err}"
        );
    });
}

#[test]
fn sync_rewrites_urls_like_git() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        w.ok(&sup, &["submodule", "update", "--init"]);
        run(
            &sup,
            &[
                "config",
                "-f",
                ".gitmodules",
                "submodule.sub.url",
                "../other",
            ],
        );
        let out = w.ok(&sup, &["submodule", "sync"]);
        assert_eq!(out, "Synchronizing submodule url for 'sub'\n");
        let want = w.root.join("other");
        assert_eq!(
            run(&sup, &["config", "submodule.sub.url"]).trim(),
            want.to_str().unwrap()
        );
        assert_eq!(
            run(&sup.join("sub"), &["config", "remote.origin.url"]).trim(),
            want.to_str().unwrap()
        );
        assert_eq!(w.ok(&sup, &["submodule", "--quiet", "sync"]), "");
    });
}

#[test]
fn deinit_clears_and_update_restores() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        w.ok(&sup, &["submodule", "update", "--init"]);
        fs::write(sup.join("sub/untracked"), "x").unwrap();
        let err = w.fails(&sup, &["submodule", "deinit", "sub"], 128);
        assert!(err.contains("contains local modifications"), "{err}");
        w.fails(&sup, &["submodule", "deinit"], 128);
        let out = w.ok(&sup, &["submodule", "deinit", "--force", "sub"]);
        assert!(out.contains("Cleared directory 'sub'"), "{out}");
        assert!(out.contains("unregistered for path 'sub'"), "{out}");
        assert!(fs::read_dir(sup.join("sub")).unwrap().next().is_none());
        assert!(
            !git(&sup)
                .args(["config", "submodule.sub.url"])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(sup.join(".git/modules/sub").is_dir());
        assert_eq!(w.ok(&sup, &["submodule", "status"]).as_bytes()[0], b'-');
        w.ok(&sup, &["submodule", "update", "--init"]);
        assert_eq!(read(sup.join("sub/f")), "two\n");
        w.accepted(&sup);
    });
}

#[test]
fn set_branch_and_set_url_edit_gitmodules() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        w.ok(&sup, &["submodule", "set-branch", "--branch", "dev", "sub"]);
        assert_eq!(
            run(
                &sup,
                &["config", "-f", ".gitmodules", "submodule.sub.branch"]
            )
            .trim(),
            "dev"
        );
        w.ok(&sup, &["submodule", "set-branch", "--default", "sub"]);
        assert!(!read(sup.join(".gitmodules")).contains("branch"));
        w.ok(&sup, &["submodule", "set-url", "sub", "../elsewhere"]);
        assert_eq!(
            run(&sup, &["config", "-f", ".gitmodules", "submodule.sub.url"]).trim(),
            "../elsewhere"
        );
        let err = w.fails(&sup, &["submodule", "set-url", "nope", "x"], 128);
        assert!(err.contains("no submodule mapping"), "{err}");
    });
}

#[test]
fn a_changed_gitlink_commits_and_git_agrees() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        w.ok(&sup, &["submodule", "update", "--init"]);
        let sub = sup.join("sub");
        let new = commit(&sub, "f", b"changed\n", "change");
        assert_eq!(run(&sup, &["status", "--porcelain"]), " M sub\n");
        w.ok(&sup, &["add", "sub"]);
        assert_eq!(run(&sup, &["status", "--porcelain"]), "M  sub\n");
        w.ok(&sup, &["commit", "-m", "bump sub"]);
        assert_eq!(
            run(&sup, &["ls-tree", "HEAD", "sub"])
                .split_whitespace()
                .nth(2),
            Some(new.as_str())
        );
        w.accepted(&sup);
        assert_eq!(
            run(&sup, &["submodule", "status"]),
            w.ok(&sup, &["submodule", "status"])
        );
    });
}

#[test]
fn clone_recurse_submodules_matches_git() {
    each(|w| {
        let sup = made_by_git(w, "origin");
        let url = format!("file://{}", sup.display());
        let by_git = w.root.join("c_git");
        let by_pyn = w.root.join("c_pyn");
        run(
            &w.root,
            &["clone", "-q", "--recurse-submodules", &url, "c_git"],
        );
        let out = w.pyn(&w.root, &["clone", "--recurse-submodules", &url, "c_pyn"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Submodule path 'sub': checked out"),
            "{stdout}"
        );
        assert!(
            stdout.contains("Submodule path 'sub/deep': checked out"),
            "{stdout}"
        );
        for dir in [&by_git, &by_pyn] {
            assert_eq!(read(dir.join("sub/f")), "two\n");
            assert_eq!(read(dir.join("sub/deep/d")), "deep\n");
        }
        let norm = |d: &Path, name: &str| {
            w.norm(&run(d, &["submodule", "status", "--recursive"]))
                .replace(name, "X")
        };
        assert_eq!(norm(&by_git, "c_git"), norm(&by_pyn, "c_pyn"));
        assert_eq!(
            w.ok(&by_pyn, &["submodule", "status", "--recursive"]),
            run(&by_pyn, &["submodule", "status", "--recursive"])
        );
        for p in ["", "sub", "sub/deep"] {
            let a = run(
                &by_git.join(p),
                &["config", "--get-regexp", "^(submodule|remote)\\."],
            );
            let b = run(
                &by_pyn.join(p),
                &["config", "--get-regexp", "^(submodule|remote)\\."],
            );
            assert_eq!(
                w.norm(&a).replace("c_git", "X"),
                w.norm(&b).replace("c_pyn", "X"),
                "{p}"
            );
        }
        w.accepted(&by_pyn);
        w.accepted(&by_pyn.join("sub"));
        w.accepted(&by_pyn.join("sub/deep"));
    });
}

#[test]
fn clone_without_recursion_leaves_empty_folders_and_the_alias_works() {
    each(|w| {
        let sup = made_by_git(w, "origin");
        let url = format!("file://{}", sup.display());
        w.ok(&w.root, &["clone", &url, "plain"]);
        assert!(
            fs::read_dir(w.root.join("plain/sub"))
                .unwrap()
                .next()
                .is_none()
        );
        assert_eq!(
            w.ok(&w.root.join("plain"), &["submodule", "status"])
                .as_bytes()[0],
            b'-'
        );
        w.ok(&w.root, &["clone", "--recursive", &url, "alias"]);
        assert!(w.root.join("alias/sub/deep/d").exists());
        w.ok(
            &w.root,
            &["clone", "--recurse-submodules=sub", &url, "some"],
        );
        assert!(w.root.join("some/sub/f").exists());
        assert!(w.root.join("some/sub/deep/d").exists());
    });
}

#[test]
fn shallow_submodules_are_cloned_at_depth_one() {
    each(|w| {
        let sup = made_by_git(w, "origin");
        let url = format!("file://{}", sup.display());
        w.ok(
            &w.root,
            &[
                "clone",
                "--recurse-submodules",
                "--shallow-submodules",
                &url,
                "shallow",
            ],
        );
        let sub = w.root.join("shallow/sub");
        assert_eq!(run(&sub, &["rev-list", "--count", "HEAD"]).trim(), "1");
        assert!(sub.join("f").exists());
        fsck(&sub);
    });
}

#[test]
fn working_from_a_subfolder_names_paths_from_there() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        fs::create_dir_all(sup.join("d")).unwrap();
        let out = w.ok(&sup.join("d"), &["submodule", "status"]);
        assert_eq!(out, run(&sup.join("d"), &["submodule", "status"]));
        w.ok(&sup.join("d"), &["submodule", "update", "--init", "../sub"]);
        assert!(sup.join("sub/f").exists());
    });
}

#[test]
fn unknown_paths_are_refused() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        let err = w.fails(&sup, &["submodule", "status", "nope"], 1);
        assert!(
            err.contains("pathspec 'nope' did not match any file(s) known to git"),
            "{err}"
        );
    });
}

/// A superproject with `sub` checked out, a branch `old` recording the earlier commit and `main` recording a later one.
fn two_records(w: &World) -> (PathBuf, String, String) {
    let sup = made_by_git(w, "sup");
    w.ok(&sup, &["submodule", "update", "--init"]);
    let sub = sup.join("sub");
    let two = run(&sub, &["rev-parse", "HEAD"]).trim().to_owned();
    run(&sup, &["branch", "old"]);
    let three = commit(&w.lib(), "f", b"three\n", "three");
    run(&sub, &["fetch", "-q"]);
    run(&sub, &["checkout", "-q", &three]);
    run(&sup, &["add", "sub"]);
    run(&sup, &["commit", "-qm", "bump"]);
    (sup, two, three)
}

fn head(dir: &Path) -> String {
    run(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

#[test]
fn switch_and_checkout_recurse_into_submodules() {
    each(|w| {
        let (sup, two, three) = two_records(w);
        let sub = sup.join("sub");
        w.ok(&sup, &["switch", "old"]);
        assert_eq!(head(&sub), three, "no recursion without the flag");
        w.ok(&sup, &["switch", "main"]);
        w.ok(&sup, &["switch", "--recurse-submodules", "old"]);
        assert_eq!(head(&sub), two);
        w.ok(&sup, &["checkout", "--recurse-submodules", "main"]);
        assert_eq!(head(&sub), three);
        run(&sup, &["config", "submodule.recurse", "true"]);
        w.ok(&sup, &["checkout", "old"]);
        assert_eq!(head(&sub), two);
        w.ok(&sup, &["switch", "--no-recurse-submodules", "main"]);
        assert_eq!(head(&sub), two);
        w.ok(&sup, &["switch", "old"]);
        w.ok(&sup, &["checkout", "main"]);
        assert_eq!(head(&sub), three);
    });
}

#[test]
fn reset_hard_recurses_into_submodules() {
    each(|w| {
        let (sup, two, three) = two_records(w);
        let sub = sup.join("sub");
        let tip = head(&sup);
        w.ok(&sup, &["reset", "--hard", "--recurse-submodules", "HEAD~1"]);
        assert_eq!(head(&sub), two);
        w.ok(&sup, &["reset", "--hard", &tip]);
        assert_eq!(head(&sub), two);
        w.ok(&sup, &["reset", "--hard", "--recurse-submodules", &tip]);
        assert_eq!(head(&sub), three);
        w.accepted(&sup);
    });
}

#[test]
fn fetch_recurses_into_submodules() {
    each(|w| {
        let sup = made_by_git(w, "work");
        let url = format!("file://{}", sup.display());
        w.ok(&w.root, &["clone", "--recurse-submodules", &url, "copy"]);
        let copy = w.root.join("copy");
        let sub = copy.join("sub");
        let tracked = |d: &Path| {
            run(d, &["rev-parse", "refs/remotes/origin/main"])
                .trim()
                .to_owned()
        };
        let before = tracked(&sub);
        let third = commit(&w.lib(), "f", b"three\n", "three");

        w.ok(&copy, &["fetch"]);
        assert_eq!(
            tracked(&sub),
            before,
            "on demand: nothing in the superproject needs it"
        );
        w.ok(&copy, &["fetch", "--no-recurse-submodules"]);
        assert_eq!(tracked(&sub), before);
        let err = w.pyn(&copy, &["fetch", "--recurse-submodules"]);
        assert!(String::from_utf8_lossy(&err.stderr).contains("Fetching submodule sub"));
        assert_eq!(tracked(&sub), third);

        let fourth = commit(&w.lib(), "f", b"four\n", "four");
        let record = format!("160000,{fourth},sub");
        run(&sup, &["update-index", "--cacheinfo", &record]);
        run(&sup, &["commit", "-qm", "bump"]);
        w.ok(&copy, &["fetch"]);
        assert_eq!(
            tracked(&sub),
            fourth,
            "the new record names a commit the submodule lacked"
        );
        assert_eq!(run(&sub, &["cat-file", "-t", &fourth]).trim(), "commit");
    });
}

#[test]
fn pull_recurses_into_submodules() {
    each(|w| {
        let sup = made_by_git(w, "work");
        let url = format!("file://{}", sup.display());
        w.ok(&w.root, &["clone", "--recurse-submodules", &url, "copy"]);
        let copy = w.root.join("copy");
        let fourth = commit(&w.lib(), "f", b"four\n", "four");
        let record = format!("160000,{fourth},sub");
        run(&sup, &["update-index", "--cacheinfo", &record]);
        run(&sup, &["commit", "-qm", "bump"]);
        w.ok(&copy, &["pull", "--no-rebase"]);
        assert_ne!(head(&copy.join("sub")), fourth);
        run(&sup, &["commit", "-q", "--allow-empty", "-m", "again"]);
        w.ok(&copy, &["pull", "--no-rebase", "--recurse-submodules"]);
        assert_eq!(head(&copy.join("sub")), fourth);
        w.accepted(&copy);
    });
}

#[test]
fn summary_matches_git() {
    each(|w| {
        let sup = made_by_git(w, "sup");
        let same = |args: &[&str]| {
            let mut full = vec!["submodule", "summary"];
            full.extend_from_slice(args);
            assert_eq!(w.ok(&sup, &full), run(&sup, &full), "{full:?}");
        };
        same(&[]);
        same(&["HEAD~1"]);
        w.ok(&sup, &["submodule", "update", "--init"]);
        same(&[]);
        let sub = sup.join("sub");
        commit(&sub, "g", b"g\n", "side one");
        commit(&sub, "h", b"h\n", "side two");
        for args in [
            vec![],
            vec!["--files"],
            vec!["--cached"],
            vec!["-n", "1"],
            vec!["--summary-limit", "0"],
            vec!["HEAD", "sub"],
            vec!["--", "other"],
        ] {
            same(&args);
        }
        run(&sup, &["add", "sub"]);
        same(&["--cached"]);
        same(&["--files"]);
        run(&sub, &["checkout", "-q", "HEAD~2"]);
        run(&sub, &["checkout", "-q", "-b", "elsewhere"]);
        commit(&sub, "x", b"x\n", "diverged");
        same(&[]);
        same(&["--files"]);
        run(&sup, &["submodule", "deinit", "-q", "-f", "--all"]);
        same(&[]);
        same(&["--cached"]);
    });
}

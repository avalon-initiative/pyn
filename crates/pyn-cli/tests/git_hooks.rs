//! Client hooks on a git repository: the same hook script under real git and under pyn must see the same arguments,
//! standard input, working directory and environment and must cause the same abort or continue. Real git is the oracle in
//! these tests only; wording is never compared.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

/// Records what the hook saw; `HOOKFAIL` names the hooks that exit 1, `HOOKEDIT` makes the message hooks edit the message.
const HOOK: &str = r#"#!/bin/sh
n=$(basename "$0")
{
echo "== $n"
echo "arg0: $0"
echo "args: $*"
echo "cwd: $(pwd)"
for v in GIT_INDEX_FILE GIT_PREFIX GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_AUTHOR_DATE GIT_EDITOR GIT_DIR GIT_WORK_TREE; do
  eval "echo $v=\${$v-unset}"
done
case "$n" in
  pre-push|post-rewrite) echo "stdin:"; cat ;;
  prepare-commit-msg|commit-msg) echo "msg: $(cat "$1")" ;;
esac
echo "out-of-band"
} | sed "s#$HOOKROOT#<root>#g" >> "$HOOKLOG"
echo "stdout of $n"
case "$n" in
  prepare-commit-msg|commit-msg) [ -n "$HOOKEDIT" ] && echo "edited by $n" >> "$1" ;;
esac
case " $HOOKFAIL " in *" $n "*) exit 1 ;; esac
exit 0
"#;

/// A message editor: records what it was given, then edits (`EDITMODE` unset), empties the file (`empty`) or fails (`fail`).
const EDITOR: &str = r#"#!/bin/sh
{
echo "== editor"
echo "file: $(basename "$1")"
cat "$1"
} | sed "s#$HOOKROOT#<root>#g" >> "$HOOKLOG"
case "$EDITMODE" in
  empty) : > "$1" ;;
  fail) exit 3 ;;
  *) echo "edited by editor" >> "$1" ;;
esac
"#;

const ALL: [&str; 9] = [
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-merge-commit",
    "post-merge",
    "post-checkout",
    "pre-push",
    "post-rewrite",
];

/// Arguments, environment and whether the editor is expected to open.
type EditCase<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], bool);

struct Side {
    root: PathBuf,
    repo: PathBuf,
    pyn: bool,
}

struct Run {
    code: i32,
    stderr: String,
}

impl Side {
    fn cmd(&self, dir: &Path) -> Command {
        let mut c = if self.pyn {
            let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
            c.env_clear().env("PATH", path_env()).current_dir(dir);
            c
        } else {
            git(dir)
        };
        c.env("HOME", self.root.join("home"))
            .env("TZ", "UTC")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Ann Author")
            .env("GIT_AUTHOR_EMAIL", "ann@example.com")
            .env("GIT_COMMITTER_NAME", "Cy Committer")
            .env("GIT_COMMITTER_EMAIL", "cy@example.com")
            .env("GIT_AUTHOR_DATE", "1700000000 +0100")
            .env("GIT_COMMITTER_DATE", "1700000100 -0200")
            .env("GIT_EDITOR", "true")
            .env("HOOKLOG", self.root.join("hook.log"))
            .env("HOOKROOT", &self.root);
        c
    }

    fn run_in(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
        let mut c = self.cmd(dir);
        for (key, value) in env {
            match *value {
                "<unset>" => c.env_remove(key),
                _ => c.env(key, value),
            };
        }
        c.args(args);
        let out = c.output().unwrap();
        Run {
            code: out.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn git(&self, args: &[&str]) -> String {
        let mut c = git(&self.repo);
        c.env("TZ", "UTC")
            .env("GIT_AUTHOR_DATE", "1700000000 +0100")
            .env("GIT_COMMITTER_DATE", "1700000100 -0200");
        let out = c.args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("hook.log")).unwrap_or_default()
    }

    /// The merge state files `MERGE_MSG` leaves behind, which `state` does not show.
    fn merge_files(&self) -> String {
        let git_dir = self.repo.join(".git");
        let mut out = String::new();
        for name in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
            let text = fs::read_to_string(git_dir.join(name)).ok();
            out += &format!("== {name}: {text:?}\n");
        }
        out
    }

    fn state(&self) -> String {
        let mut out = String::new();
        for args in [
            &["for-each-ref", "--format=%(refname) %(objectname)"][..],
            &["log", "-3", "--format=%H %P%n%B"],
            &["ls-files", "-s"],
            &["status", "--porcelain"],
            &["rev-parse", "--verify", "-q", "MERGE_HEAD"],
            &["ls-remote", "../remote.git"],
        ] {
            out += &format!("== {args:?}\n{}", self.git(args));
        }
        let stray = |name: &str| self.repo.join(".git").join(name).exists();
        out += &format!("index.lock: {}\n", stray("index.lock"));
        out
    }
}

struct Pair {
    _tmp: TempDir,
    o: Side,
    p: Side,
}

fn write_hook(repo: &Path, name: &str) {
    let path = repo.join(".git/hooks").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, HOOK).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

impl Pair {
    /// Two identical repositories with a bare remote each, a history of two commits, branch `side`, and hooks installed.
    fn new(format: &str, hooks: &[&str]) -> Self {
        let tmp = tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let mk = |name: &str, pyn: bool| {
            let root = base.join(name);
            let repo = root.join("repo");
            fs::create_dir_all(root.join("home")).unwrap();
            init(&repo, format, false);
            init(&root.join("remote.git"), format, true);
            Side { root, repo, pyn }
        };
        let (o, p) = (mk("o", false), mk("p", true));
        for s in [&o, &p] {
            fs::create_dir_all(s.repo.join("sub")).unwrap();
            fs::write(s.repo.join("a.txt"), "one\n").unwrap();
            fs::write(s.repo.join("sub/s.txt"), "s\n").unwrap();
            s.git(&["add", "."]);
            s.git(&["commit", "-q", "-m", "one"]);
            fs::write(s.repo.join("b.txt"), "two\n").unwrap();
            s.git(&["add", "."]);
            s.git(&["commit", "-q", "-m", "two"]);
            s.git(&["branch", "side"]);
            s.git(&["remote", "add", "o", "../remote.git"]);
            for h in hooks {
                write_hook(&s.repo, h);
            }
        }
        Self { _tmp: tmp, o, p }
    }

    fn both(&self, f: impl Fn(&Side)) {
        f(&self.o);
        f(&self.p);
    }

    /// Runs `args` in `cwd` (relative to the work tree) on both and requires equal status, hook records and state.
    fn same(&self, cwd: &str, args: &[&str], env: &[(&str, &str)]) -> Run {
        self.same_with(cwd, args, env, true)
    }

    /// [`Pair::same`]; with `tracking` false the remote-tracking refs are left out of the state.
    fn same_with(&self, cwd: &str, args: &[&str], env: &[(&str, &str)], tracking: bool) -> Run {
        let want = self.o.run_in(&self.o.repo.join(cwd), args, env);
        let got = self.p.run_in(&self.p.repo.join(cwd), args, env);
        let ctx = format!("{args:?} in {cwd:?} env {env:?}");
        assert_eq!(
            got.code, want.code,
            "{ctx}: exit status\npyn: {}\ngit: {}",
            got.stderr, want.stderr
        );
        first_difference(
            &self.p.log(),
            &self.o.log(),
            &format!("{ctx}: what the hooks saw"),
        );
        let keep = |state: String| {
            let kept: Vec<_> = state
                .lines()
                .filter(|l| {
                    tracking || !l.contains(" refs/remotes/") && !l.starts_with("refs/remotes/")
                })
                .collect();
            kept.join("\n")
        };
        first_difference(
            &keep(self.p.state()),
            &keep(self.o.state()),
            &format!("{ctx}: repository state"),
        );
        got
    }
}

/// Fails naming the first line where pyn's text departs from git's.
fn first_difference(got: &str, want: &str, what: &str) {
    if got == want {
        return;
    }
    let at = got.lines().zip(want.lines()).position(|(g, w)| g != w);
    let line = at.unwrap_or_else(|| got.lines().count().min(want.lines().count()));
    panic!(
        "{what}: line {line}\n pyn: {:?}\n git: {:?}",
        got.lines().nth(line),
        want.lines().nth(line)
    );
}

fn stage_change(pair: &Pair) {
    pair.both(|s| {
        fs::write(s.repo.join("a.txt"), "changed\n").unwrap();
        s.git(&["add", "a.txt"]);
    });
}

fn each_format(f: impl Fn(&str)) {
    for format in FORMATS {
        f(format);
    }
}

#[test]
fn commit_hooks_run_in_git_order_with_git_arguments_and_environment() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        stage_change(&pair);
        pair.same("sub", &["commit", "-m", "subject", "-s"], &[]);
        assert!(pair.p.log().contains("== post-commit"));
        assert!(pair.p.log().contains("GIT_PREFIX=sub/"), "{}", pair.p.log());
        assert!(pair.p.log().contains("GIT_INDEX_FILE=.git/index"));
        assert!(pair.p.log().contains("args: .git/COMMIT_EDITMSG message"));
    });
}

#[test]
fn commit_all_hooks_see_the_lock_file_index() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        pair.both(|s| fs::write(s.repo.join("a.txt"), "edited\n").unwrap());
        pair.same("", &["commit", "-a", "-m", "all"], &[]);
        assert!(pair.p.log().contains("index.lock"), "{}", pair.p.log());
    });
}

#[test]
fn message_sources_follow_the_flags() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        pair.both(|s| fs::write(s.repo.join("m.txt"), "from file\n").unwrap());
        pair.same("", &["commit", "--allow-empty", "-F", "m.txt"], &[]);
        pair.same("", &["commit", "--allow-empty", "-C", "HEAD~1"], &[]);
        pair.same(
            "",
            &["commit", "--amend", "--no-edit", "--allow-empty"],
            &[],
        );
        pair.same("", &["commit", "--amend", "-m", "again"], &[]);
        assert!(
            pair.p
                .log()
                .contains("args: .git/COMMIT_EDITMSG commit HEAD~1")
        );
        assert!(
            pair.p
                .log()
                .contains("args: .git/COMMIT_EDITMSG commit HEAD\n")
        );
        assert!(
            pair.p
                .log()
                .contains("== post-rewrite\narg0: .git/hooks/post-rewrite\nargs: amend")
        );
    });
}

#[test]
fn a_failing_hook_aborts_the_commit_like_git() {
    each_format(|format| {
        for failing in [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
        ] {
            let pair = Pair::new(format, &ALL);
            stage_change(&pair);
            let run = pair.same("", &["commit", "-m", "x"], &[("HOOKFAIL", failing)]);
            assert_eq!(run.code, i32::from(failing != "post-commit"), "{failing}");
        }
    });
}

#[test]
fn no_verify_skips_pre_commit_and_commit_msg_only() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        stage_change(&pair);
        let run = pair.same(
            "",
            &["commit", "--no-verify", "-m", "x"],
            &[("HOOKFAIL", "pre-commit commit-msg")],
        );
        assert_eq!(run.code, 0);
        let log = pair.p.log();
        assert!(!log.contains("== pre-commit") && !log.contains("== commit-msg"));
        assert!(log.contains("== prepare-commit-msg") && log.contains("== post-commit"));
    });
}

#[test]
fn message_hooks_can_rewrite_the_message() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        stage_change(&pair);
        pair.same("", &["commit", "-m", "x"], &[("HOOKEDIT", "1")]);
        let message = pair.p.git(&["log", "-1", "--format=%B"]);
        assert!(
            message.contains("edited by prepare-commit-msg"),
            "{message}"
        );
        assert!(message.contains("edited by commit-msg"), "{message}");
    });
}

#[test]
fn nothing_to_commit_still_runs_pre_commit_and_nothing_after() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        let run = pair.same("", &["commit", "-m", "x"], &[]);
        assert_eq!(run.code, 1);
        let log = pair.p.log();
        assert!(log.contains("== pre-commit") && !log.contains("== commit-msg"));
    });
}

#[test]
fn merge_runs_the_merge_hooks() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        pair.both(|s| {
            s.git(&["switch", "-q", "side"]);
            fs::write(s.repo.join("c.txt"), "side\n").unwrap();
            s.git(&["add", "c.txt"]);
            s.git(&["commit", "-q", "-m", "side work"]);
            s.git(&["switch", "-q", "main"]);
            fs::write(s.repo.join("d.txt"), "main\n").unwrap();
            s.git(&["add", "d.txt"]);
            s.git(&["commit", "-q", "-m", "main work"]);
        });
        pair.same("", &["merge", "side", "-m", "joined"], &[]);
        let log = pair.p.log();
        for hook in [
            "pre-merge-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-merge",
        ] {
            assert!(log.contains(&format!("== {hook}")), "{hook}: {log}");
        }
        assert!(log.contains("args: .git/MERGE_MSG merge"), "{log}");
        assert!(!log.contains("== post-commit"));
    });
}

#[test]
fn merge_vetoes_and_no_verify() {
    each_format(|format| {
        for (failing, extra) in [
            ("pre-merge-commit", None),
            ("prepare-commit-msg", None),
            ("commit-msg", None),
            ("pre-merge-commit commit-msg", Some("--no-verify")),
        ] {
            let pair = Pair::new(format, &ALL);
            pair.both(|s| {
                s.git(&["switch", "-q", "side"]);
                fs::write(s.repo.join("c.txt"), "side\n").unwrap();
                s.git(&["add", "c.txt"]);
                s.git(&["commit", "-q", "-m", "side work"]);
                s.git(&["switch", "-q", "main"]);
                fs::write(s.repo.join("d.txt"), "main\n").unwrap();
                s.git(&["add", "d.txt"]);
                s.git(&["commit", "-q", "-m", "main work"]);
            });
            let mut args = vec!["merge", "side", "-m", "joined"];
            args.extend(extra);
            let run = pair.same("", &args, &[("HOOKFAIL", failing)]);
            assert_eq!(run.code, i32::from(extra.is_none()), "{failing}");
        }
    });
}

#[test]
fn fast_forward_runs_post_merge_only() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        pair.both(|s| {
            s.git(&["switch", "-q", "side"]);
            fs::write(s.repo.join("c.txt"), "side\n").unwrap();
            s.git(&["add", "c.txt"]);
            s.git(&["commit", "-q", "-m", "side work"]);
            s.git(&["switch", "-q", "main"]);
        });
        pair.same("", &["merge", "side"], &[("HOOKFAIL", "post-merge")]);
        let log = pair.p.log();
        assert!(
            log.contains("== post-merge\narg0: .git/hooks/post-merge\nargs: 0"),
            "{log}"
        );
        assert!(!log.contains("== pre-merge-commit"));
    });
}

#[test]
fn pre_push_gets_the_remote_and_one_line_per_ref() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        pair.same("", &["push", "o", "main", "side:other"], &[]);
        pair.both(|s| {
            s.git(&["tag", "t1"]);
        });
        pair.same(
            "",
            &[
                "push",
                "o",
                "main",
                "t1",
                "HEAD:refs/heads/h",
                "HEAD~1:refs/heads/h2",
            ],
            &[],
        );
        pair.same("", &["push", "o", "main"], &[]);
        pair.same("", &["push", "o", "--delete", "other"], &[]);
        pair.same("", &["push", "--dry-run", "o", "main:dry"], &[]);
        // git 2.56 also updates the tracking refs of a remote whose URL is pushed to directly
        pair.same_with("", &["push", "../remote.git", "main:direct"], &[], false);
        let log = pair.p.log();
        assert!(log.contains("args: o ../remote.git"), "{log}");
        assert!(log.contains("(delete) "), "{log}");
        assert!(log.contains("HEAD~1 "), "{log}");
    });
}

#[test]
fn pre_push_failure_and_no_verify() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        let run = pair.same("", &["push", "o", "main"], &[("HOOKFAIL", "pre-push")]);
        assert_eq!(run.code, 1);
        assert!(pair.p.git(&["ls-remote", "../remote.git"]).is_empty());
        let run = pair.same(
            "",
            &["push", "--no-verify", "o", "main"],
            &[("HOOKFAIL", "pre-push")],
        );
        assert_eq!(run.code, 0);
    });
}

#[test]
fn rejected_refs_are_left_out_of_pre_push_input() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        pair.same("", &["push", "o", "main"], &[]);
        pair.both(|s| {
            s.git(&["reset", "-q", "--hard", "HEAD~1"]);
            fs::write(s.repo.join("z.txt"), "z\n").unwrap();
            s.git(&["add", "z.txt"]);
            s.git(&["commit", "-q", "-m", "diverge"]);
        });
        let run = pair.same("", &["push", "o", "main", "side:fresh"], &[]);
        assert_eq!(run.code, 1);
        pair.same("", &["push", "-f", "o", "main"], &[]);
    });
}

#[test]
fn post_checkout_runs_for_switches_and_file_checkouts() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        pair.same("sub", &["switch", "side"], &[]);
        pair.same("", &["switch", "-c", "fresh"], &[]);
        pair.same("", &["switch", "--detach", "main"], &[]);
        pair.same("", &["checkout", "side"], &[]);
        pair.both(|s| fs::write(s.repo.join("a.txt"), "dirty\n").unwrap());
        pair.same("", &["checkout", "--", "a.txt"], &[]);
        pair.both(|s| fs::write(s.repo.join("a.txt"), "dirty\n").unwrap());
        pair.same("", &["restore", "a.txt"], &[]);
        pair.same(
            "",
            &["restore", "--source", "HEAD~1", "--staged", "b.txt"],
            &[],
        );
        let log = pair.p.log();
        assert!(log.contains("== post-checkout"));
        assert_eq!(log.matches("== post-checkout").count(), 7, "{log}");
    });
}

#[test]
fn a_failing_post_checkout_fails_the_command_after_the_switch() {
    each_format(|format| {
        let pair = Pair::new(format, &ALL);
        let run = pair.same("", &["switch", "side"], &[("HOOKFAIL", "post-checkout")]);
        assert_eq!(run.code, 1);
        assert!(
            pair.p
                .git(&["branch", "--show-current"])
                .starts_with("side")
        );
    });
}

#[test]
fn hooks_path_and_discovery_follow_git() {
    each_format(|format| {
        for setup in ["relative", "absolute", "not-executable", "sample"] {
            let pair = Pair::new(format, &[]);
            stage_change(&pair);
            pair.both(|s| match setup {
                "relative" => {
                    write_hook(&s.repo, "pre-commit");
                    let moved = s.repo.join("my-hooks");
                    fs::rename(s.repo.join(".git/hooks"), &moved).unwrap();
                    s.git(&["config", "core.hooksPath", "my-hooks"]);
                }
                "absolute" => {
                    write_hook(&s.repo, "pre-commit");
                    let moved = s.root.join("abs-hooks");
                    fs::rename(s.repo.join(".git/hooks"), &moved).unwrap();
                    s.git(&["config", "core.hooksPath", moved.to_str().unwrap()]);
                }
                "not-executable" => {
                    write_hook(&s.repo, "pre-commit");
                    let path = s.repo.join(".git/hooks/pre-commit");
                    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                _ => {
                    write_hook(&s.repo, "pre-commit");
                    let hooks = s.repo.join(".git/hooks");
                    fs::rename(hooks.join("pre-commit"), hooks.join("pre-commit.sample")).unwrap();
                }
            });
            let run = pair.same("", &["commit", "-m", "x"], &[]);
            assert_eq!(run.code, 0, "{setup}");
            let ran = pair.p.log().contains("== pre-commit");
            assert_eq!(ran, matches!(setup, "relative" | "absolute"), "{setup}");
        }
    });
}

#[test]
fn hooks_can_be_switched_off_with_a_pyn_setting() {
    let pair = Pair::new("sha1", &ALL);
    stage_change(&pair);
    pair.p.git(&["config", "pyn.hooks", "false"]);
    let run = pair.p.run_in(
        &pair.p.repo,
        &["commit", "-m", "x"],
        &[("HOOKFAIL", "pre-commit")],
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(
        run.stderr.contains("pyn does not run git hooks"),
        "{}",
        run.stderr
    );
    assert!(pair.p.log().is_empty());
}

#[test]
fn a_clone_gets_no_hooks_from_its_source() {
    let pair = Pair::new("sha1", &ALL);
    let dest = pair.p.root.join("copy");
    let run = pair.p.run_in(
        &pair.p.root,
        &[
            "clone",
            pair.p.repo.to_str().unwrap(),
            dest.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(run.code, 0, "{}", run.stderr);
    for h in ALL {
        assert!(!dest.join(".git/hooks").join(h).exists(), "{h}");
    }
}

/// Both repositories diverged on `side` and `main`, with hooks installed and an editor script outside them.
fn divergent(format: &str, hooks: &[&str]) -> (Pair, String) {
    let pair = Pair::new(format, hooks);
    pair.both(|s| {
        s.git(&["switch", "-q", "side"]);
        fs::write(s.repo.join("c.txt"), "side\n").unwrap();
        s.git(&["add", "c.txt"]);
        s.git(&["commit", "-q", "-m", "side work"]);
        s.git(&["switch", "-q", "main"]);
        fs::write(s.repo.join("d.txt"), "main\n").unwrap();
        s.git(&["add", "d.txt"]);
        s.git(&["commit", "-q", "-m", "main work"]);
    });
    let editor = pair._tmp.path().join("editor.sh");
    fs::write(&editor, EDITOR).unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
    let editor = editor.to_string_lossy().into_owned();
    (pair, editor)
}

/// Like `same`, and the merge state files must agree too.
fn same_merge(pair: &Pair, args: &[&str], env: &[(&str, &str)]) -> Run {
    let run = pair.same("", args, env);
    first_difference(
        &pair.p.merge_files(),
        &pair.o.merge_files(),
        &format!("{args:?} env {env:?}: merge state files"),
    );
    run
}

#[test]
fn merge_edit_flags_and_defaults_follow_git() {
    each_format(|format| {
        let cases: &[EditCase] = &[
            (&["merge", "side"], &[], false),
            (&["merge", "side", "-m", "joined"], &[], false),
            (&["merge", "side", "-e", "-m", "joined"], &[], true),
            (&["merge", "side", "-e"], &[], true),
            (&["merge", "side", "--edit"], &[], true),
            (&["merge", "side", "--no-edit"], &[], false),
            (&["merge", "side", "--no-edit", "-m", "joined"], &[], false),
            (&["merge", "side", "-e", "--no-edit"], &[], false),
            (&["merge", "side", "--no-edit", "-e"], &[], true),
            (&["merge", "side"], &[("GIT_MERGE_AUTOEDIT", "yes")], true),
            (&["merge", "side"], &[("GIT_MERGE_AUTOEDIT", "no")], false),
            (&["merge", "side"], &[("GIT_MERGE_AUTOEDIT", "")], false),
            (
                &["merge", "side", "-m", "j"],
                &[("GIT_MERGE_AUTOEDIT", "yes")],
                false,
            ),
            (
                &["merge", "side", "-e"],
                &[("GIT_MERGE_AUTOEDIT", "no")],
                true,
            ),
            (
                &["merge", "side", "--no-edit"],
                &[("GIT_MERGE_AUTOEDIT", "yes")],
                false,
            ),
            (
                &["merge", "side", "--no-ff", "-e", "--no-verify"],
                &[],
                true,
            ),
        ];
        for hooks in [&[][..], &ALL[..]] {
            for (args, env, edits) in cases {
                let (pair, editor) = divergent(format, hooks);
                let mut env = env.to_vec();
                env.push(("GIT_EDITOR", &editor));
                same_merge(&pair, args, &env);
                let ran = pair.p.log().contains("== editor");
                assert_eq!(ran, *edits, "{args:?} {env:?}: editor ran");
                if *edits {
                    let text = pair.p.git(&["log", "-1", "--format=%B"]);
                    assert!(text.contains("edited by editor"), "{text}");
                }
            }
        }
    });
}

#[test]
fn merge_edit_with_a_bad_autoedit_value_is_fatal() {
    each_format(|format| {
        let (pair, editor) = divergent(format, &[]);
        let env = [
            ("GIT_EDITOR", editor.as_str()),
            ("GIT_MERGE_AUTOEDIT", "bogus"),
        ];
        let run = same_merge(&pair, &["merge", "side"], &env);
        assert_eq!(run.code, 128);
        same_merge(&pair, &["merge", "side", "-m", "joined"], &env);
        same_merge(&pair, &["merge", "side", "--no-edit"], &env);
    });
}

#[test]
fn merge_edit_hooks_see_the_editor_and_its_message() {
    each_format(|format| {
        let (pair, editor) = divergent(format, &ALL);
        let env = [("GIT_EDITOR", editor.as_str()), ("HOOKEDIT", "1")];
        same_merge(&pair, &["merge", "side", "-e"], &env);
        let log = pair.p.log();
        assert!(log.contains("args: .git/MERGE_MSG merge"), "{log}");
        assert!(
            log.contains("GIT_EDITOR=<root>") || log.contains("editor.sh"),
            "{log}"
        );
    });
}

#[test]
fn merge_edit_aborts_leave_the_merge_in_progress() {
    each_format(|format| {
        for (mode, failing) in [
            ("empty", ""),
            ("fail", ""),
            ("", "commit-msg"),
            ("", "prepare-commit-msg"),
            ("", "pre-merge-commit"),
        ] {
            for hooks in [&[][..], &ALL[..]] {
                let (pair, editor) = divergent(format, hooks);
                let env = [
                    ("GIT_EDITOR", editor.as_str()),
                    ("EDITMODE", mode),
                    ("HOOKFAIL", failing),
                    ("HOOKEDIT", "1"),
                ];
                let run = same_merge(&pair, &["merge", "side", "-e"], &env);
                if hooks.is_empty() {
                    assert_eq!(run.code, i32::from(!mode.is_empty()), "{mode}");
                }
            }
        }
    });
}

#[test]
fn merge_edit_without_a_commit_or_with_a_fast_forward_opens_no_editor() {
    each_format(|format| {
        let (pair, editor) = divergent(format, &ALL);
        let env = [("GIT_EDITOR", editor.as_str())];
        same_merge(&pair, &["merge", "side", "-e", "--no-commit"], &env);
        assert!(!pair.p.log().contains("== editor"));
        let pair = Pair::new(format, &ALL);
        pair.both(|s| {
            s.git(&["switch", "-q", "side"]);
            fs::write(s.repo.join("c.txt"), "side\n").unwrap();
            s.git(&["add", "c.txt"]);
            s.git(&["commit", "-q", "-m", "side work"]);
            s.git(&["switch", "-q", "main"]);
        });
        same_merge(&pair, &["merge", "side", "-e"], &env);
        assert!(!pair.p.log().contains("== editor"));
    });
}

#[test]
fn merge_editor_is_chosen_like_commits() {
    each_format(|format| {
        let (pair, editor) = divergent(format, &[]);
        pair.both(|s| {
            s.git(&["config", "core.editor", &editor]);
        });
        same_merge(
            &pair,
            &["merge", "side", "-e"],
            &[("GIT_EDITOR", "<unset>")],
        );
        assert!(pair.p.log().contains("== editor"));
    });
}

//! Client hooks on a git repository: the same hook script under real git and under pyn must see the same arguments,
//! standard input, working directory and environment and must cause the same abort or continue. Real git is the oracle in
//! these tests only; wording is never compared.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

/// Records what the hook saw; `HOOKFAIL` names the hooks (or `reference-transaction:<state>`) that exit 1, `HOOKEDIT` makes the message hooks edit the message.
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
  pre-push|post-rewrite|reference-transaction) echo "stdin:"; cat ;;
  prepare-commit-msg|commit-msg|applypatch-msg) [ -n "$NOMSG" ] || echo "msg: $(cat "$1")" ;;
esac
echo "out-of-band"
} | sed "s#$HOOKROOT#<root>#g" >> "$HOOKLOG"
echo "stdout of $n"
case "$n" in
  prepare-commit-msg|commit-msg|applypatch-msg) [ -n "$HOOKEDIT" ] && echo "edited by $n" >> "$1" ;;
esac
case " $HOOKFAIL " in *" $n "*|*" $n:$1 "*) exit 1 ;; esac
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

const REPLAY: [&str; 9] = [
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "pre-rebase",
    "post-rewrite",
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
];

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
    /// Lets git's `gc.auto` come from the repository rather than the harness's `-c gc.auto=0`.
    own_gc: bool,
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
            let mut c = git(dir);
            if self.own_gc {
                c.args(["-c", "gc.auto=1", "-c", "gc.autoDetach=false"]);
            }
            c
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
        let rebasing = stray("rebase-merge");
        for name in [
            "rebase-merge",
            "rebase-apply",
            "sequencer",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
        ] {
            if !rebasing || !name.ends_with("_HEAD") {
                out += &format!("{name}: {}\n", stray(name));
            }
        }
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
            Side {
                root,
                repo,
                pyn,
                own_gc: false,
            }
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

    /// Same exit status and repository state; the hooks' log is not compared.
    fn same_outcome(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
        let want = self.o.run_in(&self.o.repo, args, env);
        let got = self.p.run_in(&self.p.repo, args, env);
        assert_eq!(
            oracle::status_class(got.code),
            oracle::status_class(want.code),
            "{args:?}: {}",
            got.stderr
        );
        first_difference(
            &self.p.state(),
            &self.o.state(),
            &format!("{args:?}: repository state"),
        );
        got
    }

    /// [`Pair::same`]; with `tracking` false the remote-tracking refs are left out of the state.
    fn same_with(&self, cwd: &str, args: &[&str], env: &[(&str, &str)], tracking: bool) -> Run {
        let want = self.o.run_in(&self.o.repo.join(cwd), args, env);
        let got = self.p.run_in(&self.p.repo.join(cwd), args, env);
        let ctx = format!("{args:?} in {cwd:?} env {env:?}");
        assert_eq!(
            oracle::status_class(got.code),
            oracle::status_class(want.code),
            "{ctx}: exit status\npyn: {}\ngit: {}",
            got.stderr,
            want.stderr
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

/// A hook log with the `reference-transaction` runs reduced to the set of `<state> <old> <new> <ref>` facts: git
/// versions differ in the `preparing` state, in how updates are grouped and ordered, and in reporting `HEAD`.
fn without_preparing(log: &str) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for line in log.lines() {
        if line.starts_with("== ") || blocks.is_empty() {
            blocks.push(String::new());
        }
        let last = blocks.last_mut().expect("a block was just pushed");
        last.push_str(line);
        last.push('\n');
    }
    let mut facts = std::collections::BTreeSet::new();
    let mut rest = String::new();
    for block in &blocks {
        if !block.starts_with("== reference-transaction\n") {
            rest.push_str(block);
            continue;
        }
        let state = block
            .lines()
            .find_map(|l| l.strip_prefix("args: "))
            .unwrap_or_default();
        if state == "preparing" {
            continue;
        }
        let updates = block.split("stdin:\n").nth(1).unwrap_or_default();
        for update in updates.lines().take_while(|l| *l != "out-of-band") {
            if !update.ends_with(" HEAD") {
                facts.insert(format!("ref-transaction {state} {update}\n"));
            }
        }
    }
    rest + &facts.into_iter().collect::<String>()
}

/// Fails naming the first line where pyn's text departs from git's.
fn first_difference(got: &str, want: &str, what: &str) {
    let (got, want) = (without_preparing(got), without_preparing(want));
    let (got, want) = (got.as_str(), want.as_str());
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

const REF_TX: [&str; 1] = ["reference-transaction"];

/// The refs the hook was told about; git's old id for a deletion differs by version, so only the names are compared.
fn touched(log: &str) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut in_stdin = false;
    for line in log.lines() {
        if line.starts_with("== ") {
            in_stdin = false;
        } else if line == "stdin:" {
            in_stdin = true;
        } else if in_stdin
            && let Some(name) = line.rsplit(' ').next().filter(|_| line.contains(' '))
        {
            out.insert(name.to_string());
        }
    }
    out
}

#[test]
fn reference_transaction_sees_the_ref_updates_git_makes() {
    each_format(|format| {
        let pair = Pair::new(format, &REF_TX);
        stage_change(&pair);
        pair.same("sub", &["commit", "-m", "subject"], &[]);
        pair.same("", &["commit", "--amend", "-m", "amended"], &[]);
        pair.same("", &["branch", "fresh"], &[]);
        pair.same("", &["tag", "light"], &[]);
        pair.same("", &["tag", "-a", "-m", "note", "heavy"], &[]);
        pair.same("", &["switch", "-c", "moved"], &[]);
        pair.same("", &["switch", "--detach", "main"], &[]);
        pair.same("", &["switch", "main"], &[]);
        pair.same("", &["reset", "--hard", "HEAD~1"], &[]);
        pair.same("", &["merge", "side"], &[]);
        pair.same("", &["push", "o", "main", "side"], &[]);
        let log = pair.p.log();
        assert!(log.contains("args: prepared"), "{log}");
        assert!(log.contains("args: committed"), "{log}");
    });
}

#[test]
fn reference_transaction_sees_fetch_pull_and_forced_resets() {
    each_format(|format| {
        let pair = Pair::new(format, &REF_TX);
        publish(&pair);
        pair.both(|s| {
            s.git(&["update-ref", "refs/remotes/o/main", "HEAD~1"]);
            s.git(&["update-ref", "-d", "refs/remotes/o/side"]);
        });
        pair.same_with("", &["fetch", "o"], &[], false);
        pair.both(|s| {
            s.git(&["update-ref", "refs/remotes/o/main", "HEAD~1"]);
        });
        pair.same_with("", &["pull", "o", "main"], &[], false);
        pair.same_with("", &["branch", "-f", "side", "HEAD~1"], &[], false);
        pair.same_with("", &["switch", "-C", "side"], &[], false);
        pair.same_with("", &["tag", "-f", "-a", "-m", "again", "heavy"], &[], false);
        pair.same_with("", &["merge", "--no-ff", "main"], &[], false);
        pair.same_with("", &["checkout", "-b", "viacheckout"], &[], false);
        let log = pair.p.log();
        assert!(log.contains("refs/remotes/o/main"), "{log}");
    });
}

#[test]
fn reference_transaction_for_deletions_ends_in_the_same_state() {
    each_format(|format| {
        let pair = Pair::new(format, &REF_TX);
        pair.both(|s| {
            s.git(&["tag", "gone"]);
        });
        let want = pair.o.run_in(&pair.o.repo, &["branch", "-D", "side"], &[]);
        let got = pair.p.run_in(&pair.p.repo, &["branch", "-D", "side"], &[]);
        assert_eq!((want.code, got.code), (0, 0), "{}", got.stderr);
        for args in [&["tag", "-d", "gone"][..], &["push", "o", "main"]] {
            pair.o.run_in(&pair.o.repo, args, &[]);
            pair.p.run_in(&pair.p.repo, args, &[]);
        }
        first_difference(
            &format!("{:?}", touched(&pair.p.log())),
            &format!("{:?}", touched(&pair.o.log())),
            "refs the hook saw",
        );
        assert!(pair.p.log().contains("args: committed"));
        first_difference(&pair.p.state(), &pair.o.state(), "repository state");
    });
}

#[test]
fn a_refusing_reference_transaction_aborts_the_update() {
    each_format(|format| {
        let pair = Pair::new(format, &REF_TX);
        stage_change(&pair);
        let veto = [("HOOKFAIL", "reference-transaction:prepared")];
        pair.same("", &["branch", "refused"], &veto);
        pair.same("", &["tag", "refused"], &veto);
        pair.same("", &["commit", "-m", "refused"], &veto);
        pair.same("", &["reset", "--hard", "HEAD~1"], &veto);
        let aborted = pair.p.log().matches("args: aborted").count();
        assert!(aborted >= 4, "{aborted}");
        assert!(pair.p.git(&["branch", "--list", "refused"]).is_empty());
        let late = [("HOOKFAIL", "reference-transaction:committed")];
        let run = pair.same("", &["branch", "late"], &late);
        assert_eq!(run.code, 0, "{}", run.stderr);
    });
}

#[test]
fn reference_transaction_is_not_run_for_a_changed_expectation_or_a_failed_command() {
    each_format(|format| {
        let pair = Pair::new(format, &REF_TX);
        let run = pair.same("", &["branch", "side"], &[]);
        assert_ne!(run.code, 0);
        assert!(pair.p.log().is_empty(), "{}", pair.p.log());
    });
}

/// Pushes `main` to the bare remote of both sides so a clone has a commit to check out.
fn publish(pair: &Pair) {
    pair.both(|s| {
        s.git(&["push", "-q", "o", "main"]);
    });
}

fn clone_with_hooks(pair: &Pair, extra: &[&str], env: &[(&str, &str)]) -> (Run, Run) {
    let one = |s: &Side| {
        let hooks = format!("core.hooksPath={}", s.repo.join(".git/hooks").display());
        let mut args = vec!["clone", "-c", &hooks];
        args.extend_from_slice(extra);
        args.extend(["../remote.git", "../copy"]);
        s.run_in(&s.repo, &args, env)
    };
    (one(&pair.o), one(&pair.p))
}

#[test]
fn clone_runs_post_checkout_with_the_null_id_and_the_new_head() {
    each_format(|format| {
        for (extra, ran) in [
            (&[][..], true),
            (&["--no-checkout"], false),
            (&["--bare"], false),
        ] {
            let pair = Pair::new(format, &["post-checkout"]);
            publish(&pair);
            let (want, got) = clone_with_hooks(&pair, extra, &[]);
            assert_eq!(want.code, got.code, "{extra:?}: {}", got.stderr);
            first_difference(&pair.p.log(), &pair.o.log(), &format!("{extra:?}: hooks"));
            assert_eq!(pair.p.log().contains("== post-checkout"), ran, "{extra:?}");
        }
    });
}

#[test]
fn a_failing_post_checkout_fails_the_clone_after_it_finished() {
    let pair = Pair::new("sha1", &["post-checkout"]);
    publish(&pair);
    let (want, got) = clone_with_hooks(&pair, &[], &[("HOOKFAIL", "post-checkout")]);
    assert_eq!(
        oracle::status_class(got.code),
        oracle::status_class(want.code),
        "{}",
        got.stderr
    );
    assert_ne!(got.code, 0);
    assert!(pair.p.root.join("copy/a.txt").exists());
}

#[test]
fn worktree_add_runs_post_checkout_in_the_new_work_tree() {
    each_format(|format| {
        let cases: [&[&str]; 5] = [
            &["worktree", "add", "../wt1"],
            &["worktree", "add", "-b", "grown", "../wt2"],
            &["worktree", "add", "--detach", "../wt3", "side"],
            &["worktree", "add", "--no-checkout", "../wt4", "-b", "bare"],
            &["worktree", "add", "--orphan", "-b", "empty", "../wt5"],
        ];
        for args in cases {
            let pair = Pair::new(format, &["post-checkout"]);
            let run = pair.same("sub", args, &[]);
            assert_eq!(run.code, 0, "{args:?}: {}", run.stderr);
            let ran = pair.p.log().contains("== post-checkout");
            assert_eq!(
                ran,
                !args.contains(&"--no-checkout") && !args.contains(&"--orphan")
            );
        }
        let pair = Pair::new(format, &["post-checkout"]);
        let run = pair.same(
            "",
            &["worktree", "add", "../wt"],
            &[("HOOKFAIL", "post-checkout")],
        );
        assert_ne!(run.code, 0);
        assert!(pair.p.root.join("wt/a.txt").exists());
    });
}

/// Enough loose objects that `gc --auto` at a threshold of 1 has work to do.
fn bulk(pair: &Pair) {
    pair.both(|s| {
        for i in 0..1500 {
            fs::write(s.repo.join(format!("bulk{i}.txt")), format!("{i}\n")).unwrap();
        }
        s.git(&["add", "."]);
        s.git(&["config", "gc.auto", "1"]);
        s.git(&["config", "gc.autoDetach", "false"]);
    });
}

fn loose(s: &Side) -> String {
    let out = s.git(&["count-objects"]);
    out.split(' ').next().unwrap_or("").to_string()
}

#[test]
fn pre_auto_gc_runs_before_an_automatic_gc_and_can_skip_it() {
    let mut pair = Pair::new("sha1", &["pre-auto-gc"]);
    pair.o.own_gc = true;
    bulk(&pair);
    pair.same(
        "sub",
        &["gc", "--auto", "--quiet"],
        &[("HOOKFAIL", "pre-auto-gc")],
    );
    assert!(pair.p.log().contains("== pre-auto-gc"));
    assert_eq!(loose(&pair.p), loose(&pair.o));
    assert_ne!(loose(&pair.p), "0");
    pair.same("", &["gc", "--auto"], &[]);
    assert_eq!(loose(&pair.p), loose(&pair.o));
    assert_eq!(loose(&pair.p), "0");
    pair.same("", &["gc", "--auto"], &[]);
    pair.same("", &["gc", "--quiet"], &[]);
    assert_eq!(
        pair.p.log().matches("== pre-auto-gc").count(),
        2,
        "{}",
        pair.p.log()
    );
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

/// `side` has `s1.txt` and `s2.txt` on top of `two`; `main` has `m.txt` on top of it. With `conflict`, `side`'s last
/// commit and `main`'s both change `a.txt`.
fn forked(format: &str, conflict: bool) -> Pair {
    let pair = Pair::new(format, &REPLAY);
    let commit = |s: &Side, file: &str, text: &str| {
        fs::write(s.repo.join(file), text).unwrap();
        s.git(&["add", file]);
        s.git(&["commit", "-q", "-m", &format!("{file} work")]);
    };
    pair.both(|s| {
        s.git(&["switch", "-q", "side"]);
        commit(s, "s1.txt", "s1\n");
        commit(s, if conflict { "a.txt" } else { "s2.txt" }, "side\n");
        s.git(&["switch", "-q", "main"]);
        commit(s, if conflict { "a.txt" } else { "m.txt" }, "main\n");
    });
    pair
}

/// The names of the hooks that ran, in order, with their first argument where it tells them apart.
fn ran(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|l| l.strip_prefix("== "))
        .map(str::to_string)
        .collect()
}

fn sequence_editor(pair: &Pair) -> String {
    let path = pair._tmp.path().join("seq.sh");
    fs::write(&path, "#!/bin/sh\nsed -i \"$SEQEXPR\" \"$1\"\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn rebase_runs_pre_rebase_commit_hooks_and_post_rewrite() {
    each_format(|format| {
        let cases: &[&[&str]] = &[
            &["rebase", "main", "side"],
            &["rebase", "--onto", "main", "main~1", "side"],
            &["rebase", "-f", "main", "side"],
            &["rebase", "--root", "--onto", "main", "side"],
        ];
        for args in cases {
            let pair = forked(format, false);
            pair.same("", args, &[]);
            let log = pair.p.log();
            let order = ran(&log);
            assert_eq!(
                order.first().map(String::as_str),
                Some("pre-rebase"),
                "{args:?}"
            );
            assert_eq!(
                order.last().map(String::as_str),
                Some("post-rewrite"),
                "{args:?}"
            );
            assert!(
                log.contains("args: .git/COMMIT_EDITMSG message"),
                "{args:?}"
            );
        }
    });
}

#[test]
fn rebase_of_the_current_branch_and_the_up_to_date_cases() {
    each_format(|format| {
        let pair = forked(format, false);
        pair.both(|s| {
            s.git(&["switch", "-q", "side"]);
        });
        pair.same("", &["rebase", "main"], &[]);
        assert!(pair.p.log().contains("args: main\n"), "{}", pair.p.log());
        let pair = forked(format, false);
        pair.both(|s| {
            s.git(&["switch", "-q", "side"]);
        });
        pair.same_outcome(&["rebase", "main~1"], &[]);
        pair.same("", &["rebase", "-f", "main~1"], &[]);
        assert!(ran(&pair.p.log()).contains(&"pre-rebase".to_string()));
    });
}

#[test]
fn rebase_pre_rebase_veto_and_no_verify() {
    each_format(|format| {
        let pair = forked(format, false);
        let run = pair.same(
            "",
            &["rebase", "main", "side"],
            &[("HOOKFAIL", "pre-rebase")],
        );
        assert_ne!(run.code, 0);
        assert_eq!(ran(&pair.p.log()), ["pre-rebase"]);
        let pair = forked(format, false);
        let run = pair.same(
            "",
            &["rebase", "--no-verify", "main", "side"],
            &[("HOOKFAIL", "pre-rebase")],
        );
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(!ran(&pair.p.log()).contains(&"pre-rebase".to_string()));
    });
}

#[test]
fn rebase_hooks_edit_and_veto_the_replayed_message() {
    each_format(|format| {
        let pair = forked(format, false);
        pair.same("", &["rebase", "main", "side"], &[("HOOKEDIT", "1")]);
        let message = pair.p.git(&["log", "-1", "--format=%B"]);
        assert!(
            message.contains("edited by prepare-commit-msg"),
            "{message}"
        );
        assert!(!pair.p.log().contains("== commit-msg"));
        for failing in ["prepare-commit-msg", "post-commit", "post-rewrite"] {
            let pair = forked(format, false);
            pair.same("", &["rebase", "main", "side"], &[("HOOKFAIL", failing)]);
        }
    });
}

#[test]
fn rebase_squash_and_fixup_report_amends() {
    each_format(|format| {
        for expr in ["2s/^pick/fixup/", "2s/^pick/squash/"] {
            let pair = forked(format, false);
            let editor = sequence_editor(&pair);
            let run = pair.same_outcome(
                &["rebase", "-i", "main", "side"],
                &[
                    ("GIT_SEQUENCE_EDITOR", &editor),
                    ("SEQEXPR", expr),
                    ("NOMSG", "1"),
                ],
            );
            assert_eq!(run.code, 0, "{}", run.stderr);
            assert!(pair.p.log().contains("args: amend"), "{}", pair.p.log());
        }
    });
}

#[test]
fn rebase_continue_after_a_conflict_runs_the_message_hook() {
    each_format(|format| {
        let pair = forked(format, true);
        let stop = pair.same("", &["rebase", "main", "side"], &[]);
        assert_eq!(stop.code, 1);
        pair.both(|s| {
            fs::write(s.repo.join("a.txt"), "resolved\n").unwrap();
            s.git(&["add", "a.txt"]);
        });
        let run = pair.same("", &["rebase", "--continue"], &[("NOMSG", "1")]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert!(ran(&pair.p.log()).contains(&"post-rewrite".to_string()));
    });
}

#[test]
fn cherry_pick_and_revert_run_the_message_hook_and_post_commit_only() {
    each_format(|format| {
        let cases: &[&[&str]] = &[
            &["cherry-pick", "side"],
            &["cherry-pick", "-x", "-s", "side"],
            &["cherry-pick", "main..side"],
            &["revert", "--no-edit", "HEAD"],
            &["cherry-pick", "-n", "side"],
        ];
        for args in cases {
            let pair = forked(format, false);
            pair.same("", args, &[]);
            let order = ran(&pair.p.log());
            assert!(
                order
                    .iter()
                    .all(|h| h == "prepare-commit-msg" || h == "post-commit"),
                "{args:?} {order:?}"
            );
            assert_eq!(order.is_empty(), args.contains(&"-n"), "{args:?}");
        }
    });
}

#[test]
fn cherry_pick_hook_failures_and_edits() {
    each_format(|format| {
        for (cmd, failing) in [
            (&["cherry-pick", "main..side"][..], "prepare-commit-msg"),
            (&["revert", "--no-edit", "HEAD"], "prepare-commit-msg"),
            (&["cherry-pick", "side"], "post-commit"),
        ] {
            let pair = forked(format, false);
            let run = pair.same("", cmd, &[("HOOKFAIL", failing)]);
            assert_eq!(
                run.code,
                i32::from(failing != "post-commit") * run.code,
                "{cmd:?}"
            );
        }
        let pair = forked(format, false);
        pair.same("", &["cherry-pick", "side"], &[("HOOKEDIT", "1")]);
        assert!(
            pair.p
                .git(&["log", "-1", "--format=%B"])
                .contains("edited by prepare-commit-msg")
        );
    });
}

#[test]
fn cherry_pick_continue_verifies_like_commit() {
    each_format(|format| {
        let pair = forked(format, true);
        let stop = pair.same("", &["cherry-pick", "side"], &[]);
        assert_eq!(stop.code, 1);
        pair.both(|s| {
            fs::write(s.repo.join("a.txt"), "resolved\n").unwrap();
            s.git(&["add", "a.txt"]);
        });
        let run = pair.same("", &["cherry-pick", "--continue"], &[("NOMSG", "1")]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert_eq!(
            ran(&pair.p.log()),
            [
                "pre-commit",
                "prepare-commit-msg",
                "commit-msg",
                "post-commit"
            ]
        );
        assert!(pair.p.log().contains("args: .git/COMMIT_EDITMSG merge"));
    });
}

fn mailbox(pair: &Pair) -> String {
    pair.both(|s| {
        let out = git(&s.repo)
            .args(["format-patch", "-2", "side", "--stdout"])
            .output()
            .unwrap();
        fs::write(s.root.join("series.mbox"), out.stdout).unwrap();
    });
    "series.mbox".to_string()
}

#[test]
fn am_runs_the_apply_hooks() {
    each_format(|format| {
        let pair = forked(format, false);
        let mbox = mailbox(&pair);
        let path_o = pair.o.root.join(&mbox);
        let path_p = pair.p.root.join(&mbox);
        let run_o = pair
            .o
            .run_in(&pair.o.repo, &["am", path_o.to_str().unwrap()], &[]);
        let run_p = pair
            .p
            .run_in(&pair.p.repo, &["am", path_p.to_str().unwrap()], &[]);
        assert_eq!(
            oracle::status_class(run_o.code),
            oracle::status_class(run_p.code),
            "{}",
            run_p.stderr
        );
        first_difference(&pair.p.log(), &pair.o.log(), "am hooks");
        first_difference(&pair.p.state(), &pair.o.state(), "am state");
        assert_eq!(
            ran(&pair.p.log()),
            ["applypatch-msg", "pre-applypatch", "post-applypatch"].repeat(2)
        );
    });
}

/// Runs `am` with `args` plus the mailbox on both sides; status, hook records and state must agree.
fn same_am(pair: &Pair, args: &[&str], mbox: Option<&str>, env: &[(&str, &str)]) -> Run {
    let run_on = |s: &Side| {
        let file = mbox.map(|m| s.root.join(m).to_string_lossy().into_owned());
        let mut all = vec!["am"];
        all.extend_from_slice(args);
        all.extend(file.as_deref());
        s.run_in(&s.repo, &all, env)
    };
    let want = run_on(&pair.o);
    let got = run_on(&pair.p);
    let ctx = format!("am {args:?} env {env:?}");
    assert_eq!(
        oracle::status_class(got.code),
        oracle::status_class(want.code),
        "{ctx}: exit status\npyn: {}",
        got.stderr
    );
    first_difference(&pair.p.log(), &pair.o.log(), &format!("{ctx}: hooks"));
    first_difference(&pair.p.state(), &pair.o.state(), &format!("{ctx}: state"));
    got
}

#[test]
fn am_hooks_veto_skip_and_resume_like_git() {
    each_format(|format| {
        let pair = forked(format, false);
        let mbox = mailbox(&pair);
        let run = same_am(&pair, &["-n"], Some(&mbox), &[]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        assert_eq!(ran(&pair.p.log()), ["post-applypatch"; 2]);
        for failing in ["applypatch-msg", "pre-applypatch", "post-applypatch"] {
            let pair = forked(format, false);
            let mbox = mailbox(&pair);
            let run = same_am(&pair, &[], Some(&mbox), &[("HOOKFAIL", failing)]);
            assert_eq!(
                run.code,
                i32::from(failing != "post-applypatch"),
                "{failing}"
            );
        }
        let pair = forked(format, false);
        let mbox = mailbox(&pair);
        same_am(&pair, &[], Some(&mbox), &[("HOOKFAIL", "pre-applypatch")]);
        same_am(&pair, &["--continue"], None, &[]);
        let pair = forked(format, false);
        let mbox = mailbox(&pair);
        same_am(&pair, &[], Some(&mbox), &[("HOOKFAIL", "pre-applypatch")]);
        same_am(&pair, &["--skip"], None, &[]);
        let pair = forked(format, false);
        let mbox = mailbox(&pair);
        same_am(&pair, &[], Some(&mbox), &[("HOOKEDIT", "1")]);
        assert!(
            pair.p
                .git(&["log", "-1", "--format=%B"])
                .contains("edited by applypatch-msg")
        );
    });
}

#[test]
fn revert_and_cherry_pick_stops_and_resumes_run_the_hooks_git_runs() {
    each_format(|format| {
        let pair = forked(format, true);
        pair.both(|s| {
            fs::write(s.repo.join("a.txt"), "later\n").unwrap();
            s.git(&["commit", "-q", "-am", "later"]);
        });
        let stop = pair.same("", &["revert", "--no-edit", "main~1"], &[]);
        assert_eq!(stop.code, 1);
        pair.both(|s| {
            fs::write(s.repo.join("a.txt"), "resolved\n").unwrap();
            s.git(&["add", "a.txt"]);
        });
        let run = pair.same("", &["revert", "--continue"], &[("NOMSG", "1")]);
        assert_eq!(run.code, 0, "{}", run.stderr);
        let pair = forked(format, true);
        pair.same("", &["cherry-pick", "side~1", "side"], &[]);
        pair.both(|s| {
            fs::write(s.repo.join("a.txt"), "resolved\n").unwrap();
            s.git(&["add", "a.txt"]);
        });
        pair.same(
            "",
            &["cherry-pick", "--continue"],
            &[("NOMSG", "1"), ("HOOKFAIL", "commit-msg")],
        );
        pair.same("", &["cherry-pick", "--continue"], &[("NOMSG", "1")]);
    });
}

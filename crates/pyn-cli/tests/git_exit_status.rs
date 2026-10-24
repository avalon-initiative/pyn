//! Exit statuses and stream use of `pyn` on a git repository, compared with real git's on a twin copy of the same
//! repository, for the error paths of the supported commands. Real git is the oracle in this test only and only
//! semantics are compared: the exit status, and whether standard output and standard error carry anything.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use common::gitfix::Repo;

#[derive(Clone, Copy, Debug, PartialEq)]
enum At {
    /// The scenario repository with staged, unstaged and untracked changes.
    Dirty,
    /// The merge repository: clean tree, branches that merge, conflict and fast-forward.
    Clean,
    /// A directory outside any repository.
    Outside,
}

struct Case {
    at: At,
    /// Real git commands run on both twins before the compared command.
    setup: &'static [&'static [&'static str]],
    args: Vec<&'static str>,
}

impl Case {
    fn id(&self) -> String {
        let setup: Vec<String> = self.setup.iter().map(|s| s.join(" ")).collect();
        let after = if setup.is_empty() {
            String::new()
        } else {
            format!(" (after git {})", setup.join(", git "))
        };
        format!("{:?}: {}{after}", self.at, self.args.join(" "))
    }
}

fn case(at: At, args: &'static str) -> Case {
    with(at, &[], args)
}

fn with(at: At, setup: &'static [&'static [&'static str]], args: &'static str) -> Case {
    Case {
        at,
        setup,
        args: args.split(' ').collect(),
    }
}

/// Cases that differ from git: the case id and the reason, either the compatibility table row that makes
/// it deliberate (`docs:`) or the gap that is not documented (`gap:`).
const DIVERGENT: &[(&str, &str)] = &[
    (
        "Clean: commit -m msg --allow-empty --dry-run",
        "docs: Daily set, `git commit --dry-run` and the other unsupported flags exit 129",
    ),
    (
        "Dirty: commit -a -m msg --only README.md",
        "docs: Daily set, `git commit --only` is unsupported (129)",
    ),
    (
        "Dirty: commit -m msg --amend --fixup=HEAD",
        "docs: Daily set, `git commit --fixup` is unsupported (129)",
    ),
    (
        "Dirty: commit -m msg nosuchpath",
        "docs: Daily set `git commit <path>...`, refused on a git home",
    ),
    (
        "Clean: rebase -s bogus main",
        "docs: Rebase differences, `-s` is unsupported (129)",
    ),
    (
        "Dirty: push origin nosuchbranch (after git remote add origin /nonexistent/pyn-oracle.git)",
        "docs: Transports and credentials, a local path that is not a repository fails on connect before refspecs are checked",
    ),
    (
        "Outside: clone --depth=0 /nonexistent/pyn-oracle.git",
        "docs: Daily set `git clone --depth` is unsupported (129)",
    ),
    (
        "Outside: clone --depth=x /nonexistent/pyn-oracle.git",
        "docs: Daily set `git clone --depth` is unsupported (129)",
    ),
    (
        "Outside: clone --bare --separate-git-dir=x /nonexistent/pyn-oracle.git",
        "docs: Daily set `git clone --separate-git-dir` is unsupported (129)",
    ),
    (
        "Dirty: fetch --depth=0 /nonexistent/pyn-oracle.git",
        "docs: Fetch on a git home, shallow flags are 129",
    ),
    (
        "Dirty: fetch --depth=x /nonexistent/pyn-oracle.git",
        "docs: Fetch on a git home, shallow flags are 129",
    ),
    (
        "Dirty: fetch --deepen=1 --unshallow /nonexistent/pyn-oracle.git",
        "docs: Fetch on a git home, shallow flags are 129",
    ),
    (
        "Dirty: push --mirror main (after git remote add origin /nonexistent/pyn-oracle.git)",
        "docs: Push on a git home, `--mirror` is unsupported (129)",
    ),
    (
        "Dirty: restore",
        "docs: Branches, switching and the work tree, `restore` with no path is 129 not 128",
    ),
    (
        "Dirty: config bad..key value",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: config -f /nonexistent/file --list",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: config --get nosuch.key",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: config nokey",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: config nosuch.key",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: config --remove-section nosuchsection",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: config --rename-section nosuchsection other",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: config --unset nosuch.key",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Outside: config --list --local",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Outside: config --local a.b c",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Outside: config --local a.b",
        "docs: Extras `git config`, pyn config has list/get/set only; other forms are 129",
    ),
    (
        "Dirty: checkout -b newb -B other",
        "docs: Conventions, `checkout -B` is not built, so the combination is 129 where git says 128",
    ),
    (
        "Dirty: status --porcelain=v9",
        "docs: Conventions, `status --porcelain` takes only v1, so any other version is 129 where git says 128 for an unknown one",
    ),
    (
        "Outside: diff",
        "docs: Conventions, `pyn diff` outside a repository asks for a server (128); git prints --no-index usage (129)",
    ),
    (
        "Dirty: log --bogus",
        "docs: Conventions, an option git lacks and one pyn does not build both exit 129, where git exits 128",
    ),
    (
        "Dirty: show --bogus",
        "docs: Conventions, an option git lacks and one pyn does not build both exit 129, where git exits 128",
    ),
    (
        "Dirty: reflog --bogus",
        "docs: Conventions, an option git lacks and one pyn does not build both exit 129, where git exits 128",
    ),
];

struct Run {
    code: i32,
    stdout: bool,
    stderr: bool,
}

fn run(bin: &str, args: &[&str], cwd: &Path, home: &Path, ceiling: &Path) -> Run {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .env("HOME", home)
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "Staging Author")
        .env("GIT_AUTHOR_EMAIL", "staging@example.com")
        .env("GIT_COMMITTER_NAME", "Staging Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0200")
        .env("GIT_COMMITTER_DATE", "1700000100 -0530")
        .env("PYN_CONFIG_DIR", home.join("pyn-config"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PYN_SERVER")
        .env_remove("PYN_USER")
        .env_remove("PYN_TOKEN")
        .env_remove("PYN_DIR")
        .env_remove("PYN_REPO");
    let out = cmd.output().unwrap();
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: !out.stdout.is_empty(),
        stderr: !out.stderr.is_empty(),
    }
}

/// What differs between git and pyn for this case, if anything.
fn compare(c: &Case) -> Option<String> {
    let twin = || {
        if c.at == At::Clean {
            Repo::merge("sha1")
        } else {
            Repo::new("sha1")
        }
    };
    let (oracle, subject) = (twin(), twin());
    let sides = [&oracle, &subject].map(|r| {
        let ceiling = r.home.parent().unwrap();
        for step in c.setup {
            let s = run("git", step, &r.main, &r.home, ceiling);
            assert!(s.code <= 1, "{}: setup git {step:?}", c.id());
        }
        let cwd = if c.at == At::Outside {
            &r.home
        } else {
            &r.main
        };
        run(
            if std::ptr::eq(r, &oracle) {
                "git"
            } else {
                env!("CARGO_BIN_EXE_pyn")
            },
            &c.args,
            cwd,
            &r.home,
            ceiling,
        )
    });
    let [git, ours] = sides;
    let mut diffs = Vec::new();
    if git.code != ours.code {
        diffs.push(format!("status git {} pyn {}", git.code, ours.code));
    }
    if git.stdout != ours.stdout {
        diffs.push(format!(
            "stdout used: git {} pyn {}",
            git.stdout, ours.stdout
        ));
    }
    if git.stderr != ours.stderr {
        diffs.push(format!(
            "stderr used: git {} pyn {}",
            git.stderr, ours.stderr
        ));
    }
    (!diffs.is_empty()).then(|| diffs.join(", "))
}

fn check(group: Vec<Case>) {
    let mut failures = Vec::new();
    for c in &group {
        let id = c.id();
        let listed = DIVERGENT.iter().any(|(i, _)| *i == id);
        match (compare(c), listed) {
            (Some(d), false) => failures.push(format!("{id}: {d}")),
            (None, true) => failures.push(format!("{id}: matches git, remove it from DIVERGENT")),
            _ => {}
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

use At::{Clean, Dirty, Outside};

fn groups() -> Vec<(&'static str, Vec<Case>)> {
    const ORIGIN: &[&[&str]] = &[&["remote", "add", "origin", "/nonexistent/pyn-oracle.git"]];
    const MERGING: &[&[&str]] = &[&["merge", "conflict"]];
    vec![
        (
            "status",
            vec![
                case(Dirty, "status --bogus"),
                case(Dirty, "status --porcelain=v9"),
                case(Dirty, "status -s --bogus"),
                case(Outside, "status"),
            ],
        ),
        (
            "diff",
            vec![
                case(Dirty, "diff --bogus"),
                case(Dirty, "diff nosuchrev"),
                case(Dirty, "diff nosuchrev --"),
                case(Dirty, "diff --cached nosuchrev"),
                case(Dirty, "diff HEAD nosuchrev"),
                case(Dirty, "diff -- nosuchpath"),
                case(Dirty, "diff nosuchpath"),
                case(Dirty, "diff --exit-code"),
                case(Dirty, "diff --quiet"),
                case(Clean, "diff --exit-code"),
                case(Clean, "diff --quiet HEAD"),
                case(Outside, "diff"),
            ],
        ),
        (
            "show",
            vec![
                case(Dirty, "show --bogus"),
                case(Dirty, "show nosuchrev"),
                case(Dirty, "show HEAD:nosuchpath"),
                case(Dirty, "show HEAD HEAD~1 nosuchrev"),
                case(Outside, "show HEAD"),
            ],
        ),
        (
            "log",
            vec![
                case(Dirty, "log --bogus"),
                case(Dirty, "log nosuchrev"),
                case(Dirty, "log HEAD..nosuchrev"),
                case(Dirty, "log -- nosuchpath"),
                case(Dirty, "log nosuchpath"),
                case(Dirty, "log --since"),
                case(Outside, "log"),
            ],
        ),
        (
            "add",
            vec![
                case(Dirty, "add --bogus"),
                case(Dirty, "add nosuchpath"),
                case(Dirty, "add -- nosuchpath"),
                case(Dirty, "add debug.out"),
                case(Dirty, "add ../outside"),
                case(Dirty, "add -u nosuchpath"),
                case(Dirty, "add"),
                case(Outside, "add file"),
            ],
        ),
        (
            "rm",
            vec![
                case(Dirty, "rm"),
                case(Dirty, "rm --bogus"),
                case(Dirty, "rm nosuchpath"),
                case(Dirty, "rm new.txt"),
                case(Dirty, "rm README.md"),
                case(Dirty, "rm staged.txt"),
                case(Dirty, "rm sub"),
                case(Dirty, "rm ../outside"),
                case(Outside, "rm file"),
            ],
        ),
        (
            "mv",
            vec![
                case(Dirty, "mv"),
                case(Dirty, "mv README.md"),
                case(Dirty, "mv --bogus a b"),
                case(Dirty, "mv nosuchpath dest"),
                case(Dirty, "mv README.md feature.txt"),
                case(Dirty, "mv new.txt moved.txt"),
                case(Dirty, "mv README.md nosuchdir/"),
                case(Dirty, "mv README.md ../outside"),
                case(Dirty, "mv src src/sub"),
                case(Outside, "mv a b"),
            ],
        ),
        (
            "restore",
            vec![
                case(Dirty, "restore"),
                case(Dirty, "restore --bogus"),
                case(Dirty, "restore nosuchpath"),
                case(Dirty, "restore --source=nosuchrev ."),
                case(
                    Dirty,
                    "restore --staged --worktree --source=HEAD nosuchpath",
                ),
                case(Dirty, "restore --ours README.md"),
                case(Dirty, "restore --staged --ours README.md"),
                case(Dirty, "restore ../outside"),
                case(Outside, "restore file"),
            ],
        ),
        (
            "commit",
            vec![
                case(Clean, "commit -m msg"),
                case(Clean, "commit --bogus"),
                case(Dirty, "commit -m"),
                case(Dirty, "commit --bogus"),
                case(Dirty, "commit -m msg -F nosuchfile"),
                case(Dirty, "commit -F nosuchfile"),
                case(Dirty, "commit -m msg nosuchpath"),
                case(Dirty, "commit -m msg --author=bogus"),
                case(Dirty, "commit -m msg --date=bogus"),
                case(Dirty, "commit -a -m msg --only README.md"),
                case(Dirty, "commit -m msg --amend --fixup=HEAD"),
                case(Clean, "commit --amend -m msg -C nosuchrev"),
                case(Clean, "commit -m msg --allow-empty --dry-run"),
                case(Outside, "commit -m msg"),
            ],
        ),
        (
            "branch",
            vec![
                case(Dirty, "branch --bogus"),
                case(Dirty, "branch main"),
                case(Dirty, "branch newb nosuchrev"),
                case(Dirty, "branch bad..name"),
                case(Dirty, "branch a b c"),
                case(Dirty, "branch -d nosuchbranch"),
                case(Dirty, "branch -D main"),
                case(Dirty, "branch -d feature"),
                case(Dirty, "branch -m nosuchbranch other"),
                case(Dirty, "branch -m main feature"),
                case(Dirty, "branch -c nosuchbranch other"),
                case(Dirty, "branch --set-upstream-to=nosuchbranch"),
                case(Dirty, "branch --unset-upstream"),
                case(Dirty, "branch --contains nosuchrev"),
                case(Dirty, "branch --show-current extra"),
                case(Outside, "branch"),
            ],
        ),
        (
            "switch",
            vec![
                case(Dirty, "switch"),
                case(Dirty, "switch --bogus"),
                case(Dirty, "switch nosuchbranch"),
                case(Dirty, "switch -c"),
                case(Dirty, "switch -c main"),
                case(Dirty, "switch -c newb nosuchrev"),
                case(Dirty, "switch feature"),
                case(Dirty, "switch --detach nosuchrev"),
                case(Dirty, "switch light"),
                case(Outside, "switch main"),
            ],
        ),
        (
            "checkout",
            vec![
                case(Dirty, "checkout --bogus"),
                case(Dirty, "checkout nosuchbranch"),
                case(Dirty, "checkout nosuchbranch --"),
                case(Dirty, "checkout -- nosuchpath"),
                case(Dirty, "checkout -b main"),
                case(Dirty, "checkout -b"),
                case(Dirty, "checkout -b newb nosuchrev"),
                case(Dirty, "checkout feature"),
                case(Dirty, "checkout -b newb -B other"),
                case(Dirty, "checkout HEAD nosuchpath"),
                case(Outside, "checkout main"),
            ],
        ),
        (
            "merge",
            vec![
                case(Dirty, "merge"),
                case(Dirty, "merge --bogus"),
                case(Dirty, "merge nosuchrev"),
                case(Dirty, "merge feature"),
                case(Dirty, "merge --abort"),
                case(Dirty, "merge --continue"),
                case(Clean, "merge conflict"),
                case(Clean, "merge --ff-only clean"),
                case(Clean, "merge --ff-only --no-ff clean"),
                case(Clean, "merge --no-ff --ff-only clean"),
                case(Clean, "merge -s bogus clean"),
                case(Clean, "merge -m"),
                with(Clean, MERGING, "merge clean"),
                with(Clean, MERGING, "merge --continue"),
                with(Clean, MERGING, "commit -m msg"),
                case(Outside, "merge main"),
            ],
        ),
        (
            "tag",
            vec![
                case(Dirty, "tag --bogus"),
                case(Dirty, "tag v1"),
                case(Dirty, "tag newtag nosuchrev"),
                case(Dirty, "tag bad..name"),
                case(Dirty, "tag -d nosuchtag"),
                case(Dirty, "tag -a newtag"),
                case(Dirty, "tag -m msg"),
                case(Dirty, "tag -l -d v1"),
                case(Dirty, "tag a b c"),
                case(Dirty, "tag -f"),
                case(Dirty, "tag -a"),
                case(Dirty, "tag -s"),
                case(Dirty, "tag -d"),
                case(Dirty, "tag -l -f"),
                case(Dirty, "tag -d -f v1"),
                case(Dirty, "tag -f newtag"),
                case(Outside, "tag"),
            ],
        ),
        (
            "stash",
            vec![
                case(Dirty, "stash --bogus"),
                case(Dirty, "stash bogus"),
                case(Dirty, "stash pop"),
                case(Clean, "stash"),
                case(Clean, "stash pop"),
                case(Clean, "stash apply"),
                case(Clean, "stash drop"),
                case(Clean, "stash show"),
                case(Clean, "stash branch newb"),
                case(Dirty, "stash branch"),
                case(Dirty, "stash apply stash@{5}"),
                case(Dirty, "stash push -m"),
                case(Dirty, "stash push --bogus"),
                case(Dirty, "stash push -- nosuchpath"),
                case(Dirty, "stash clear extra"),
                case(Outside, "stash"),
            ],
        ),
        (
            "remote",
            vec![
                case(Dirty, "remote --bogus"),
                case(Dirty, "remote bogus"),
                case(Dirty, "remote add"),
                case(Dirty, "remote add name"),
                case(Dirty, "remote add bad..name url"),
                with(Dirty, ORIGIN, "remote add origin url"),
                case(Dirty, "remote remove nosuchremote"),
                case(Dirty, "remote rename nosuchremote other"),
                case(Dirty, "remote rename nosuchremote"),
                case(Dirty, "remote get-url nosuchremote"),
                case(Dirty, "remote set-url nosuchremote url"),
                with(Dirty, ORIGIN, "remote rename origin origin"),
                case(Dirty, "remote show nosuchremote"),
                case(Dirty, "remote prune nosuchremote"),
                case(Dirty, "remote set-head nosuchremote -a"),
                case(Outside, "remote"),
            ],
        ),
        (
            "reset",
            vec![
                case(Dirty, "reset --bogus"),
                case(Dirty, "reset nosuchrev"),
                case(Dirty, "reset --hard nosuchrev"),
                case(Dirty, "reset -- nosuchpath"),
                case(Dirty, "reset --soft --hard"),
                case(Dirty, "reset --soft -- README.md"),
                case(Dirty, "reset --hard README.md"),
                case(Dirty, "reset --mixed --keep"),
                case(Dirty, "reset HEAD nosuchrev --"),
                case(Dirty, "reset a b c --"),
                case(Dirty, "reset ../outside"),
                case(Outside, "reset"),
            ],
        ),
        (
            "clean",
            vec![
                case(Dirty, "clean"),
                case(Dirty, "clean --bogus"),
                case(Dirty, "clean -f ../outside"),
                case(Dirty, "clean -n -f -- nosuchpath"),
                case(Dirty, "clean -f -X -x"),
                case(Dirty, "clean -f -e"),
                case(Dirty, "clean -fi -n"),
                case(Outside, "clean -f"),
            ],
        ),
        (
            "reflog",
            vec![
                case(Dirty, "reflog --bogus"),
                case(Dirty, "reflog nosuchrev"),
                case(Dirty, "reflog show nosuchrev"),
                case(Dirty, "reflog exists nosuchref"),
                case(Dirty, "reflog delete nosuchref@{0}"),
                case(Dirty, "reflog delete"),
                case(Dirty, "reflog expire --bogus"),
                case(Dirty, "reflog expire nosuchref"),
                case(Outside, "reflog"),
            ],
        ),
        (
            "rebase",
            vec![
                case(Dirty, "rebase --bogus"),
                case(Dirty, "rebase nosuchrev"),
                case(Dirty, "rebase"),
                case(Dirty, "rebase feature"),
                case(Dirty, "rebase --continue"),
                case(Dirty, "rebase --abort"),
                case(Dirty, "rebase --skip"),
                case(Dirty, "rebase --quit"),
                case(Dirty, "rebase --onto nosuchrev main"),
                case(Dirty, "rebase main nosuchbranch"),
                case(Clean, "rebase -s bogus main"),
                case(Clean, "rebase --abort --continue"),
                case(Outside, "rebase main"),
            ],
        ),
        (
            "cherry-pick",
            vec![
                case(Dirty, "cherry-pick"),
                case(Dirty, "cherry-pick --bogus"),
                case(Dirty, "cherry-pick nosuchrev"),
                case(Dirty, "cherry-pick feature"),
                case(Dirty, "cherry-pick --continue"),
                case(Dirty, "cherry-pick --abort"),
                case(Dirty, "cherry-pick --skip"),
                case(Clean, "cherry-pick HEAD"),
                case(Clean, "cherry-pick -m 1 HEAD"),
                case(Clean, "cherry-pick -m 9 clean"),
                case(Clean, "cherry-pick -x -e"),
                case(Clean, "cherry-pick --abort --continue"),
                case(Outside, "cherry-pick main"),
            ],
        ),
        (
            "revert",
            vec![
                case(Dirty, "revert"),
                case(Dirty, "revert --bogus"),
                case(Dirty, "revert nosuchrev"),
                case(Dirty, "revert HEAD"),
                case(Dirty, "revert --continue"),
                case(Dirty, "revert --abort"),
                case(Dirty, "revert --skip"),
                case(Clean, "revert -m 1 HEAD"),
                case(Clean, "revert --no-edit nosuchrev"),
                case(Clean, "revert --abort --continue"),
                case(Outside, "revert HEAD"),
            ],
        ),
        (
            "fetch",
            vec![
                case(Dirty, "fetch --bogus"),
                case(Dirty, "fetch"),
                case(Dirty, "fetch nosuchremote"),
                case(Dirty, "fetch /nonexistent/pyn-oracle.git"),
                case(Dirty, "fetch file:///nonexistent/pyn-oracle.git"),
                case(Dirty, "fetch http://127.0.0.1:1/x.git"),
                with(Dirty, ORIGIN, "fetch origin"),
                with(Dirty, ORIGIN, "fetch --all origin"),
                case(Dirty, "fetch --depth=x /nonexistent/pyn-oracle.git"),
                case(Dirty, "fetch --depth=0 /nonexistent/pyn-oracle.git"),
                case(
                    Dirty,
                    "fetch --deepen=1 --unshallow /nonexistent/pyn-oracle.git",
                ),
                case(Dirty, "fetch --all /nonexistent/pyn-oracle.git"),
                case(Outside, "fetch"),
            ],
        ),
        (
            "pull",
            vec![
                case(Dirty, "pull --bogus"),
                case(Dirty, "pull"),
                case(Dirty, "pull nosuchremote"),
                case(Dirty, "pull /nonexistent/pyn-oracle.git"),
                case(Dirty, "pull --ff-only --no-ff /nonexistent/pyn-oracle.git"),
                with(Dirty, ORIGIN, "pull"),
                with(Dirty, ORIGIN, "pull origin main"),
                case(Dirty, "pull --rebase=bogus /nonexistent/pyn-oracle.git"),
                case(Outside, "pull"),
            ],
        ),
        (
            "push",
            vec![
                case(Dirty, "push --bogus"),
                case(Dirty, "push"),
                case(Dirty, "push nosuchremote"),
                case(Dirty, "push /nonexistent/pyn-oracle.git main"),
                case(Dirty, "push file:///nonexistent/pyn-oracle.git main"),
                with(Dirty, ORIGIN, "push origin main"),
                with(Dirty, ORIGIN, "push origin nosuchbranch"),
                with(Dirty, ORIGIN, "push origin"),
                with(Dirty, ORIGIN, "push"),
                with(Dirty, ORIGIN, "push --all main"),
                with(Dirty, ORIGIN, "push --all --tags"),
                with(Dirty, ORIGIN, "push --delete"),
                with(Dirty, ORIGIN, "push --delete origin"),
                with(Dirty, ORIGIN, "push --mirror main"),
                with(Dirty, ORIGIN, "push -u"),
                with(Dirty, ORIGIN, "push origin bad..name"),
                case(Outside, "push"),
            ],
        ),
        (
            "clone",
            vec![
                case(Outside, "clone"),
                case(Outside, "clone --bogus"),
                case(Outside, "clone /nonexistent/pyn-oracle.git target"),
                case(Outside, "clone file:///nonexistent/pyn-oracle.git target"),
                case(Outside, "clone http://127.0.0.1:1/x.git target"),
                case(Outside, "clone a b c"),
                case(Outside, "clone --depth=x /nonexistent/pyn-oracle.git"),
                case(Outside, "clone --depth=0 /nonexistent/pyn-oracle.git"),
                case(
                    Outside,
                    "clone --bare --separate-git-dir=x /nonexistent/pyn-oracle.git",
                ),
                case(Outside, "clone -b"),
                case(Dirty, "clone ../main ../home"),
                case(Dirty, "clone . ."),
            ],
        ),
        (
            "config",
            vec![
                case(Dirty, "config"),
                case(Dirty, "config --bogus"),
                case(Dirty, "config nokey"),
                case(Dirty, "config nosuch.key"),
                case(Dirty, "config --get nosuch.key"),
                case(Dirty, "config --get"),
                case(Dirty, "config --unset nosuch.key"),
                case(Dirty, "config --unset"),
                case(Dirty, "config a.b c d e"),
                case(Dirty, "config --get-all"),
                case(Dirty, "config -f /nonexistent/file --list"),
                case(Dirty, "config --local --global a.b c"),
                case(Dirty, "config --list nosuch.key"),
                case(Dirty, "config bad..key value"),
                case(Dirty, "config --replace-all nosuch.key"),
                case(Dirty, "config --remove-section nosuchsection"),
                case(Dirty, "config --rename-section nosuchsection other"),
                case(Outside, "config --local a.b"),
                case(Outside, "config --local a.b c"),
                case(Outside, "config --list --local"),
            ],
        ),
        (
            "grep",
            vec![
                case(Dirty, "grep"),
                case(Dirty, "grep -e"),
                case(Dirty, "grep --bogus"),
                case(Dirty, "grep README"),
                case(Dirty, "grep zzzzzz"),
                case(Dirty, "grep -q README"),
                case(Dirty, "grep -l README"),
                case(Dirty, "grep README HEAD"),
                case(Dirty, "grep --cached README"),
                case(Dirty, "grep --cached README HEAD"),
                case(Dirty, "grep README nosuchpath"),
                case(Dirty, "grep README -- nosuchpath"),
                case(Dirty, "grep README nosuchrev --"),
                case(Dirty, "grep README -n"),
                case(Dirty, "grep -E ("),
                case(Dirty, "grep ("),
                case(Dirty, "grep --not"),
                case(Dirty, "grep -e README --and"),
                case(Dirty, "grep README ../outside"),
                case(Clean, "grep clean"),
                case(Clean, "grep -c clean HEAD"),
            ],
        ),
        (
            "patch-exchange",
            vec![
                case(Clean, "format-patch nosuchrev"),
                case(Clean, "format-patch --stdout -o out -1"),
                case(Clean, "format-patch --stdout -1"),
                case(Clean, "format-patch --stdout HEAD"),
                case(Clean, "format-patch --stdout nosuch..HEAD"),
                case(Outside, "format-patch --stdout -1"),
                case(Clean, "apply"),
                case(Clean, "apply nosuch.patch"),
                case(Clean, "apply --check"),
                case(Clean, "apply --allow-empty"),
                case(Clean, "apply --index --cached"),
                case(Outside, "apply nosuch.patch"),
                case(Clean, "am"),
                case(Clean, "am nosuch.mbox"),
                case(Clean, "am --continue"),
                case(Clean, "am --skip"),
                case(Clean, "am --abort"),
                case(Clean, "am --quit"),
                case(Clean, "am --show-current-patch"),
                case(Outside, "am --continue"),
            ],
        ),
    ]
}

macro_rules! group_tests {
    ($($name:ident),*) => {$(
        #[test]
        fn $name() {
            let want = stringify!($name).replace('_', "-");
            let (_, cases) = groups().into_iter().find(|(n, _)| *n == want).unwrap();
            check(cases);
        }
    )*};
}

group_tests!(
    status,
    diff,
    show,
    log,
    add,
    rm,
    mv,
    restore,
    commit,
    branch,
    switch,
    checkout,
    merge,
    tag,
    stash,
    remote,
    reset,
    clean,
    reflog,
    rebase,
    cherry_pick,
    revert,
    fetch,
    pull,
    push,
    clone,
    config,
    grep,
    patch_exchange
);

#[test]
fn divergent_ids_name_real_cases() {
    let ids: Vec<String> = groups()
        .iter()
        .flat_map(|(_, g)| g.iter().map(Case::id))
        .collect();
    for (id, _) in DIVERGENT {
        assert!(ids.contains(&id.to_string()), "{id} is not a case");
    }
}

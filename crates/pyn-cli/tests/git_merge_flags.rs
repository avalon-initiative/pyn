//! The message and surrounding options of `pyn merge` on a git repository (`--signoff`, `--log`, `-F`, `--cleanup`,
//! `--into-name`, `--allow-unrelated-histories`, `--autostash`, `--quit`, `--verify-signatures`, `merge.*`), compared with
//! the real `git merge` on a copy of the same repository: exit status class, `HEAD` (so the message), index, status,
//! the merge state files, stashes and the work tree. Standard output is not compared. Needs `git` on the PATH.

mod common;
#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::gitfix::{copy_tree, snapshot};
use oracle::status_class;

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

/// Two copies of one repository: one for git, one for pyn.
struct Pair {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
    editor: std::cell::RefCell<String>,
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn have_ssh_keygen() -> bool {
    Command::new("ssh-keygen")
        .arg("-?")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

impl Pair {
    fn new(format: &str, script: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let git = root.join("git");
        fs::create_dir_all(&git).unwrap();
        let pair = Self {
            _tmp: tmp,
            pyn: root.join("pyn"),
            editor: std::cell::RefCell::new("true".into()),
            git,
            home,
        };
        let init = format!(
            "git init -q -b main --object-format={format} .\n\
             git config gc.auto 0\ngit config maintenance.auto false\ngit config merge.conflictStyle diff3\n{script}"
        );
        let out = Command::new("sh")
            .args(["-e", "-c", &init])
            .current_dir(&pair.git)
            .env("HOME", &pair.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        copy_tree(&pair.git, &pair.pyn);
        pair
    }

    fn git_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn run(&self, pyn: bool, args: &[&str]) -> Output {
        let mut cmd = if pyn {
            let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
            c.current_dir(&self.pyn)
                .env("TZ", "UTC")
                .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
                .env_remove("PYN_SERVER")
                .env_remove("PYN_REPO");
            c
        } else {
            let mut c = Command::new("git");
            c.current_dir(&self.git).env("GIT_CONFIG_NOSYSTEM", "1");
            c
        };
        cmd.args(args)
            .env("HOME", &self.home)
            .env("GIT_EDITOR", &*self.editor.borrow())
            .envs(IDENTITY)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// A shell command run in both copies, with real git.
    fn both_sh(&self, script: &str) {
        for dir in [&self.git, &self.pyn] {
            let out = Command::new("sh")
                .args(["-e", "-c", script])
                .current_dir(dir)
                .env("HOME", &self.home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .envs(IDENTITY)
                .output()
                .unwrap();
            assert!(out.status.success(), "{script}: {}", text(&out.stderr));
        }
    }

    /// Everything a merge leaves behind that a person or git can see.
    fn state(&self, dir: &Path) -> Vec<String> {
        let g = |args: &[&str]| text(&self.git_in(dir, args).stdout);
        let mut out = vec![
            format!("HEAD {}", g(&["rev-parse", "HEAD"]).trim()),
            format!("index\n{}", g(&["ls-files", "-s"])),
            format!("status\n{}", g(&["status", "--porcelain"])),
            format!("stashes\n{}", g(&["stash", "list", "--format=%gs"])),
            format!(
                "stash tree {}",
                g(&["rev-parse", "-q", "--verify", "refs/stash^{tree}"]).trim()
            ),
        ];
        for name in [
            "MERGE_HEAD",
            "MERGE_MODE",
            "MERGE_MSG",
            "ORIG_HEAD",
            "SQUASH_MSG",
        ] {
            out.push(format!(
                "{name} {:?}",
                fs::read_to_string(dir.join(".git").join(name)).ok()
            ));
        }
        let held = fs::read_to_string(dir.join(".git/MERGE_AUTOSTASH")).ok();
        let tree = held.map(|id| g(&["rev-parse", &format!("{}^{{tree}}", id.trim())]));
        out.push(format!("MERGE_AUTOSTASH {tree:?}"));
        for (path, content) in snapshot(dir) {
            if path == ".git" || path.starts_with(".git/") || path.starts_with(".pyn") {
                continue;
            }
            out.push(format!("{path} {content}"));
        }
        out
    }

    /// Runs `git <args>` and `pyn <args>`; the status class and the whole resulting state must agree.
    fn same(&self, args: &[&str]) -> (i32, i32) {
        let g = self.run(false, args);
        let p = self.run(true, args);
        let what = format!("{args:?}");
        let (gc, pc) = (g.status.code().unwrap(), p.status.code().unwrap());
        assert_eq!(
            status_class(gc),
            status_class(pc),
            "{what}: git said {} pyn said {}",
            text(&g.stderr),
            text(&p.stderr)
        );
        assert_eq!(self.state(&self.git), self.state(&self.pyn), "{what}");
        (gc, pc)
    }

    /// Like [`Pair::same`], and the command must have succeeded.
    fn works(&self, args: &[&str]) {
        let (g, p) = self.same(args);
        assert_eq!((g, p), (0, 0), "{args:?}");
    }

    /// Like [`Pair::same`], and the command must have been refused.
    fn refused(&self, args: &[&str]) {
        let (g, _) = self.same(args);
        assert_ne!(g, 0, "{args:?} should fail");
    }
}

fn each_format(script: &str, run: impl Fn(&Pair)) {
    for format in common::gitfix::FORMATS {
        run(&Pair::new(format, script));
    }
}

/// `side` has three commits (one with a body) and changes `f` cleanly against `main`.
const CLEAN: &str = "printf 'a\\nb\\nc\\nd\\ne\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'a\\nb\\nc\\nd\\nE\\n' >f; git commit -qam 'side one'\n\
    echo n >n; git add n; git commit -qm 'side two' -m 'with a body'\n\
    echo m >m; git add m; git commit -qm 'side three'\n\
    git checkout -q main\nprintf 'A\\nb\\nc\\nd\\ne\\n' >f; git commit -qam main\n\
    git tag -a -m 'tag message' v1 side\ngit update-ref refs/remotes/origin/x side\n";

/// `main` and `side` both changed `f` (conflict).
const CONFLICT: &str = "printf 'a\\nb\\nc\\nd\\ne\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'a\\nB2\\nc\\nd\\ne\\n' >f; git commit -qam 'side one'\n\
    git checkout -q main\nprintf 'a\\nB1\\nc\\nd\\ne\\n' >f; git commit -qam main\n";

/// `side` is ahead of `main`.
const AHEAD: &str = "printf 'a\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b side\necho n >n; git add .; git commit -qm side\ngit checkout -q main\n";

/// A second root commit on `other`, with a file `f` that also exists on `main`.
const UNRELATED: &str = "printf 'a\\nb\\n' >f; git add .; git commit -qm base\n\
    git checkout -q --orphan other\ngit rm -rfq .\nprintf 'a\\nb\\nz\\n' >f; echo o >o; git add .; git commit -qm other\n\
    git checkout -q main\n";

#[test]
fn signoff() {
    each_format(CLEAN, |p| {
        p.works(&["merge", "--signoff", "side"]);
    });
    each_format(CLEAN, |p| {
        p.works(&["merge", "--signoff", "--no-signoff", "side"]);
    });
    each_format(CLEAN, |p| {
        p.works(&["merge", "--no-signoff", "--signoff", "-m", "mine", "side"]);
    });
    each_format(CLEAN, |p| {
        p.works(&[
            "merge",
            "--signoff",
            "--no-ff",
            "-m",
            "mine\n\nBody",
            "side",
        ]);
    });
    each_format(CLEAN, |p| {
        p.works(&["merge", "--signoff", "--no-commit", "side"]);
        p.works(&["commit", "--no-edit"]);
    });
    each_format(CLEAN, |p| {
        p.works(&["merge", "--signoff", "--squash", "side"]);
        p.works(&["commit", "--no-edit"]);
    });
    each_format(AHEAD, |p| {
        p.works(&["merge", "--signoff", "side"]);
    });
    each_format(CONFLICT, |p| {
        p.refused(&["merge", "--signoff", "side"]);
        p.both_sh("printf 'a\\nB\\nc\\nd\\ne\\n' >f; git add f");
        p.works(&["commit", "--no-edit"]);
    });
}

#[test]
fn log() {
    for args in [
        &["merge", "--log", "side"][..],
        &["merge", "--log=1", "side"],
        &["merge", "--log=2", "side"],
        &["merge", "--log=3", "side"],
        &["merge", "--log=0", "side"],
        &["merge", "--no-log", "side"],
        &["merge", "--log", "--no-log", "side"],
        &["merge", "--no-log", "--log=2", "side"],
        &["merge", "--log", "-m", "mine", "side"],
        &["merge", "--log", "-m", "a", "-m", "b", "side"],
        &["merge", "--log", "--signoff", "side"],
        &["merge", "--log", "v1"],
        &["merge", "--log", "origin/x"],
        &["merge", "--log", "--no-commit", "side"],
        &["merge", "--log", "--squash", "side"],
    ] {
        each_format(CLEAN, |p| p.works(args));
    }
    each_format(CLEAN, |p| p.refused(&["merge", "--log=x", "side"]));
    each_format(AHEAD, |p| p.works(&["merge", "--log", "--no-ff", "side"]));
    each_format(CONFLICT, |p| {
        p.refused(&["merge", "--log", "side"]);
        p.both_sh("printf 'a\\nB\\nc\\nd\\ne\\n' >f; git add f");
        p.works(&["commit", "--no-edit"]);
    });
}

#[test]
fn log_from_settings() {
    for (value, args) in [
        ("true", &["merge", "side"][..]),
        ("2", &["merge", "side"]),
        ("1", &["merge", "side"]),
        ("false", &["merge", "side"]),
        ("false", &["merge", "--log", "side"]),
        ("true", &["merge", "--no-log", "side"]),
        ("5", &["merge", "--log=1", "side"]),
        ("0", &["merge", "--log", "side"]),
    ] {
        each_format(CLEAN, |p| {
            p.both_sh(&format!("git config merge.log {value}"));
            p.works(args);
        });
    }
}

#[test]
fn message_from_a_file() {
    let body = "  Title  \n\n\n# a comment\nbody   \n\n\n";
    for args in [
        &["merge", "--no-ff"][..],
        &["merge", "--cleanup=verbatim"],
        &["merge", "--cleanup=strip"],
        &["merge", "--cleanup=whitespace"],
        &["merge", "--cleanup=scissors"],
        &["merge", "--cleanup=default"],
        &["merge", "--signoff"],
        &["merge", "--log"],
    ] {
        each_format(CLEAN, |p| {
            let file = p.home.join("message");
            fs::write(&file, body).unwrap();
            let file = file.to_str().unwrap();
            p.works(&[args, &["-F", file, "side"]].concat());
        });
    }
    each_format(CLEAN, |p| {
        let file = p.home.join("message");
        fs::write(&file, "from a file\n").unwrap();
        p.works(&["merge", &format!("--file={}", file.display()), "side"]);
    });
}

#[test]
fn cleanup_of_a_message_given_with_m() {
    for args in [
        &["merge", "--cleanup=verbatim", "-m", "  spaced  "][..],
        &["merge", "--cleanup=strip", "-m", "x\n# comment\ny"],
        &["merge", "--cleanup=whitespace", "-m", "x\n# comment\ny  "],
        &["merge", "--cleanup=verbatim"],
    ] {
        each_format(CLEAN, |p| p.works(&[args, &["side"]].concat()));
    }
    each_format(CLEAN, |p| p.refused(&["merge", "--cleanup=bogus", "side"]));
}

#[test]
fn into_name_and_suppress_dest() {
    for (setting, args) in [
        ("", &["merge", "--into-name", "release", "side"][..]),
        ("", &["merge", "--into-name", "main", "side"]),
        ("", &["merge", "--into-name", "master", "side"]),
        ("merge.suppressDest zzz", &["merge", "side"]),
        (
            "merge.suppressDest zzz",
            &["merge", "--into-name", "zzz", "side"],
        ),
        ("merge.suppressDest ma*", &["merge", "side"]),
        ("merge.suppressDest ''", &["merge", "side"]),
        (
            "merge.suppressDest main",
            &["merge", "--into-name", "x", "side"],
        ),
        ("", &["merge", "--into-name", "x", "--log", "side"]),
        ("", &["merge", "--into-name", "x", "v1"]),
    ] {
        each_format(CLEAN, |p| {
            if !setting.is_empty() {
                p.both_sh(&format!("git config {setting}"));
            }
            p.works(args);
        });
    }
    each_format(CLEAN, |p| {
        p.both_sh("git checkout -q -b topic");
        p.works(&["merge", "side"]);
    });
    each_format(CLEAN, |p| {
        p.both_sh("git checkout -q -b topic\ngit config merge.suppressDest 'to*'");
        p.works(&["merge", "side"]);
    });
}

#[test]
fn unrelated_histories() {
    each_format(UNRELATED, |p| {
        p.refused(&["merge", "other"]);
        p.refused(&["merge", "--no-allow-unrelated-histories", "other"]);
        p.refused(&[
            "merge",
            "--allow-unrelated-histories",
            "--no-allow-unrelated-histories",
            "other",
        ]);
    });
    each_format(UNRELATED, |p| {
        p.refused(&["merge", "--allow-unrelated-histories", "other"]);
        p.both_sh("printf 'a\\nb\\nz\\n' >f; git add f");
        p.works(&["commit", "--no-edit"]);
    });
    each_format(UNRELATED, |p| {
        p.refused(&["merge", "--allow-unrelated-histories", "other"]);
        p.works(&["merge", "--abort"]);
    });
    each_format(UNRELATED, |p| {
        p.works(&[
            "merge",
            "--allow-unrelated-histories",
            "-X",
            "theirs",
            "other",
        ]);
    });
    each_format(UNRELATED, |p| {
        p.works(&[
            "merge",
            "--allow-unrelated-histories",
            "-X",
            "ours",
            "other",
        ]);
    });
    each_format(UNRELATED, |p| {
        p.works(&[
            "merge",
            "--allow-unrelated-histories",
            "-s",
            "ours",
            "other",
        ]);
    });
    each_format(UNRELATED, |p| {
        p.both_sh("git rm -q f; git commit -qm 'drop f'");
        p.works(&["merge", "--allow-unrelated-histories", "other"]);
    });
    each_format(UNRELATED, |p| {
        p.both_sh("git rm -q f; git commit -qm 'drop f'");
        p.works(&[
            "merge",
            "--allow-unrelated-histories",
            "--no-commit",
            "other",
        ]);
    });
}

#[test]
fn ff_setting() {
    for (value, args) in [
        ("false", &["merge", "side"][..]),
        ("false", &["merge", "--ff", "side"]),
        ("only", &["merge", "side"]),
        ("only", &["merge", "--no-ff", "side"]),
        ("true", &["merge", "side"]),
    ] {
        each_format(AHEAD, |p| {
            p.both_sh(&format!("git config merge.ff {value}"));
            let (g, _) = p.same(args);
            assert_eq!(g, 0, "{args:?}");
        });
    }
    each_format(CLEAN, |p| {
        p.both_sh("git config merge.ff only");
        p.refused(&["merge", "side"]);
        p.works(&["merge", "--no-ff", "side"]);
    });
    each_format(AHEAD, |p| {
        p.works(&["merge", "--ff", "--commit", "side"]);
    });
}

#[test]
fn accepted_without_effect() {
    for flag in ["--progress", "--no-progress", "-v", "--verbose", "--commit"] {
        each_format(CLEAN, |p| p.works(&["merge", flag, "side"]));
    }
    each_format(CLEAN, |p| p.works(&["merge", "-q", "side"]));
    each_format(AHEAD, |p| p.works(&["merge", "--quiet", "side"]));
}

/// `side` rewrites the first line of `g` and adds `s`; `main` changes `f`. A local edit at the end of `g` blocks the merge.
const STASHY: &str = "printf '1\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n9\\n' >g; printf 'a\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b side\nprintf 'one\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n9\\n' >g; echo s >s; git add .; git commit -qm side\n\
    git checkout -q main\nprintf 'a2\\n' >f; git commit -qam main\n";

#[test]
fn autostash() {
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.refused(&["merge", "side"]);
        p.works(&["merge", "--autostash", "side"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.works(&["merge", "--autostash", "--no-ff", "-m", "mine", "side"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g\ngit add g\necho more >>g");
        p.works(&["merge", "--autostash", "side"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g\ngit config merge.autoStash true");
        p.works(&["merge", "side"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g\ngit config merge.autoStash true");
        p.refused(&["merge", "--no-autostash", "side"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.refused(&["merge", "--autostash", "--no-autostash", "side"]);
    });
    each_format(STASHY, |p| {
        p.works(&["merge", "--autostash", "side"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.works(&["merge", "--autostash", "main"]);
    });
    // a fast-forward
    each_format(STASHY, |p| {
        p.both_sh("git reset -q --hard HEAD~1\necho dirty >>g");
        p.refused(&["merge", "side"]);
        p.works(&["merge", "--autostash", "side"]);
    });
}

#[test]
fn autostash_ends_in_each_way_a_merge_can() {
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.works(&["merge", "--autostash", "--no-commit", "side"]);
        p.works(&["commit", "--no-edit"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.works(&["merge", "--autostash", "--no-commit", "side"]);
        p.works(&["merge", "--abort"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.works(&["merge", "--autostash", "--no-commit", "side"]);
        p.works(&["merge", "--quit"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.works(&["merge", "--autostash", "--squash", "side"]);
        p.works(&["commit", "--no-edit"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.works(&["merge", "--autostash", "--no-commit", "side"]);
        p.works(&["merge", "--continue"]);
    });
}

#[test]
fn autostash_with_conflicts() {
    let script = format!(
        "{STASHY}printf 'one-too\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n9\\n' >g; git commit -qam g\n"
    );
    each_format(&script, |p| {
        p.both_sh("echo dirty >>g");
        p.refused(&["merge", "--autostash", "side"]);
        p.works(&["merge", "--abort"]);
    });
    each_format(&script, |p| {
        p.both_sh("echo dirty >>g");
        p.refused(&["merge", "--autostash", "side"]);
        p.both_sh("printf 'resolved\\n2\\n3\\n4\\n5\\n6\\n7\\n8\\n9\\n' >g; git add g");
        p.works(&["commit", "--no-edit"]);
    });
    each_format(&script, |p| {
        p.both_sh("echo dirty >>g");
        p.refused(&["merge", "--autostash", "side"]);
        p.works(&["merge", "--quit"]);
    });
}

#[test]
fn autostash_that_does_not_apply_cleanly() {
    // `side` changes `g`, and the local edit of `g` conflicts with it when put back
    let script = "printf 'a\\nb\\nc\\n' >g; git add .; git commit -qm base\n\
        git checkout -q -b side\nprintf 'a\\nb\\nS\\n' >g; git commit -qam side\n\
        git checkout -q main\n";
    each_format(script, |p| {
        p.both_sh("printf 'a\\nb\\nL\\n' >g");
        p.works(&["merge", "--autostash", "side"]);
    });
}

#[test]
fn autostash_when_the_merge_cannot_start() {
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g");
        p.refused(&["merge", "--autostash", "--ff-only", "side"]);
    });
    each_format(STASHY, |p| {
        p.both_sh("echo dirty >>g\necho in-the-way >s");
        p.refused(&["merge", "--autostash", "side"]);
    });
}

#[test]
fn quit() {
    each_format(CONFLICT, |p| {
        p.refused(&["merge", "side"]);
        p.works(&["merge", "--quit"]);
        p.works(&["merge", "--quit"]);
        p.refused(&["merge", "side"]);
    });
    each_format(CLEAN, |p| {
        p.works(&["merge", "--quit"]);
        p.works(&["merge", "--no-commit", "side"]);
        p.works(&["merge", "--quit"]);
        p.works(&["status", "--porcelain"]);
    });
    each_format(CLEAN, |p| {
        p.works(&["merge", "--squash", "side"]);
        p.works(&["merge", "--quit"]);
    });
    each_format(CLEAN, |p| {
        p.works(&["merge", "--no-commit", "side"]);
        p.refused(&["merge", "--quit", "side"]);
        p.refused(&["merge", "--quit", "--abort"]);
        p.refused(&["merge", "--quit", "--continue"]);
    });
}

const SIGNED: &str = "ssh-keygen -q -t ed25519 -N '' -C t@example.com -f \"$HOME/k\"\n\
    echo \"t@example.com $(cat \"$HOME/k.pub\")\" >\"$HOME/allowed\"\n\
    git config gpg.format ssh\ngit config user.signingkey \"$HOME/k\"\n\
    git config gpg.ssh.allowedSignersFile \"$HOME/allowed\"\n\
    printf 'a\\n' >f; git add .; git commit -qm base\n\
    git checkout -q -b signed\necho s >s; git add s; git commit -q -S -m signed\n\
    git checkout -q -b unsigned main\necho u >u; git add u; git commit -qm unsigned\n\
    git checkout -q -b mixed main\necho m >m; git add m; git commit -q -S -m signed-one\n\
    echo n >n; git add n; git commit -qm unsigned-tip\n\
    git checkout -q main\nprintf 'a2\\n' >f; git commit -qam main\n";

#[test]
fn verify_signatures() {
    if !have_ssh_keygen() {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    each_format(SIGNED, |p| {
        p.works(&["merge", "--verify-signatures", "signed"]);
    });
    each_format(SIGNED, |p| {
        p.refused(&["merge", "--verify-signatures", "unsigned"]);
        p.refused(&["merge", "--verify-signatures", "mixed"]);
    });
    each_format(SIGNED, |p| {
        p.works(&["merge", "--no-verify-signatures", "unsigned"]);
    });
    each_format(SIGNED, |p| {
        p.works(&[
            "merge",
            "--verify-signatures",
            "--no-verify-signatures",
            "unsigned",
        ]);
    });
    each_format(SIGNED, |p| {
        p.both_sh("git config merge.verifySignatures true");
        p.refused(&["merge", "unsigned"]);
        p.works(&["merge", "--no-verify-signatures", "unsigned"]);
    });
    each_format(SIGNED, |p| {
        p.both_sh("git config merge.verifySignatures true");
        p.works(&["merge", "signed"]);
    });
    each_format(SIGNED, |p| {
        p.both_sh("git checkout -q signed^ 2>/dev/null; git checkout -q -B main signed^");
        p.works(&["merge", "--verify-signatures", "signed"]);
    });
}

#[test]
fn title_of_an_ancestor_of_a_branch() {
    for spec in [
        "side^", "side^^", "side~1", "side~2", "side~", "side~0", "side^0", "side~~", "side^~1",
        "nope^", "v1^",
    ] {
        each_format(CLEAN, |p| {
            p.same(&["merge", "--no-ff", spec]);
        });
        each_format(CLEAN, |p| {
            p.same(&["merge", "--no-ff", "--log", spec]);
        });
    }
}

#[test]
fn scissors_cleanup_with_the_editor() {
    let editors = [
        "true",
        "sed -i '1s/$/ edited/'",
        "sh -c 'echo below >>\"$0\"'",
        "sh -c 'printf \"top\\n\\n\" | cat - \"$0\" >\"$0.new\"; mv \"$0.new\" \"$0\"'",
        "sh -c ': >\"$0\"'",
    ];
    for editor in editors {
        for flags in [
            &["--cleanup=scissors"][..],
            &["--cleanup=scissors", "--no-ff"][..],
        ] {
            each_format(CLEAN, |p| {
                *p.editor.borrow_mut() = editor.into();
                let mut args = vec!["merge", "--edit"];
                args.extend(flags);
                args.push("side");
                p.same(&args);
            });
        }
        each_format(CLEAN, |p| {
            *p.editor.borrow_mut() = editor.into();
            p.same(&[
                "merge",
                "--edit",
                "--cleanup=scissors",
                "-m",
                "given",
                "side",
            ]);
        });
    }
    each_format(CLEAN, |p| {
        p.same(&[
            "merge",
            "--cleanup=scissors",
            "-m",
            "kept\n# ------------------------ >8 ------------------------\ngone",
            "side",
        ]);
    });
}

#[test]
fn negated_forms_reset_what_came_before() {
    let cases: &[&[&str]] = &[
        &["-X", "ours", "--no-strategy-option"],
        &["-X", "ours", "--no-strategy-option", "-X", "theirs"],
        &["--into-name=topic", "--no-into-name"],
        &["--no-into-name", "--into-name=topic"],
    ];
    for flag in ["--no-message", "--no-strategy", "--no-cleanup"] {
        each_format(CLEAN, |p| {
            let before = p.state(&p.pyn);
            let out = p.run(true, &["merge", "-m", "x", flag, "side"]);
            assert_eq!(out.status.code(), Some(129), "{flag}");
            assert_eq!(p.state(&p.pyn), before, "{flag}");
        });
    }
    for flags in cases {
        each_format(CONFLICT, |p| {
            p.both_sh("printf 'file message\\n' >msg");
            let mut args = vec!["merge"];
            args.extend(flags.iter());
            args.push("side");
            p.same(&args);
        });
        each_format(CLEAN, |p| {
            p.both_sh("printf 'file message\\n' >msg");
            let mut args = vec!["merge"];
            args.extend(flags.iter());
            args.push("side");
            p.same(&args);
        });
    }
}

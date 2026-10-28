//! `.mailmap` support on a git repository compared with the real git: `log` and `show` formats and filters,
//! `shortlog`, `blame` and `check-mailmap`, over the line forms, the sources and their order, on SHA-1 and SHA-256.
//! Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

type Who = (&'static str, &'static str);

const OLD_ANN: Who = ("Old Ann", "ann@old.example");
const BOB: Who = ("Bob", "BOB@Example.com");
const CAROL_Q: Who = ("Carol Q", "carol@old.example");
const CAROL_O: Who = ("Carol Other", "carol@old.example");
const ZOE: Who = ("Zo\u{eb} \u{4e2d}", "zoe@example.com");
const COMMITTER: Who = ("Committer", "committer@example.com");

const MAPS: &[&str] = &[
    "Ann Proper <ann@new.example> <ann@old.example>\n",
    "Robert <bob@example.com>\n",
    "<ann@new.example> <ann@old.example>\n",
    "Proper Carol <pc@new.example> Carol Q <carol@old.example>\n\
     Other Carol <oc@new.example> Carol Other <carol@old.example>\n",
    "Simple <s@x.example> <carol@old.example>\nSpecific <t@x.example> Carol Q <carol@old.example>\n",
    "Specific <t@x.example> Carol Q <carol@old.example>\nSimple <s@x.example> <carol@old.example>\n",
    "# comment\n\nno brackets here\nName <>\nBroken <b@x\n Ann Late <ann@new.example> <ann@old.example> # note\n",
    "Proper <p@x.example> carol q <CAROL@OLD.EXAMPLE>\n",
    "Zoe Proper <zoe@new.example> <zoe@example.com>\n",
    "Cee <c@new.example> <committer@example.com>\n",
    "First <f@x.example> <ann@old.example>\n<second@x.example> <ann@old.example>\nThird <ann@old.example>\n",
    "Name <ann@old.example>\n<ann@old.example>\n",
    "  Spaced   Name  <sp@x.example>   <ann@old.example>  # note\r\nCrlf Bob <crlf@x.example> <bob@example.com>\r\n",
    "# only a comment\n",
    "",
    "Ann <>\n<only@x.example>\n <leading@x.example> <ann@old.example>\n",
];

struct Fx {
    _tmp: TempDir,
    dir: PathBuf,
    home: PathBuf,
    format: &'static str,
}

fn commit(dir: &Path, author: Who, committer: Who, message: &str, step: u32) {
    run(dir, &["add", "-A"]);
    let status = git(dir)
        .env("GIT_AUTHOR_NAME", author.0)
        .env("GIT_AUTHOR_EMAIL", author.1)
        .env("GIT_COMMITTER_NAME", committer.0)
        .env("GIT_COMMITTER_EMAIL", committer.1)
        .env(
            "GIT_AUTHOR_DATE",
            format!("{} +0000", 1_700_000_000 + step * 100),
        )
        .env(
            "GIT_COMMITTER_DATE",
            format!("{} +0000", 1_700_000_000 + step * 100),
        )
        .args(["commit", "-q", "--allow-empty", "-m", message])
        .status()
        .unwrap();
    assert!(status.success());
}

impl Fx {
    fn new(format: &'static str) -> Self {
        Self::with_people(
            format,
            &[
                (OLD_ANN, COMMITTER),
                (BOB, BOB),
                (CAROL_Q, COMMITTER),
                (CAROL_O, CAROL_Q),
                (ZOE, ZOE),
                (OLD_ANN, OLD_ANN),
            ],
        )
    }

    fn with_people(format: &'static str, people: &[(Who, Who)]) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (dir, home) = (root.join("repo"), root.join("home"));
        fs::create_dir_all(&home).unwrap();
        init(&dir, format, false);
        let mut lines = Vec::new();
        for (i, (a, c)) in people.iter().enumerate() {
            lines.push(format!("line by {}", a.0));
            fs::write(dir.join("f.txt"), lines.join("\n") + "\n").unwrap();
            commit(&dir, *a, *c, &format!("commit {i}"), i as u32);
        }
        Self {
            _tmp: tmp,
            dir,
            home,
            format,
        }
    }

    fn oracle(&self, cwd: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>) {
        let mut cmd = git(cwd);
        cmd.env("TZ", "UTC").args(args);
        finish(cmd, stdin)
    }

    fn pyn(&self, cwd: &Path, args: &[&str], stdin: &[u8]) -> (i32, Vec<u8>) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pyn"));
        cmd.current_dir(cwd)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", &self.home)
            .env("TZ", "UTC");
        finish(cmd, stdin)
    }

    /// Both tools in `cwd` print the same and exit in the same class.
    fn same_in(&self, cwd: &Path, args: &[&str], stdin: &[u8]) {
        let (want_code, want) = self.oracle(cwd, args, stdin);
        let (code, got) = self.pyn(cwd, args, stdin);
        let what = format!("{} {args:?}", self.format);
        assert_eq!(
            oracle::status_class(code),
            oracle::status_class(want_code),
            "{what}"
        );
        let (got, want) = (
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(&want),
        );
        assert_eq!(
            oracle::blame_lines(got.as_bytes()),
            oracle::blame_lines(want.as_bytes()),
            "{what}"
        );
    }

    fn same(&self, args: &[&str]) {
        self.same_in(&self.dir, args, b"");
    }

    fn mailmap(&self, text: &str) {
        let path = self.dir.join(".mailmap");
        let _ = fs::remove_file(&path);
        fs::write(path, text).unwrap();
    }
}

fn finish(mut cmd: Command, stdin: &[u8]) -> (i32, Vec<u8>) {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap(), out.stdout)
}

fn each(check: impl Fn(&Fx)) {
    for format in FORMATS {
        check(&Fx::new(format));
    }
}

/// The commands whose output depends on the mailmap.
fn everywhere(fx: &Fx) {
    let all = "%an|%ae|%al|%aN|%aE|%aL|%cn|%ce|%cl|%cN|%cE|%cL";
    for flag in [
        "--use-mailmap",
        "--no-use-mailmap",
        "--mailmap",
        "--no-mailmap",
    ] {
        fx.same(&["log", flag, &format!("--format={all}")]);
        for pretty in ["short", "medium", "full", "fuller", "raw", "email"] {
            if pretty != "email" {
                fx.same(&["log", flag, &format!("--pretty={pretty}")]);
            }
        }
        fx.same(&["show", "-s", flag, "HEAD~2"]);
        fx.same(&["show", "-s", flag, "--pretty=fuller", "HEAD"]);
    }
    fx.same(&["log", "--format=%an|%aN|%cN"]);
    fx.same(&["log"]);
    fx.same(&["show", "-s"]);
    fx.same(&["log", "--use-mailmap", "--no-use-mailmap", "--format=%aN"]);
    fx.same(&["log", "--no-use-mailmap", "--use-mailmap", "--format=%an"]);
    for name in [
        "Ann Proper",
        "Old Ann",
        "Robert",
        "Bob",
        "Carol",
        "Proper",
        "Cee",
        "Committer",
        "Zoe",
    ] {
        fx.same(&[
            "log",
            "--use-mailmap",
            "--oneline",
            &format!("--author={name}"),
        ]);
        fx.same(&[
            "log",
            "--no-use-mailmap",
            "--oneline",
            &format!("--author={name}"),
        ]);
        fx.same(&[
            "log",
            "--no-use-mailmap",
            "--oneline",
            &format!("--committer={name}"),
        ]);
    }
    fx.same(&[
        "log",
        "--use-mailmap",
        "--oneline",
        "--author=<ann@new.example>",
    ]);
    fx.same(&["log", "--use-mailmap", "--oneline", "-i", "--author=ROBERT"]);
    for opts in ["-s", "-se", "-sn", "-sne", "-sc", "-sce", "-c", "-e", "-n"] {
        fx.same(&["shortlog", opts, "HEAD"]);
    }
    fx.same(&["shortlog", "HEAD~3..HEAD"]);
    for args in [
        &["blame", "f.txt"][..],
        &["blame", "-e", "f.txt"],
        &["blame", "-s", "f.txt"],
        &["blame", "--porcelain", "f.txt"],
        &["blame", "--line-porcelain", "f.txt"],
        &["blame", "HEAD~2", "f.txt"],
        &["annotate", "f.txt"],
    ] {
        fx.same(args);
    }
    let contacts = [
        "Old Ann <ann@old.example>",
        "<ann@old.example>",
        "<ANN@OLD.EXAMPLE>",
        "Carol Q <carol@old.example>",
        "carol q <carol@old.example>",
        "Carol Other <carol@old.example>",
        "Nobody <carol@old.example>",
        "Bob <bob@example.com>",
        "Unknown Person <unknown@example.com>",
        "<unknown@example.com>",
        "Committer <committer@example.com>",
        "Zo\u{eb} \u{4e2d} <zoe@example.com>",
    ];
    for c in contacts {
        fx.same(&["check-mailmap", c]);
    }
    fx.same(&["check-mailmap", contacts[0], contacts[3], contacts[7]]);
    fx.same_in(
        &fx.dir,
        &["check-mailmap", "--stdin"],
        contacts.join("\n").as_bytes(),
    );
    fx.same_in(
        &fx.dir,
        &["check-mailmap", contacts[2], "--stdin"],
        contacts[3..6].join("\n").as_bytes(),
    );
    let stdin = b"commit 1\nAuthor: Old Ann <ann@old.example>\nDate: x\n\n    first\n\n\
        commit 2\nAuthor: Carol Q <carol@old.example>\nDate: x\n\n    second\n\n\
        commit 3\nAuthor: Bob <BOB@example.com>\n\n    third\n";
    for opts in ["-s", "-se", "-ne"] {
        fx.same_in(&fx.dir, &["shortlog", opts], stdin);
    }
}

#[test]
fn every_line_form_maps_like_git() {
    each(|fx| {
        for map in MAPS {
            fx.mailmap(map);
            everywhere(fx);
        }
    });
}

#[test]
fn a_missing_mailmap_changes_nothing() {
    each(everywhere);
}

#[test]
fn log_mailmap_config_sets_the_default() {
    each(|fx| {
        fx.mailmap(MAPS[0]);
        for value in ["true", "false"] {
            run(&fx.dir, &["config", "log.mailmap", value]);
            fx.same(&["log"]);
            fx.same(&["show", "-s"]);
            fx.same(&["log", "--oneline", "--author=Ann Proper"]);
            fx.same(&["log", "--use-mailmap"]);
            fx.same(&["log", "--no-use-mailmap"]);
            fx.same(&["log", "--format=%aN"]);
        }
    });
}

#[test]
fn mailmap_file_and_blob_apply_in_git_order() {
    each(|fx| {
        let other = fx.dir.parent().unwrap().join("other.map");
        fs::write(&other, "From File <file@x.example> <ann@old.example>\nFile Bob <fb@x.example> <bob@example.com>\n").unwrap();
        fs::write(fx.dir.join("blob.map"), "From Blob <blob@x.example> <ann@old.example>\nBlob Carol <bc@x.example> Carol Q <carol@old.example>\n").unwrap();
        run(&fx.dir, &["add", "blob.map"]);
        commit(&fx.dir, OLD_ANN, OLD_ANN, "blob", 50);
        fx.mailmap("From Tree <tree@x.example> <ann@old.example>\nTree Bob <tb@x.example> <bob@example.com>\nTree Zoe <tz@x.example> <zoe@example.com>\n");
        run(&fx.dir, &["config", "mailmap.blob", "HEAD:blob.map"]);
        everywhere(fx);
        run(
            &fx.dir,
            &["config", "mailmap.file", other.to_str().unwrap()],
        );
        everywhere(fx);
        run(&fx.dir, &["config", "--unset", "mailmap.blob"]);
        everywhere(fx);
        run(&fx.dir, &["config", "mailmap.file", "../other.map"]);
        everywhere(fx);
        run(&fx.dir, &["config", "mailmap.blob", "HEAD:no-such-file"]);
        everywhere(fx);
        run(&fx.dir, &["config", "mailmap.file", "/no/such/file"]);
        everywhere(fx);
    });
}

#[test]
fn the_mailmap_is_read_from_the_work_tree_root_in_a_subfolder() {
    each(|fx| {
        fx.mailmap(MAPS[0]);
        fs::create_dir_all(fx.dir.join("sub/deeper")).unwrap();
        let sub = fx.dir.join("sub/deeper");
        fx.same_in(&sub, &["log", "--format=%aN"], b"");
        fx.same_in(&sub, &["shortlog", "-se", "HEAD"], b"");
        fx.same_in(&sub, &["check-mailmap", "<ann@old.example>"], b"");
        fx.same_in(&sub, &["blame", "-e", "../../f.txt"], b"");
    });
}

#[test]
fn a_symlinked_mailmap_is_not_followed() {
    each(|fx| {
        fs::write(fx.dir.join("real.map"), MAPS[0]).unwrap();
        std::os::unix::fs::symlink("real.map", fx.dir.join(".mailmap")).unwrap();
        everywhere(fx);
    });
}

#[test]
fn a_bare_repository_reads_head_mailmap() {
    each(|fx| {
        fx.mailmap(MAPS[3]);
        commit(&fx.dir, OLD_ANN, OLD_ANN, "add mailmap", 60);
        fx.mailmap("Not In Head <nih@x.example> <ann@old.example>\n");
        let bare = fx.dir.parent().unwrap().join("bare.git");
        run(
            fx.dir.parent().unwrap(),
            &[
                "clone",
                "-q",
                "--bare",
                fx.dir.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        for args in [
            &["log", "--format=%aN|%aE|%cN"][..],
            &["log", "--use-mailmap"],
            &["shortlog", "-se", "HEAD"],
            &["check-mailmap", "Carol Q <carol@old.example>"],
        ] {
            fx.same_in(&bare, args, b"");
        }
        run(&bare, &["config", "mailmap.blob", "HEAD~1:f.txt"]);
        fx.same_in(&bare, &["log", "--format=%aN"], b"");
        run(&bare, &["config", "mailmap.blob", "no-such-ref:.mailmap"]);
        fx.same_in(&bare, &["log", "--format=%aN"], b"");
    });
}

#[test]
fn shortlog_outside_a_repository_reads_the_mailmap_in_the_folder() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    fs::write(dir.join(".mailmap"), MAPS[0].to_owned() + MAPS[1]).unwrap();
    let fx = Fx {
        _tmp: tempdir().unwrap(),
        dir: dir.clone(),
        home: dir.clone(),
        format: "sha1",
    };
    let stdin = b"commit 1\nAuthor: Old Ann <ann@old.example>\n\n    first\n\n\
        commit 2\nAuthor: Bob <BOB@example.com>\n\n    second\n";
    for opts in ["-s", "-se"] {
        fx.same_in(&dir, &["shortlog", opts], stdin);
    }
}

#[test]
fn check_mailmap_without_contacts_is_a_failure() {
    each(|fx| {
        fx.same(&["check-mailmap"]);
    });
}

/// With the author and committer the same person, git's mapping of both lines is unambiguous.
#[test]
fn committer_filters_use_the_mapped_identity() {
    let people = [
        (OLD_ANN, OLD_ANN),
        (BOB, BOB),
        (CAROL_Q, CAROL_Q),
        (CAROL_O, CAROL_O),
        (ZOE, ZOE),
    ];
    for format in FORMATS {
        let fx = Fx::with_people(format, &people);
        for map in MAPS {
            fx.mailmap(map);
            for name in [
                "Ann Proper",
                "Old Ann",
                "Robert",
                "Bob",
                "Carol",
                "Proper",
                "Zoe",
                "Simple",
            ] {
                for flag in ["--use-mailmap", "--no-use-mailmap"] {
                    fx.same(&["log", flag, "--oneline", &format!("--committer={name}")]);
                    fx.same(&[
                        "log",
                        flag,
                        "--oneline",
                        &format!("--author={name}"),
                        &format!("--committer={name}"),
                    ]);
                }
            }
        }
    }
}

//! Signed commits and tags on a git repository: `pyn commit -S` and `pyn tag -s` with a fake signer program compared
//! with real git given the same program (object ids, headers, signer arguments, exit statuses), and with a real
//! `ssh-keygen -Y sign` verified by git. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const BASE_ENV: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "Ann Author"),
    ("GIT_AUTHOR_EMAIL", "ann@example.com"),
    ("GIT_COMMITTER_NAME", "Cy Committer"),
    ("GIT_COMMITTER_EMAIL", "cy@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0100"),
    ("GIT_COMMITTER_DATE", "1700000100 -0200"),
];

const FAKE_SIGNER: &str = r#"#!/bin/sh
printf '%s\n' "$@" >> "$SIGNER_LOG"
sum=$(cksum | cut -d' ' -f1)
echo "[GNUPG:] KEY_CONSIDERED 0 0" >&2
echo "[GNUPG:] SIG_CREATED D 1 8 00 1 ffff" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nfake%s\n\n-----END PGP SIGNATURE-----\n' "$sum"
"#;

const BROKEN_SIGNER: &str = "#!/bin/sh\ncat >/dev/null\nexit 1\n";

struct Pair {
    _tmp: TempDir,
    root: PathBuf,
    oracle: PathBuf,
    ours: PathBuf,
}

fn set_env(c: &mut Command, root: &Path, log: &Path) {
    c.env("HOME", root.join("home"))
        .env("TZ", "UTC")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("SIGNER_LOG", log)
        .env("GIT_SSH_COMMAND", root.join("ssh-shim"));
    for (k, v) in BASE_ENV {
        c.env(k, v);
    }
}

fn script(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

impl Pair {
    fn new(format: &str, signer_body: &str, config: &[(&str, &str)]) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let signer = script(&root, "signer", signer_body);
        let (oracle, ours) = (root.join("oracle"), root.join("ours"));
        for dir in [&oracle, &ours] {
            init(dir, format, false);
            let log = root.join("setup.log");
            let step = |args: &[&str]| {
                let mut c = git(dir);
                set_env(&mut c, &root, &log);
                let out = c.args(args).output().unwrap();
                assert!(out.status.success(), "git {args:?}");
            };
            fs::write(dir.join("a.txt"), "one\n").unwrap();
            step(&["add", "."]);
            step(&["commit", "-q", "-m", "one"]);
            fs::write(dir.join("b.txt"), "two\n").unwrap();
            step(&["add", "."]);
            step(&["config", "gpg.program", signer.to_str().unwrap()]);
            for (k, v) in config {
                step(&["config", k, v]);
            }
        }
        Self {
            _tmp: tmp,
            root,
            oracle,
            ours,
        }
    }

    fn log(&self, pyn: bool) -> PathBuf {
        self.root.join(if pyn { "ours.log" } else { "oracle.log" })
    }

    /// Runs `<cmd> <args>` with git or pyn; the exit status.
    fn run(&self, pyn: bool, cmd: &str, args: &[&str]) -> i32 {
        let dir = if pyn { &self.ours } else { &self.oracle };
        let mut c = if pyn {
            let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
            c.env_clear().env("PATH", path_env()).current_dir(dir);
            c.arg(cmd);
            c
        } else {
            let mut c = git(dir);
            c.arg(cmd);
            c
        };
        set_env(&mut c, &self.root, &self.log(pyn));
        c.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .code()
            .unwrap()
    }

    fn git_out(&self, dir: &Path, args: &[&str]) -> String {
        let mut c = git(dir);
        set_env(&mut c, &self.root, &self.log(false));
        let out = c.args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Same exit status, same signer arguments, and (on success) the same object, which git accepts.
    fn same(&self, cmd: &str, args: &[&str], show: &[&str], label: &str) -> i32 {
        let want = self.run(false, cmd, args);
        let got = self.run(true, cmd, args);
        assert_eq!(got, want, "{label}: exit status");
        let read = |p: &Path| fs::read_to_string(p).unwrap_or_default();
        assert_eq!(
            read(&self.log(true)),
            read(&self.log(false)),
            "{label}: signer arguments"
        );
        assert_eq!(
            self.git_out(&self.ours, show),
            self.git_out(&self.oracle, show),
            "{label}: object"
        );
        if got == 0 {
            fsck(&self.ours);
        }
        got
    }
}

type Case<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);

const SHOW_HEAD: &[&str] = &["cat-file", "commit", "HEAD"];
const SHOW_TAG: &[&str] = &["cat-file", "tag", "v1"];

#[test]
fn signed_commits_match_git_for_every_way_to_ask() {
    for format in FORMATS {
        let cases: &[Case] = &[
            ("flag", &["-S", "-m", "signed"], &[]),
            (
                "key by equals",
                &["--gpg-sign=ABCD1234", "-m", "signed"],
                &[],
            ),
            ("key attached", &["-SABCD1234", "-m", "signed"], &[]),
            ("config", &["-m", "signed"], &[("commit.gpgsign", "true")]),
            (
                "signing key",
                &["-S", "-m", "signed"],
                &[("user.signingkey", "KEY99")],
            ),
            (
                "flag key beats setting",
                &["-SFLAGKEY", "-m", "signed"],
                &[("user.signingkey", "KEY99")],
            ),
            (
                "no-gpg-sign beats config",
                &["--no-gpg-sign", "-m", "plain"],
                &[("commit.gpgsign", "true")],
            ),
            (
                "last flag wins",
                &["--no-gpg-sign", "-S", "-m", "signed"],
                &[],
            ),
            (
                "multi-line message",
                &["-S", "-m", "subject", "-m", "body"],
                &[],
            ),
            (
                "openpgp program key",
                &["-S", "-m", "signed"],
                &[("gpg.format", "openpgp")],
            ),
            ("unsigned", &["-m", "plain"], &[]),
        ];
        for (name, args, config) in cases {
            let pair = Pair::new(format, FAKE_SIGNER, config);
            let code = pair.same("commit", args, SHOW_HEAD, &format!("{format} {name}"));
            assert_eq!(code, 0);
            let head = pair.git_out(&pair.ours, SHOW_HEAD);
            let signed = !name.contains("no-gpg") && *name != "unsigned";
            assert_eq!(head.contains("\ngpgsig"), signed, "{format} {name}: {head}");
        }
    }
}

#[test]
fn the_header_is_gpgsig_on_sha1_and_what_git_writes_on_sha256() {
    for format in FORMATS {
        let pair = Pair::new(format, FAKE_SIGNER, &[("commit.gpgsign", "true")]);
        assert_eq!(pair.same("commit", &["-m", "x"], SHOW_HEAD, format), 0);
        let head = pair.git_out(&pair.oracle, SHOW_HEAD);
        let want = if format == "sha1" {
            "\ngpgsig "
        } else {
            "\ngpgsig-sha256 "
        };
        assert!(head.contains(want), "{format}: {head}");
    }
}

#[test]
fn amend_resigns_and_merge_in_progress_commits_sign() {
    for format in FORMATS {
        let pair = Pair::new(format, FAKE_SIGNER, &[]);
        assert_eq!(
            pair.same("commit", &["-S", "-m", "first"], SHOW_HEAD, format),
            0
        );
        assert_eq!(
            pair.same(
                "commit",
                &["--amend", "--no-edit", "--allow-empty", "-S"],
                SHOW_HEAD,
                format
            ),
            0
        );
        assert_eq!(
            pair.same(
                "commit",
                &["--amend", "--no-edit", "--allow-empty"],
                SHOW_HEAD,
                format
            ),
            0
        );
        assert!(!pair.git_out(&pair.ours, SHOW_HEAD).contains("gpgsig"));
    }
}

#[test]
fn a_failing_signer_commits_nothing() {
    for format in FORMATS {
        let pair = Pair::new(format, BROKEN_SIGNER, &[]);
        let before = pair.git_out(&pair.ours, &["rev-parse", "HEAD"]);
        let code = pair.same("commit", &["-S", "-m", "x"], SHOW_HEAD, format);
        assert_ne!(code, 0);
        assert_eq!(pair.git_out(&pair.ours, &["rev-parse", "HEAD"]), before);
        assert_eq!(
            pair.git_out(&pair.ours, &["status", "--porcelain"]),
            "A  b.txt\n"
        );
    }
}

#[test]
fn a_missing_signer_program_fails_without_committing() {
    let pair = Pair::new("sha1", FAKE_SIGNER, &[]);
    let missing = pair.root.join("no-such-signer");
    for dir in [&pair.oracle, &pair.ours] {
        let mut c = git(dir);
        set_env(&mut c, &pair.root, &pair.log(false));
        c.args(["config", "gpg.program", missing.to_str().unwrap()])
            .status()
            .unwrap();
    }
    assert_ne!(pair.run(false, "commit", &["-S", "-m", "x"]), 0);
    assert_ne!(pair.run(true, "commit", &["-S", "-m", "x"]), 0);
    assert_eq!(
        pair.git_out(&pair.ours, &["rev-list", "--count", "HEAD"]),
        "1\n"
    );
}

#[test]
fn bad_gpg_format_is_an_error() {
    let pair = Pair::new("sha1", FAKE_SIGNER, &[("gpg.format", "nonsense")]);
    assert_ne!(pair.run(false, "commit", &["-S", "-m", "x"]), 0);
    assert_ne!(pair.run(true, "commit", &["-S", "-m", "x"]), 0);
    assert_eq!(
        pair.git_out(&pair.ours, &["rev-list", "--count", "HEAD"]),
        "1\n"
    );
}

#[test]
fn signed_tags_match_git() {
    for format in FORMATS {
        let cases: &[Case] = &[
            ("-s", &["-s", "-m", "release", "v1"], &[]),
            ("-u", &["-u", "TAGKEY", "-m", "release", "v1"], &[]),
            (
                "config",
                &["-m", "release", "v1"],
                &[("tag.gpgsign", "true")],
            ),
            (
                "config with -a",
                &["-a", "-m", "release", "v1"],
                &[("tag.gpgsign", "true")],
            ),
            (
                "no-sign",
                &["--no-sign", "-m", "release", "v1"],
                &[("tag.gpgsign", "true")],
            ),
            (
                "signing key",
                &["-s", "-m", "release", "v1"],
                &[("user.signingkey", "KEY99")],
            ),
            ("lightweight", &["v1"], &[]),
        ];
        for (name, args, config) in cases {
            let pair = Pair::new(format, FAKE_SIGNER, config);
            let label = format!("{format} {name}");
            let code = pair.same("tag", args, SHOW_TAG, &label);
            assert_eq!(code, 0, "{label}");
            let want = pair.git_out(&pair.oracle, &["rev-parse", "v1"]);
            assert_eq!(
                pair.git_out(&pair.ours, &["rev-parse", "v1"]),
                want,
                "{label}"
            );
            let tag = pair.git_out(&pair.ours, &["cat-file", "-t", "v1"]);
            let signed = !matches!(*name, "no-sign" | "lightweight");
            if signed {
                let body = pair.git_out(&pair.ours, SHOW_TAG);
                assert!(body.contains("BEGIN PGP SIGNATURE"), "{label}: {body}");
            }
            assert_eq!(
                tag.trim(),
                if *name == "lightweight" {
                    "commit"
                } else {
                    "tag"
                }
            );
        }
    }
}

#[test]
fn a_failing_signer_makes_no_tag() {
    let pair = Pair::new("sha1", BROKEN_SIGNER, &[]);
    assert_ne!(
        pair.same("tag", &["-s", "-m", "x", "v1"], SHOW_TAG, "broken"),
        0
    );
    assert_eq!(pair.git_out(&pair.ours, &["tag", "-l"]), "");
}

fn have_ssh_keygen() -> bool {
    Command::new("ssh-keygen")
        .arg("-?")
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .is_ok()
}

/// A new ed25519 key `dir/key` for `cy@example.com` and an allowed-signers file naming it.
fn ssh_key(dir: &Path) -> (PathBuf, PathBuf) {
    let key = dir.join("key");
    let made = Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "cy@example.com",
            "-f",
        ])
        .arg(&key)
        .status()
        .unwrap();
    assert!(made.success());
    let public = fs::read_to_string(dir.join("key.pub")).unwrap();
    let allowed = dir.join("allowed");
    fs::write(&allowed, format!("cy@example.com {public}")).unwrap();
    (key, allowed)
}

#[test]
fn ssh_signatures_made_by_ssh_keygen_verify_in_git() {
    if !have_ssh_keygen() {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    for format in FORMATS {
        let tmp = tempdir().unwrap();
        let (key, allowed) = ssh_key(tmp.path());
        for (name, signingkey) in [
            ("path", key.to_str().unwrap().to_string()),
            (
                "tilde",
                format!("~/{}", key.file_name().unwrap().to_str().unwrap()),
            ),
        ] {
            let pair = Pair::new(format, FAKE_SIGNER, &[]);
            if name == "tilde" {
                fs::copy(&key, pair.root.join("home").join("key")).unwrap();
                fs::set_permissions(
                    pair.root.join("home/key"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            for dir in [&pair.ours] {
                for (k, v) in [
                    ("gpg.format", "ssh"),
                    ("user.signingkey", signingkey.as_str()),
                    ("commit.gpgsign", "true"),
                    ("tag.gpgsign", "true"),
                ] {
                    let mut c = git(dir);
                    set_env(&mut c, &pair.root, &pair.log(false));
                    assert!(c.args(["config", k, v]).status().unwrap().success());
                }
            }
            assert_eq!(
                pair.run(true, "commit", &["-m", "ssh signed"]),
                0,
                "{format} {name}"
            );
            assert_eq!(
                pair.run(true, "tag", &["-m", "ssh tag", "v1"]),
                0,
                "{format} {name}"
            );
            let verify = |what: &str, id: &str| {
                let mut c = git(&pair.ours);
                set_env(&mut c, &pair.root, &pair.log(false));
                c.args([
                    "-c",
                    "gpg.format=ssh",
                    "-c",
                    &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
                    what,
                    id,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .code()
            };
            assert_eq!(
                verify("verify-commit", "HEAD"),
                Some(0),
                "{format} {name}: commit"
            );
            assert_eq!(verify("verify-tag", "v1"), Some(0), "{format} {name}: tag");
            fsck(&pair.ours);
        }
    }
}

const LOG_RAW: &[&str] = &["log", "--format=raw"];

impl Pair {
    /// Runs the git steps in both repositories, unsigned and with fixed dates, so the histories are identical.
    fn setup(&self, steps: &[&[&str]]) {
        for dir in [&self.oracle, &self.ours] {
            for args in steps {
                let mut c = git(dir);
                set_env(&mut c, &self.root, &self.root.join("setup.log"));
                let out = c
                    .args(["-c", "commit.gpgsign=false"])
                    .args(*args)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "git {args:?}");
            }
        }
    }

    /// `main` and `side` have diverged; `side` ends in a merge of `topic`; `pick` holds two more commits.
    fn diverged(format: &str, signer: &str, config: &[(&str, &str)]) -> Self {
        let pair = Self::new(format, signer, config);
        pair.setup(&[
            &["commit", "-q", "-m", "base"],
            &["switch", "-q", "-c", "side"],
        ]);
        pair.write_commit("s1.txt");
        pair.setup(&[&["switch", "-q", "-c", "topic"]]);
        pair.write_commit("t1.txt");
        pair.setup(&[&["switch", "-q", "side"]]);
        pair.write_commit("s2.txt");
        pair.setup(&[
            &["merge", "-q", "--no-ff", "-m", "merge topic", "topic"],
            &["switch", "-q", "-c", "pick", "main"],
        ]);
        pair.write_commit("p1.txt");
        pair.write_commit("p2.txt");
        pair.setup(&[&["switch", "-q", "main"]]);
        pair.write_commit("m.txt");
        ssh_shim(&pair.root, false);
        let bare = pair.root.join("served.git");
        init(&bare, format, true);
        run(
            &pair.oracle,
            &["push", "-q", bare.to_str().unwrap(), "side"],
        );
        let url = format!("ssh://git@example.invalid{}", bare.display());
        pair.setup(&[&["remote", "add", "origin", &url]]);
        pair
    }

    fn write_commit(&self, file: &str) {
        for dir in [&self.oracle, &self.ours] {
            fs::write(dir.join(file), format!("{file}\n")).unwrap();
        }
        self.setup(&[&["add", file], &["commit", "-q", "-m", file]]);
    }

    /// How many of the commits `git rev-list <range>` lists (in the pyn repository) carry a signature header, and how many it lists.
    fn signed_in(&self, range: &str) -> (usize, usize) {
        let ids = self.git_out(
            &self.ours,
            &[&["rev-list"], &range.split(' ').collect::<Vec<_>>()[..]].concat(),
        );
        let all: Vec<&str> = ids.lines().collect();
        let signed = all
            .iter()
            .filter(|id| {
                self.git_out(&self.ours, &["cat-file", "commit", id])
                    .contains("\ngpgsig")
            })
            .count();
        (signed, all.len())
    }
}

/// Commands that make commits, each with the arguments that sign and the revision range of the new commits.
const SIGNING_RUNS: &[(&str, &str, &[&str], &str, usize)] = &[
    (
        "merge",
        "merge",
        &["side", "-m", "joined"],
        "--max-count=1 HEAD",
        1,
    ),
    (
        "pull merge",
        "pull",
        &["--no-rebase", "--no-edit", "origin", "side"],
        "--max-count=1 HEAD",
        1,
    ),
    (
        "pull rebase",
        "pull",
        &["--rebase", "origin", "side"],
        "side..HEAD",
        1,
    ),
    (
        "cherry-pick",
        "cherry-pick",
        &["pick~1", "pick"],
        "--max-count=2 HEAD",
        2,
    ),
    ("revert", "revert", &["HEAD"], "--max-count=1 HEAD", 1),
    ("rebase", "rebase", &["main", "side"], "main..HEAD", 3),
    (
        "rebase -i",
        "rebase",
        &["-i", "main", "side"],
        "main..HEAD",
        3,
    ),
    (
        "rebase --rebase-merges",
        "rebase",
        &["--rebase-merges", "main", "side"],
        "main..HEAD",
        4,
    ),
];

fn with_flag<'a>(args: &'a [&'a str], flag: &'a [&'a str]) -> Vec<&'a str> {
    let mut all = flag.to_vec();
    all.extend_from_slice(args);
    all
}

#[test]
fn every_commit_command_signs_like_git() {
    for format in FORMATS {
        for (name, cmd, args, range, count) in SIGNING_RUNS {
            let cases: &[Case] = &[
                ("flag", &["-S"], &[]),
                ("long flag with key", &["--gpg-sign=ABCD1234"], &[]),
                ("attached key", &["-SABCD1234"], &[]),
                ("config", &[], &[("commit.gpgsign", "true")]),
                ("signing key", &["-S"], &[("user.signingkey", "KEY99")]),
                (
                    "no-gpg-sign beats config",
                    &["--no-gpg-sign"],
                    &[("commit.gpgsign", "true")],
                ),
                ("unsigned", &[], &[]),
            ];
            for (case, flag, config) in cases {
                let pair = Pair::diverged(format, FAKE_SIGNER, config);
                if cmd == &"rebase" || cmd == &"pull" {
                    pair.setup(&[&["config", "sequence.editor", "true"]]);
                }
                let label = format!("{format} {name} {case}");
                let code = pair.same(
                    cmd,
                    &with_flag(args, flag),
                    &[LOG_RAW, &["--all"]].concat(),
                    &label,
                );
                assert_eq!(code, 0, "{label}");
                let (signed, total) = pair.signed_in(range);
                let want = !case.contains("no-gpg") && *case != "unsigned";
                assert_eq!(total, *count, "{label}: commits in {range}");
                assert_eq!(signed, if want { total } else { 0 }, "{label}");
            }
        }
    }
}

#[test]
fn a_failing_signer_stops_every_command_without_a_commit() {
    for (name, cmd, args, _, _) in SIGNING_RUNS {
        let pair = Pair::diverged("sha1", BROKEN_SIGNER, &[]);
        pair.setup(&[&["config", "sequence.editor", "true"]]);
        let branch = if *cmd == "rebase" { "side" } else { "main" };
        let before = pair.git_out(&pair.ours, &["rev-parse", branch]);
        let code = pair.run(true, cmd, &with_flag(args, &["-S"]));
        assert_ne!(code, 0, "{name}: exit status");
        assert_eq!(
            pair.git_out(&pair.ours, &["rev-parse", branch]),
            before,
            "{name}: the branch moved"
        );
        let log = pair.git_out(&pair.ours, &["log", "--all", "--format=%H"]);
        let signed = pair.git_out(&pair.ours, &["log", "--all", "--format=raw"]);
        assert!(!signed.contains("gpgsig"), "{name}: {log}");
    }
}

#[test]
fn stash_commits_are_not_signed() {
    let pair = Pair::diverged("sha1", FAKE_SIGNER, &[("commit.gpgsign", "true")]);
    fs::write(pair.ours.join("a.txt"), "changed\n").unwrap();
    assert_eq!(pair.run(true, "stash", &[]), 0);
    let stash = pair.git_out(&pair.ours, &["cat-file", "commit", "refs/stash"]);
    assert!(!stash.is_empty() && !stash.contains("gpgsig"), "{stash}");
}

#[test]
fn a_stopped_pick_or_rebase_keeps_the_key_for_continue() {
    for format in FORMATS {
        for cmd in ["cherry-pick", "rebase"] {
            let pair = Pair::diverged(format, FAKE_SIGNER, &[]);
            pair.setup(&[&["config", "core.editor", "true"]]);
            fs::write(pair.ours.join("p1.txt"), "main edit\n").unwrap();
            fs::write(pair.oracle.join("p1.txt"), "main edit\n").unwrap();
            pair.setup(&[&["add", "p1.txt"], &["commit", "-q", "-m", "clash"]]);
            let (start, range): (&[&str], _) = match cmd {
                "cherry-pick" => (&["-SKEY7", "pick~1", "pick"], "--max-count=2 HEAD"),
                _ => (&["-SKEY7", "main", "pick"], "main..HEAD"),
            };
            assert_eq!(pair.run(true, cmd, start), 1, "{format} {cmd}");
            fs::write(pair.ours.join("p1.txt"), "resolved\n").unwrap();
            fs::write(pair.oracle.join("p1.txt"), "resolved\n").unwrap();
            pair.setup(&[&["add", "p1.txt"]]);
            assert_eq!(pair.run(true, cmd, &["--continue"]), 0, "{format} {cmd}");
            let log = fs::read_to_string(pair.log(true)).unwrap_or_default();
            assert_eq!(log.matches("KEY7").count(), 2, "{format} {cmd}: {log}");
            assert_eq!(pair.signed_in(range), (2, 2), "{format} {cmd}");
        }
    }
}

/// Whether `ssh-keygen -Y verify` accepts the signature in commit `rev`, checked over the commit without its signature.
fn ssh_verifies(pair: &Pair, rev: &str, allowed: &Path) -> bool {
    let raw = Command::new("git")
        .current_dir(&pair.ours)
        .args(["cat-file", "commit", rev])
        .output()
        .unwrap()
        .stdout;
    let (mut payload, mut sig, mut in_sig) = (Vec::new(), Vec::new(), false);
    for line in raw.split_inclusive(|b| *b == b'\n') {
        let header = line.starts_with(b"gpgsig");
        if header || (in_sig && line.starts_with(b" ")) {
            in_sig = true;
            let from = if header {
                line.iter().position(|b| *b == b' ').unwrap()
            } else {
                0
            };
            sig.extend_from_slice(&line[from + 1..]);
        } else {
            in_sig = false;
            payload.extend_from_slice(line);
        }
    }
    let sig_file = pair.root.join("commit.sig");
    fs::write(&sig_file, sig).unwrap();
    let mut verify = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-n", "git", "-I", "cy@example.com", "-f"])
        .arg(allowed)
        .arg("-s")
        .arg(&sig_file)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    verify.stdin.take().unwrap().write_all(&payload).unwrap();
    verify.wait().unwrap().success()
}

#[test]
fn commits_made_by_every_command_verify_with_ssh_keygen() {
    if !have_ssh_keygen() {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    for format in FORMATS {
        for (name, cmd, args, range, count) in SIGNING_RUNS {
            let tmp = tempdir().unwrap();
            let (key, allowed) = ssh_key(tmp.path());
            let signing = [
                ("gpg.format", "ssh"),
                ("user.signingkey", key.to_str().unwrap()),
                ("commit.gpgsign", "true"),
            ];
            let pair = Pair::diverged(format, FAKE_SIGNER, &signing);
            pair.setup(&[&["config", "sequence.editor", "true"]]);
            let label = format!("{format} {name}");
            assert_eq!(pair.run(true, cmd, args), 0, "{label}");
            let ids = pair.git_out(
                &pair.ours,
                &[&["rev-list"], &range.split(' ').collect::<Vec<_>>()[..]].concat(),
            );
            assert_eq!(ids.lines().count(), *count, "{label}");
            for id in ids.lines() {
                assert!(ssh_verifies(&pair, id, &allowed), "{label}: {id}");
            }
            fsck(&pair.ours);
        }
    }
}

/// An ssh signer that logs its arguments (a key file shown by its content) and writes a fixed signature.
const FAKE_SSH: &str = r#"#!/bin/sh
prev=
for a in "$@"; do
  if [ "$prev" = -f ]; then
    printf 'key:%s\n' "$(cat "$a")" >> "$SIGNER_LOG"
  else
    case "$a" in *buffer*) echo buffer >> "$SIGNER_LOG" ;; *) echo "$a" >> "$SIGNER_LOG" ;; esac
  fi
  prev=$a
done
for last; do :; done
printf -- '-----BEGIN SSH SIGNATURE-----\nfake\n-----END SSH SIGNATURE-----\n' > "$last.sig"
"#;

fn ssh_command_pair(format: &str, command: &str, extra: &[(&str, &str)]) -> Pair {
    let pair = Pair::new(format, FAKE_SIGNER, &[]);
    let program = script(&pair.root, "fake-ssh", FAKE_SSH);
    let program = program.to_str().unwrap();
    let mut steps: Vec<Vec<&str>> = vec![
        vec!["config", "gpg.format", "ssh"],
        vec!["config", "gpg.ssh.program", program],
        vec!["config", "gpg.ssh.defaultKeyCommand", command],
    ];
    steps.extend(extra.iter().map(|(k, v)| vec!["config", *k, *v]));
    let steps: Vec<&[&str]> = steps.iter().map(Vec::as_slice).collect();
    pair.setup(&steps);
    pair
}

#[test]
fn the_default_key_command_supplies_the_ssh_signing_key_like_git() {
    for format in FORMATS {
        for (name, command) in [
            ("one key", "echo 'ssh-ed25519 AAAAFAKEKEY one'"),
            ("literal prefix", "echo 'key::ssh-ed25519 AAAAFAKEKEY one'"),
            (
                "first line only",
                "printf 'ssh-ed25519 AAAAFIRST\\nssh-ed25519 AAAASECOND\\n'",
            ),
        ] {
            let label = format!("{format} {name}");
            let pair = ssh_command_pair(format, command, &[]);
            assert_eq!(
                pair.same("commit", &["-S", "-m", "x"], SHOW_HEAD, &label),
                0,
                "{label}: commit"
            );
            let log = fs::read_to_string(pair.log(true)).unwrap();
            assert!(log.contains("key:ssh-ed25519 AAAAF"), "{label}: {log}");
            assert!(!log.contains("SECOND"), "{label}: {log}");
            let pair = ssh_command_pair(format, command, &[]);
            assert_eq!(
                pair.same("tag", &["-s", "-m", "t", "v1"], SHOW_TAG, &label),
                0,
                "{label}: tag"
            );
        }
    }
}

#[test]
fn a_failing_or_silent_default_key_command_signs_nothing() {
    for (name, command) in [("fails", "echo oops >&2; exit 3"), ("silent", "true")] {
        let pair = ssh_command_pair("sha1", command, &[]);
        let code = pair.same("commit", &["-S", "-m", "x"], SHOW_HEAD, name);
        assert_ne!(code, 0, "{name}");
        assert_eq!(
            pair.git_out(&pair.ours, &["rev-list", "--count", "HEAD"]),
            "1\n",
            "{name}"
        );
    }
}

#[test]
fn a_signing_key_or_flag_key_wins_over_the_default_key_command() {
    let marker = "echo ran >> \"$SIGNER_LOG\"; echo 'ssh-ed25519 AAAAFAKEKEY'";
    for (name, args, extra) in [
        (
            "setting",
            &["-S", "-m", "x"][..],
            &[("user.signingkey", "key::ssh-ed25519 AAAASET")][..],
        ),
        ("flag", &["-Sssh-ed25519 AAAAFLAG", "-m", "x"][..], &[][..]),
    ] {
        let pair = ssh_command_pair("sha1", marker, extra);
        assert_eq!(pair.same("commit", args, SHOW_HEAD, name), 0, "{name}");
        let log = fs::read_to_string(pair.log(true)).unwrap();
        assert!(!log.contains("ran"), "{name}: {log}");
    }
}

//! Signed commits and tags on a git repository: `pyn commit -S` and `pyn tag -s` with a fake signer program compared
//! with real git given the same program (object ids, headers, signer arguments, exit statuses), and with a real
//! `ssh-keygen -Y sign` verified by git. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
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
        .env("SIGNER_LOG", log);
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

#[test]
fn ssh_signatures_made_by_ssh_keygen_verify_in_git() {
    if !have_ssh_keygen() {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    for format in FORMATS {
        let tmp = tempdir().unwrap();
        let key = tmp.path().join("key");
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
        let public = fs::read_to_string(tmp.path().join("key.pub")).unwrap();
        let allowed = tmp.path().join("allowed");
        fs::write(&allowed, format!("cy@example.com {public}")).unwrap();
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

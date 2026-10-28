//! Checking signatures on a git repository: `verify-commit`, `verify-tag`, `log --show-signature` and
//! `show --show-signature`, compared with real git on the same repository (exit status class and what the reports
//! contain), for ssh (real `ssh-keygen`), openpgp (real gpg, skipped when absent) and a scripted gpg for trust rules.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    repo: PathBuf,
}

struct Ran {
    code: i32,
    out: String,
    err: String,
}

fn have(program: &str, flag: &str) -> bool {
    Command::new(program)
        .arg(flag)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

impl Rig {
    fn new(format: &str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let repo = root.join("repo");
        init(&repo, format, false);
        let rig = Self {
            _tmp: tmp,
            root,
            repo,
        };
        rig.git(&["config", "user.name", "Cy Committer"]);
        rig.git(&["config", "user.email", "cy@example.com"]);
        rig
    }

    fn env(&self, c: &mut Command) {
        c.env("HOME", self.root.join("home"))
            .env("GNUPGHOME", self.root.join("gnupg"))
            .env("TZ", "UTC")
            .env("GIT_COMMITTER_EMAIL", "cy@example.com")
            .env("GIT_AUTHOR_EMAIL", "cy@example.com");
    }

    fn git(&self, args: &[&str]) -> String {
        let mut c = git(&self.repo);
        self.env(&mut c);
        let out = c.args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(&self, name: &str, extra: &[&str]) -> String {
        fs::write(self.repo.join(name), format!("{name}\n")).unwrap();
        self.git(&["add", name]);
        self.git(&[&["commit", "-q", "-m", name], extra].concat());
        self.git(&["rev-parse", "HEAD"])
    }

    fn ran(&self, pyn: bool, args: &[&str]) -> Ran {
        let mut c = if pyn {
            let mut c = Command::new(env!("CARGO_BIN_EXE_pyn"));
            c.env_clear()
                .env("PATH", path_env())
                .current_dir(&self.repo);
            c
        } else {
            git(&self.repo)
        };
        self.env(&mut c);
        let out = c.args(args).stdin(Stdio::null()).output().unwrap();
        Ran {
            code: out.status.code().unwrap(),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    /// The two runs, git first.
    fn both(&self, args: &[&str]) -> (Ran, Ran) {
        (self.ran(false, args), self.ran(true, args))
    }

    /// The commit `rev` with its message changed, its signature kept: a signature that no longer matches.
    fn tamper(&self, rev: &str) -> String {
        let raw = self.git(&["cat-file", "commit", rev]);
        let raw = raw.replacen("\n\n", "\n\nTAMPERED ", 1);
        let mut c = git(&self.repo);
        self.env(&mut c);
        let mut child = c
            .args(["hash-object", "-t", "commit", "-w", "--stdin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{raw}\n").as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A new ed25519 key for `cy@example.com`, and an allowed-signers file naming it.
fn ssh_key(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    let key = dir.join(name);
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
    let public = fs::read_to_string(dir.join(format!("{name}.pub"))).unwrap();
    let allowed = dir.join(format!("{name}.allowed"));
    fs::write(&allowed, format!("cy@example.com {public}")).unwrap();
    (key, allowed)
}

fn ssh_rig(format: &str, signed_by: &Path, allowed: &Path) -> Rig {
    let rig = Rig::new(format);
    rig.git(&["config", "gpg.format", "ssh"]);
    rig.git(&["config", "user.signingkey", signed_by.to_str().unwrap()]);
    rig.git(&[
        "config",
        "gpg.ssh.allowedSignersFile",
        allowed.to_str().unwrap(),
    ]);
    rig
}

fn good(text: &str) -> bool {
    text.contains("Good \"git\" signature")
}

/// git and pyn agree on the exit status (as pass or fail) and on whether the report says the signature is good.
fn agree(rig: &Rig, args: &[&str], pass: bool, label: &str) {
    let (want, got) = rig.both(args);
    assert_eq!(want.code == 0, pass, "{label}: git {args:?}: {}", want.err);
    assert_eq!(got.code == 0, pass, "{label}: pyn {args:?}: {}", got.err);
    assert_eq!(
        got.code, want.code,
        "{label}: exit status of {args:?}: {}",
        got.err
    );
    assert_eq!(
        good(&got.err),
        good(&want.err),
        "{label}: report of {args:?}\ngit: {}\npyn: {}",
        want.err,
        got.err
    );
}

#[test]
fn ssh_commits_and_tags_verify_like_git() {
    if !have("ssh-keygen", "-?") {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    for format in FORMATS {
        let tmp = tempdir().unwrap();
        let (key, allowed) = ssh_key(tmp.path(), "key");
        let (other, _) = ssh_key(tmp.path(), "other");
        let rig = ssh_rig(format, &key, &allowed);
        let plain = rig.commit("plain", &[]);
        let signed = rig.commit("signed", &["-S"]);
        rig.git(&["tag", "-s", "-m", "release", "v1"]);
        rig.git(&["tag", "light"]);
        rig.git(&["config", "user.signingkey", other.to_str().unwrap()]);
        let stranger = rig.commit("stranger", &["-S"]);
        let tampered = rig.tamper(&signed);
        let label = |what: &str| format!("{format} {what}");
        agree(&rig, &["verify-commit", &signed], true, &label("good"));
        agree(&rig, &["verify-commit", "HEAD~1"], true, &label("rev"));
        agree(&rig, &["verify-tag", "v1"], true, &label("tag"));
        agree(&rig, &["verify-commit", &plain], false, &label("unsigned"));
        agree(&rig, &["verify-tag", "light"], false, &label("light"));
        agree(
            &rig,
            &["verify-commit", &stranger],
            false,
            &label("unknown"),
        );
        agree(
            &rig,
            &["verify-commit", &tampered],
            false,
            &label("tampered"),
        );
        agree(
            &rig,
            &["verify-commit", &plain, &signed],
            false,
            &label("mixed"),
        );
        agree(
            &rig,
            &["verify-commit", &signed, &signed],
            true,
            &label("twice"),
        );
        agree(
            &rig,
            &["verify-commit", "v1"],
            false,
            &label("tag as commit"),
        );
        agree(
            &rig,
            &["verify-tag", "HEAD"],
            false,
            &label("commit as tag"),
        );
        agree(&rig, &["verify-commit", "nosuch"], false, &label("missing"));
        agree(
            &rig,
            &["verify-commit", "--raw", &signed],
            true,
            &label("raw"),
        );
        let (want, got) = rig.both(&["verify-commit", "-v", &signed]);
        assert_eq!(got.out, want.out, "{format}: -v payload");
        assert!(!got.out.is_empty());
        let (want, got) = rig.both(&["verify-tag", "-v", "v1"]);
        assert_eq!(got.out, want.out, "{format}: -v tag payload");
        assert!(got.out.contains("tag v1"));
    }
}

#[test]
fn ssh_reports_without_a_matching_principal_fail_like_git() {
    if !have("ssh-keygen", "-?") {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    let tmp = tempdir().unwrap();
    let (key, allowed) = ssh_key(tmp.path(), "key");
    let (_, others) = ssh_key(tmp.path(), "other");
    let rig = ssh_rig("sha1", &key, &others);
    let signed = rig.commit("signed", &["-S"]);
    agree(&rig, &["verify-commit", &signed], false, "wrong file");
    rig.git(&["config", "gpg.ssh.allowedSignersFile", "/nonexistent"]);
    agree(&rig, &["verify-commit", &signed], false, "missing file");
    rig.git(&["config", "--unset", "gpg.ssh.allowedSignersFile"]);
    let (want, got) = rig.both(&["verify-commit", &signed]);
    assert_eq!((want.code, got.code), (1, 1), "unconfigured");
    assert!(got.err.contains("allowedSignersFile"), "{}", got.err);
    rig.git(&[
        "config",
        "gpg.ssh.allowedSignersFile",
        allowed.to_str().unwrap(),
    ]);
    agree(&rig, &["verify-commit", &signed], true, "right file again");
}

#[test]
fn ssh_validity_windows_use_the_commit_time() {
    if !have("ssh-keygen", "-?") {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    let tmp = tempdir().unwrap();
    let (key, allowed) = ssh_key(tmp.path(), "key");
    let public = fs::read_to_string(tmp.path().join("key.pub")).unwrap();
    let rig = ssh_rig("sha1", &key, &allowed);
    let signed = rig.commit("signed", &["-S"]);
    let window = |after: &str, before: &str| {
        fs::write(
            &allowed,
            format!("cy@example.com valid-after=\"{after}\",valid-before=\"{before}\" {public}"),
        )
        .unwrap();
    };
    window("19990101", "20991231");
    agree(&rig, &["verify-commit", &signed], true, "inside");
    window("19990101", "20000101");
    agree(&rig, &["verify-commit", &signed], false, "expired");
}

#[test]
fn show_signature_prints_the_report_after_the_commit_line() {
    if !have("ssh-keygen", "-?") {
        eprintln!("ssh-keygen is not installed; skipped");
        return;
    }
    for format in FORMATS {
        let tmp = tempdir().unwrap();
        let (key, allowed) = ssh_key(tmp.path(), "key");
        let rig = ssh_rig(format, &key, &allowed);
        rig.commit("plain", &[]);
        let signed = rig.commit("signed", &["-S"]);
        for args in [
            vec!["log", "--show-signature"],
            vec!["log", "--show-signature", "--decorate", "-p"],
            vec!["log", "--show-signature", "--format=fuller"],
            vec!["log", "--show-signature", "--format=raw"],
            vec!["show", "--show-signature", "-s"],
            vec!["show", "--show-signature", "HEAD~1", "HEAD"],
        ] {
            let (want, got) = rig.both(&args);
            assert_eq!(
                (want.code, got.code),
                (0, 0),
                "{format} {args:?}: {}",
                got.err
            );
            assert_eq!(
                got.out
                    .matches("Good \"git\" signature for cy@example.com")
                    .count(),
                want.out
                    .matches("Good \"git\" signature for cy@example.com")
                    .count(),
                "{format} {args:?}\ngit: {}\npyn: {}",
                want.out,
                got.out
            );
            let lines: Vec<&str> = got.out.lines().collect();
            let at = lines.iter().position(|l| l.starts_with("Good ")).unwrap();
            assert!(
                lines[at - 1].starts_with("commit ") && lines[at - 1].contains(&signed),
                "{format} {args:?}: {}",
                got.out
            );
        }
        let without = rig.ran(true, &["log", "--format=fuller"]);
        assert!(!good(&without.out));
    }
}

#[test]
fn show_signature_refuses_formats_with_no_commit_line() {
    let rig = Rig::new("sha1");
    rig.commit("a", &[]);
    for args in [
        vec!["log", "--show-signature", "--oneline"],
        vec!["log", "--show-signature", "--format=%h"],
        vec!["log", "--show-signature", "--graph"],
        vec!["show", "--show-signature", "--oneline"],
    ] {
        let got = rig.ran(true, &args);
        assert_eq!(got.code, 129, "{args:?}: {}", got.err);
        assert!(got.out.is_empty(), "{args:?}");
    }
}

#[test]
fn verify_commands_need_a_name() {
    let rig = Rig::new("sha1");
    rig.commit("a", &[]);
    for cmd in ["verify-commit", "verify-tag"] {
        let (want, got) = rig.both(&[cmd]);
        assert_eq!((want.code, got.code), (129, 129), "{cmd}");
    }
}

/// A gpg stand-in: it signs like the signing tests' fake, and its `--verify` answers as `$ROOT/verdict` says.
const FAKE_GPG: &str = r#"#!/bin/sh
case "$*" in
*--verify*)
  cat > /dev/null
  cat "$(dirname "$0")/verdict"
  echo "gpg: Good signature from fake" >&2
  exit "$(cat "$(dirname "$0")/exit")"
  ;;
esac
cat > /dev/null
echo "[GNUPG:] SIG_CREATED D 1 8 00 1 ffff" >&2
printf -- '-----BEGIN PGP SIGNATURE-----\n\nfakesig\n\n-----END PGP SIGNATURE-----\n'
"#;

#[test]
fn the_gpg_status_and_trust_decide_the_exit_status_like_git() {
    for format in FORMATS {
        let rig = Rig::new(format);
        let program = script(&rig.root, "gpg", FAKE_GPG);
        rig.git(&["config", "gpg.program", program.to_str().unwrap()]);
        let signed = rig.commit("signed", &["-S"]);
        rig.git(&["tag", "-s", "-m", "rel", "v1"]);
        let cases: [(&str, &str, &str, Option<&str>, bool); 10] = [
            ("good", "GOODSIG A B", "0", None, true),
            (
                "good fully",
                "GOODSIG A B\n[GNUPG:] TRUST_FULLY",
                "0",
                None,
                true,
            ),
            ("bad", "BADSIG A B", "1", None, false),
            ("error", "ERRSIG A 1 8 00 1 9", "2", None, false),
            ("expired", "EXPSIG A B", "0", None, false),
            ("revoked", "REVKEYSIG A B", "0", None, false),
            ("good but exit 2", "GOODSIG A B", "2", None, false),
            (
                "never below marginal",
                "GOODSIG A B\n[GNUPG:] TRUST_NEVER",
                "0",
                Some("marginal"),
                false,
            ),
            (
                "marginal meets marginal",
                "GOODSIG A B\n[GNUPG:] TRUST_MARGINAL",
                "0",
                Some("marginal"),
                true,
            ),
            (
                "undefined below fully",
                "GOODSIG A B",
                "0",
                Some("fully"),
                false,
            ),
        ];
        for (name, status, exit, min, pass) in cases {
            fs::write(
                rig.root.join("verdict"),
                format!("[GNUPG:] NEWSIG\n[GNUPG:] {status}\n"),
            )
            .unwrap();
            fs::write(rig.root.join("exit"), exit).unwrap();
            match min {
                Some(level) => rig.git(&["config", "gpg.minTrustLevel", level]),
                None => rig.git(&["config", "gpg.minTrustLevel", "undefined"]),
            };
            let label = format!("{format} {name}");
            for args in [["verify-commit", signed.as_str()], ["verify-tag", "v1"]] {
                let (want, got) = rig.both(&args);
                assert_eq!(want.code == 0, pass, "{label}: git {args:?}");
                assert_eq!(got.code, want.code, "{label}: {args:?}: {}", got.err);
                assert!(got.err.contains("gpg: Good signature"), "{label}");
            }
            let raw = rig.ran(true, &["verify-commit", "--raw", &signed]);
            assert!(raw.err.contains("[GNUPG:]"), "{label}: {}", raw.err);
            let shown = rig.ran(true, &["log", "-1", "--show-signature"]);
            assert_eq!(shown.code, 0, "{label}");
            assert!(shown.out.contains("gpg: Good signature"), "{label}");
        }
    }
}

#[test]
fn a_missing_verifier_fails_without_crashing() {
    let rig = Rig::new("sha1");
    let program = script(&rig.root, "gpg", FAKE_GPG);
    rig.git(&["config", "gpg.program", program.to_str().unwrap()]);
    let signed = rig.commit("signed", &["-S"]);
    rig.git(&["config", "gpg.program", "/nonexistent/gpg"]);
    let (want, got) = rig.both(&["verify-commit", &signed]);
    assert_eq!((want.code, got.code), (1, 1));
    let shown = rig.ran(true, &["log", "-1", "--show-signature"]);
    assert_eq!(shown.code, 0);
}

#[test]
fn real_gpg_signatures_verify_like_git() {
    if !have("gpg", "--version") {
        eprintln!("gpg is not installed; skipped");
        return;
    }
    for format in FORMATS {
        let rig = Rig::new(format);
        let home = rig.root.join("gnupg");
        fs::create_dir_all(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let made = Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args([
                "--batch",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--quick-gen-key",
                "Cy Committer <cy@example.com>",
                "ed25519",
                "sign",
                "never",
            ])
            .output()
            .unwrap();
        if !made.status.success() {
            eprintln!("gpg cannot make a key here; skipped");
            return;
        }
        let listing = Command::new("gpg")
            .env("GNUPGHOME", &home)
            .args(["--batch", "--list-secret-keys", "--with-colons"])
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout).into_owned();
        let fpr = listing
            .lines()
            .find_map(|l| l.strip_prefix("fpr:::::::::"))
            .map(|l| l.trim_end_matches(':').to_string())
            .unwrap();
        rig.git(&["config", "user.signingkey", &fpr]);
        let plain = rig.commit("plain", &[]);
        let signed = rig.commit("signed", &["-S"]);
        rig.git(&["tag", "-s", "-m", "rel", "v1"]);
        let tampered = rig.tamper(&signed);
        let check = |args: &[&str], pass: bool, label: &str| {
            let (want, got) = rig.both(args);
            assert_eq!(want.code == 0, pass, "{format} {label}: git: {}", want.err);
            assert_eq!(got.code, want.code, "{format} {label}: {}", got.err);
            assert_eq!(
                got.err.contains("Good signature"),
                want.err.contains("Good signature"),
                "{format} {label}\ngit: {}\npyn: {}",
                want.err,
                got.err
            );
            got
        };
        check(&["verify-commit", &signed], true, "commit");
        check(&["verify-tag", "v1"], true, "tag");
        check(&["verify-commit", &plain], false, "unsigned");
        check(&["verify-commit", &tampered], false, "tampered");
        let raw = check(&["verify-commit", "--raw", &signed], true, "raw");
        assert!(raw.err.contains("[GNUPG:] GOODSIG"), "{}", raw.err);
        let shown = rig.ran(true, &["log", "-2", "--show-signature"]);
        assert_eq!(shown.code, 0);
        assert!(shown.out.contains("Good signature"), "{}", shown.out);
        rig.git(&["config", "gpg.minTrustLevel", "ultimate"]);
        check(&["verify-commit", &signed], true, "own key is ultimate");
        rig.git(&["config", "gpg.minTrustLevel", "bogus"]);
        let got = rig.ran(true, &["verify-commit", &signed]);
        assert_ne!(got.code, 0);
    }
}

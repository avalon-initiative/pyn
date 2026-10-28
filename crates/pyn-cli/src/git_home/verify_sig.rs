//! Checking commit and tag signatures with the configured program, as git does: `verify-commit`, `verify-tag` and
//! `--show-signature` (gpg, gpgsm or `ssh-keygen -Y`).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use anyhow::{Result, bail};
use clap::Args;
use pyn_object::{Kind, ObjectId};

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes, prefix};
use super::signing::{expand_home, header_name};
use crate::commit_format::Pretty;

/// What a check found: whether it passes, the program's report and the raw status text.
pub(super) struct Checked {
    pub ok: bool,
    pub output: String,
    pub status: String,
    /// No report could be made (ssh without an allowed signers file); `log` says "No signature".
    pub unchecked: bool,
}

/// A signed object split into the signed bytes, the signature and the time it was made.
pub(super) struct Signed {
    pub payload: Vec<u8>,
    sig: Vec<u8>,
    when: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    OpenPgp,
    X509,
    Ssh,
}

const MARKERS: [(&[u8], Format); 4] = [
    (b"-----BEGIN PGP SIGNATURE-----", Format::OpenPgp),
    (b"-----BEGIN PGP MESSAGE-----", Format::OpenPgp),
    (b"-----BEGIN SIGNED MESSAGE-----", Format::X509),
    (b"-----BEGIN SSH SIGNATURE-----", Format::Ssh),
];

/// The time on the header line that starts with `who`.
fn header_time(raw: &[u8], who: &[u8]) -> i64 {
    raw.split(|b| *b == b'\n')
        .take_while(|l| !l.is_empty())
        .find(|l| l.starts_with(who))
        .and_then(|l| {
            let line = String::from_utf8_lossy(l);
            line.rsplit(' ').nth(1)?.parse().ok()
        })
        .unwrap_or(0)
}

/// The signature in a raw commit and the commit without it, or `None` when unsigned.
pub(super) fn split_commit(git: &GitHome, raw: &[u8]) -> Option<Signed> {
    let name = header_name(git.algorithm());
    let (head, body) = match raw.windows(2).position(|w| w == b"\n\n") {
        Some(at) => raw.split_at(at + 1),
        None => (raw, &raw[raw.len()..]),
    };
    let (mut payload, mut sig, mut inside) = (Vec::new(), Vec::new(), false);
    for line in head.split_inclusive(|b| *b == b'\n') {
        let first = line.starts_with(name) && line.get(name.len()) == Some(&b' ');
        if first || (inside && line.starts_with(b" ")) {
            inside = true;
            sig.extend_from_slice(&line[if first { name.len() + 1 } else { 1 }..]);
        } else {
            inside = false;
            payload.extend_from_slice(line);
        }
    }
    if sig.is_empty() {
        return None;
    }
    payload.extend_from_slice(body);
    Some(Signed {
        payload,
        sig,
        when: header_time(raw, b"committer "),
    })
}

/// The signature appended to a raw tag and the tag before it, or `None` when unsigned.
pub(super) fn split_tag(raw: &[u8]) -> Option<Signed> {
    let mut at = 0;
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if MARKERS.iter().any(|(m, _)| line.starts_with(m)) {
            return Some(Signed {
                payload: raw[..at].to_vec(),
                sig: raw[at..].to_vec(),
                when: header_time(raw, b"tagger "),
            });
        }
        at += line.len();
    }
    None
}

fn format_of(sig: &[u8]) -> Format {
    MARKERS
        .iter()
        .find(|(m, _)| sig.starts_with(m))
        .map_or(Format::OpenPgp, |(_, f)| *f)
}

/// Runs `cmd` with `input` on standard input.
fn run(mut cmd: Command, input: &[u8]) -> std::io::Result<Output> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped");
    let data = input.to_vec();
    let feeder = std::thread::spawn(move || stdin.write_all(&data));
    let out = child.wait_with_output()?;
    let _ = feeder.join();
    Ok(out)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn report(out: &Output) -> String {
    text(&out.stdout) + &text(&out.stderr)
}

fn min_trust(git: &GitHome) -> Result<u8> {
    Ok(match git.config().get("gpg.mintrustlevel") {
        None => 0,
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "undefined" => 0,
            "never" => 1,
            "marginal" => 2,
            "fully" => 3,
            "ultimate" => 4,
            _ => bail!("invalid value for 'gpg.minTrustLevel': '{v}'"),
        },
    })
}

/// Checks `signed` with the program its signature format names.
pub(super) fn check(git: &GitHome, signed: &Signed) -> Result<Checked> {
    let min = min_trust(git)?;
    let dir = tempfile::tempdir()?;
    let sig_file = dir.path().join("sig");
    std::fs::write(&sig_file, &signed.sig)?;
    match format_of(&signed.sig) {
        Format::Ssh => check_ssh(git, signed, &sig_file),
        format => check_gpg(git, format, signed, &sig_file, min),
    }
}

fn unrunnable(program: &str, e: &std::io::Error) -> Checked {
    eprintln!("error: cannot run {program}: {e}");
    Checked {
        ok: false,
        output: String::new(),
        status: String::new(),
        unchecked: false,
    }
}

fn check_gpg(
    git: &GitHome,
    format: Format,
    signed: &Signed,
    sig_file: &Path,
    min: u8,
) -> Result<Checked> {
    let config = git.config();
    let program = if format == Format::X509 {
        config.get("gpg.x509.program").unwrap_or("gpgsm")
    } else {
        config
            .get("gpg.openpgp.program")
            .or_else(|| config.get("gpg.program"))
            .unwrap_or("gpg")
    };
    let mut cmd = Command::new(program);
    cmd.args(["--keyid-format=long", "--status-fd=1", "--verify"])
        .arg(sig_file)
        .arg("-");
    let out = match run(cmd, &signed.payload) {
        Ok(out) => out,
        Err(e) => return Ok(unrunnable(program, &e)),
    };
    let status = text(&out.stdout);
    let (mut result, mut trust) = (None, 0u8);
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("[GNUPG:] ") else {
            continue;
        };
        let word = rest.split(' ').next().unwrap_or("");
        match word {
            "GOODSIG" | "BADSIG" | "ERRSIG" | "EXPSIG" | "EXPKEYSIG" | "REVKEYSIG" => {
                result.get_or_insert(word);
            }
            "TRUST_UNDEFINED" => trust = 0,
            "TRUST_NEVER" => trust = 1,
            "TRUST_MARGINAL" => trust = 2,
            "TRUST_FULLY" => trust = 3,
            "TRUST_ULTIMATE" => trust = 4,
            _ => {}
        }
    }
    Ok(Checked {
        ok: out.status.success()
            && result == Some("GOODSIG")
            && format!("\n{status}").contains("\n[GNUPG:] GOODSIG ")
            && trust >= min,
        output: text(&out.stderr),
        status,
        unchecked: false,
    })
}

fn check_ssh(git: &GitHome, signed: &Signed, sig_file: &Path) -> Result<Checked> {
    let config = git.config();
    let program = config.get("gpg.ssh.program").unwrap_or("ssh-keygen");
    let allowed = config
        .get("gpg.ssh.allowedsignersfile")
        .filter(|f| !f.is_empty())
        .map(expand_home);
    let Some(allowed) = allowed else {
        eprintln!(
            "error: gpg.ssh.allowedSignersFile needs to be configured and exist for ssh signature verification"
        );
        return Ok(Checked {
            ok: false,
            output: String::new(),
            status: String::new(),
            unchecked: true,
        });
    };
    let time = chrono::DateTime::from_timestamp(signed.when, 0)
        .map(|t| format!("-Overify-time={}Z", t.format("%Y%m%d%H%M%S")));
    let ssh = |sub: &str| {
        let mut c = Command::new(program);
        c.args(["-Y", sub]);
        c
    };
    let mut principals = Vec::new();
    let mut cmd = ssh("find-principals");
    cmd.arg("-f").arg(&allowed).arg("-s").arg(sig_file);
    cmd.args(&time);
    let out = match run(cmd, b"") {
        Ok(out) => out,
        Err(e) => return Ok(unrunnable(program, &e)),
    };
    let find_err = text(&out.stderr);
    if out.status.success() {
        principals = text(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
    } else if find_err.contains("usage:") {
        bail!(
            "ssh-keygen -Y find-principals/verify is needed for ssh signature verification (available in openssh 8.2p1+)"
        );
    }
    let mut last = String::new();
    for principal in &principals {
        let mut cmd = ssh("verify");
        cmd.args(["-n", "git", "-f"]);
        cmd.arg(&allowed);
        cmd.args(["-I", principal, "-s"]).arg(sig_file);
        if let Some(rev) = config
            .get("gpg.ssh.revocationfile")
            .filter(|r| !r.is_empty())
        {
            cmd.arg("-r").arg(expand_home(rev));
        }
        cmd.args(&time);
        let out = match run(cmd, &signed.payload) {
            Ok(out) => out,
            Err(e) => return Ok(unrunnable(program, &e)),
        };
        last = report(&out);
        if out.status.success() {
            return Ok(Checked {
                ok: true,
                status: last.clone(),
                output: last,
                unchecked: false,
            });
        }
    }
    if principals.is_empty() {
        let mut cmd = ssh("check-novalidate");
        cmd.args(["-n", "git", "-s"]).arg(sig_file);
        let out = match run(cmd, &signed.payload) {
            Ok(out) => out,
            Err(e) => return Ok(unrunnable(program, &e)),
        };
        last = report(&out);
        if out.status.success() {
            last += &find_err;
        }
    }
    Ok(Checked {
        ok: false,
        status: last.clone(),
        output: last,
        unchecked: false,
    })
}

/// The verification report `--show-signature` prints for a commit, if it is signed.
pub(super) fn commit_report(git: &GitHome, id: &ObjectId) -> Result<Option<String>> {
    let Some(object) = git.repo.objects().read_opt(id)? else {
        return Ok(None);
    };
    let Some(signed) = split_commit(git, &object.data) else {
        return Ok(None);
    };
    let checked = check(git, &signed)?;
    Ok(Some(if checked.unchecked {
        "No signature\n".to_string()
    } else {
        checked.output
    }))
}

/// Refuses the formats that print no `commit` line to put the report after.
pub(super) fn check_format(pretty: &Pretty) {
    if matches!(pretty, Pretty::Oneline { .. } | Pretty::User { .. }) {
        crate::usage_error("`--show-signature` needs a commit format with a `commit` line");
    }
}

/// Inserts a signature report after the `commit` line, as git prints it.
pub(super) fn with_report(mut text: String, report: &str) -> String {
    if !text.contains('\n') {
        text.push('\n');
    }
    let at = text.find('\n').map_or(text.len(), |i| i + 1);
    text.insert_str(at, report);
    text
}

/// Check the GPG or SSH signature of commits.
#[derive(Debug, Args)]
pub struct VerifyCommitArgs {
    /// Print the commit contents.
    #[arg(short, long)]
    verbose: bool,
    /// Print the raw status output of the program.
    #[arg(long)]
    raw: bool,
    /// The commits to verify.
    #[arg(required = true, value_name = "COMMIT")]
    commits: Vec<String>,
}

/// Check the GPG or SSH signature of tags.
#[derive(Debug, Args)]
pub struct VerifyTagArgs {
    /// Print the tag contents.
    #[arg(short, long)]
    verbose: bool,
    /// Print the raw status output of the program.
    #[arg(long)]
    raw: bool,
    /// The tags to verify.
    #[arg(required = true, value_name = "TAG")]
    tags: Vec<String>,
}

pub(super) fn verify_commit(git: &GitHome, cwd: &Path, args: VerifyCommitArgs) -> Result<()> {
    verify_each(
        git,
        cwd,
        &args.commits,
        Kind::Commit,
        (args.verbose, args.raw),
    )
}

pub(super) fn verify_tag(git: &GitHome, cwd: &Path, args: VerifyTagArgs) -> Result<()> {
    verify_each(git, cwd, &args.tags, Kind::Tag, (args.verbose, args.raw))
}

fn verify_each(
    git: &GitHome,
    cwd: &Path,
    names: &[String],
    want: Kind,
    (verbose, raw): (bool, bool),
) -> Result<()> {
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let (noun, other) = if want == Kind::Commit {
        ("commit", "non-commit")
    } else {
        ("tag", "non-tag")
    };
    let mut failed = false;
    for name in names {
        let object = match resolver.object(name)? {
            Some(id) => git.repo.objects().read_opt(&id)?,
            None => None,
        };
        let Some(object) = object else {
            eprintln!("error: {noun} '{name}' not found.");
            failed = true;
            continue;
        };
        if object.kind != want {
            eprintln!(
                "error: {name}: cannot verify a {other} object of type {}.",
                object.kind.name()
            );
            failed = true;
            continue;
        }
        let signed = if want == Kind::Commit {
            split_commit(git, &object.data)
        } else {
            split_tag(&object.data)
        };
        let Some(signed) = signed else {
            failed = true;
            continue;
        };
        let checked = check(git, &signed)?;
        if verbose {
            emit_bytes(&signed.payload);
        }
        eprint!(
            "{}",
            if raw {
                &checked.status
            } else {
                &checked.output
            }
        );
        failed |= !checked.ok;
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

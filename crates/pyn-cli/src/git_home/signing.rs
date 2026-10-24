//! Commit and tag signatures made by an external program, called as git calls it (gpg, gpgsm or `ssh-keygen -Y sign`).

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use pyn_object::{Algorithm, Commit, Signature};

use super::GitHome;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    OpenPgp,
    X509,
    Ssh,
}

/// A configured signer: the format, the program and the key to sign with.
pub(super) struct Signer {
    format: Format,
    program: String,
    key: String,
}

/// What `-S` / `--no-gpg-sign` asked for on the command line.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct SignFlag {
    /// Sign the commits with `user.signingkey` or the given key id (git repository only).
    #[arg(short = 'S', long, value_name = "KEYID", num_args = 0..=1, require_equals = true, overrides_with = "no_gpg_sign")]
    pub gpg_sign: Option<Option<String>>,
    /// Do not sign, whatever `commit.gpgsign` says (git repository only).
    #[arg(long, overrides_with = "gpg_sign")]
    pub no_gpg_sign: bool,
}

impl SignFlag {
    /// The flag that was given, for the refusal where nothing signs.
    pub fn first_given(&self) -> Option<&'static str> {
        [
            ("--gpg-sign", self.gpg_sign.is_some()),
            ("--no-gpg-sign", self.no_gpg_sign),
        ]
        .into_iter()
        .find_map(|(f, on)| on.then_some(f))
    }

    /// The `-S[<keyid>]` form git keeps in a rebase's `gpg_sign_opt`.
    pub(super) fn saved(&self) -> Option<String> {
        self.gpg_sign
            .as_ref()
            .map(|k| format!("-S{}", k.as_deref().unwrap_or_default()))
    }

    /// Reads back what [`SignFlag::saved`] wrote.
    pub(super) fn load(text: &str) -> Self {
        let key = text.trim().strip_prefix("-S");
        Self {
            gpg_sign: key.map(|k| (!k.is_empty()).then(|| k.to_string())),
            no_gpg_sign: false,
        }
    }
}

/// The signer for a commit or tag, or `None` when it is not to be signed: the flags win over `config_key`.
pub(super) fn signer_for(
    git: &GitHome,
    flag: &SignFlag,
    config_key: &str,
    who: Option<&Signature>,
) -> Result<Option<Signer>> {
    let (on, key) = match (&flag.gpg_sign, flag.no_gpg_sign) {
        (_, true) => return Ok(None),
        (Some(key), false) => (true, key.clone()),
        (None, false) => (git.repo.bool_setting(config_key, false), None),
    };
    if !on {
        return Ok(None);
    }
    let config = git.repo.config();
    let format = match config.get("gpg.format").unwrap_or("openpgp") {
        "openpgp" => Format::OpenPgp,
        "x509" => Format::X509,
        "ssh" => Format::Ssh,
        other => bail!("invalid value for 'gpg.format': '{other}'"),
    };
    let setting = config
        .get("user.signingkey")
        .filter(|k| !k.is_empty())
        .map(str::to_string);
    let key = key.filter(|k| !k.is_empty()).or(setting);
    let program = match format {
        Format::OpenPgp => config
            .get("gpg.openpgp.program")
            .or_else(|| config.get("gpg.program"))
            .unwrap_or("gpg"),
        Format::X509 => config.get("gpg.x509.program").unwrap_or("gpgsm"),
        Format::Ssh => config.get("gpg.ssh.program").unwrap_or("ssh-keygen"),
    }
    .to_string();
    let key = match (key, format) {
        (Some(k), _) => k,
        (None, Format::Ssh) => bail!(
            "user.signingkey needs to be set for ssh signing (gpg.ssh.defaultKeyCommand is not built)"
        ),
        (None, _) => {
            let who = who.ok_or_else(|| anyhow!("unable to determine who you are to sign as"))?;
            format!(
                "{} <{}>",
                String::from_utf8_lossy(&who.name),
                String::from_utf8_lossy(&who.email)
            )
        }
    };
    Ok(Some(Signer {
        format,
        program,
        key,
    }))
}

/// Adds the signature header to `record` when `flag` or `commit.gpgsign` asks for one.
pub(super) fn sign_commit(git: &GitHome, flag: &SignFlag, record: &mut Commit) -> Result<()> {
    let who = record.committer.clone();
    if let Some(signer) = signer_for(git, flag, "commit.gpgsign", Some(&who))? {
        let sig = signer.sign(&record.encode(git.algorithm())?)?;
        record
            .extra_headers
            .push((header_name(git.algorithm()).to_vec(), header_value(sig)));
    }
    Ok(())
}

impl Signer {
    /// The detached signature over `payload`, with `\r` removed as git does.
    pub(super) fn sign(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut sig = match self.format {
            Format::Ssh => self.sign_ssh(payload)?,
            _ => self.sign_gpg(payload)?,
        };
        sig.retain(|b| *b != b'\r');
        if sig.is_empty() {
            bail!("failed to sign the data: the signer produced no signature");
        }
        Ok(sig)
    }

    fn sign_gpg(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let mut child = Command::new(&self.program)
            .args(["--status-fd=2", "-bsau", &self.key])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("cannot run {} to sign the data", self.program))?;
        let mut stdin = child.stdin.take().expect("piped");
        let data = payload.to_vec();
        let feeder = std::thread::spawn(move || stdin.write_all(&data));
        let out = child.wait_with_output()?;
        let _ = feeder.join();
        let status = format!("\n{}", String::from_utf8_lossy(&out.stderr));
        if !out.status.success() || !status.contains("\n[GNUPG:] SIG_CREATED ") {
            bail!("gpg failed to sign the data:\n{}", status.trim());
        }
        Ok(out.stdout)
    }

    fn sign_ssh(&self, payload: &[u8]) -> Result<Vec<u8>> {
        let dir = tempfile::tempdir().context("cannot make a temporary directory for signing")?;
        let buffer = dir.path().join("buffer");
        std::fs::write(&buffer, payload)?;
        let mut cmd = Command::new(&self.program);
        cmd.args(["-Y", "sign", "-n", "git", "-f"]);
        if self.key.starts_with("ssh-") || self.key.starts_with("key::") {
            let key = dir.path().join("key.pub");
            let literal = self.key.strip_prefix("key::").unwrap_or(&self.key);
            std::fs::write(&key, literal)?;
            cmd.arg(&key).arg("-U");
        } else {
            cmd.arg(expand_home(&self.key));
        }
        let out = cmd
            .arg(&buffer)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("cannot run {} to sign the data", self.program))?;
        let signed = dir.path().join("buffer.sig");
        if !out.status.success() {
            let text = String::from_utf8_lossy(&out.stderr);
            if text.contains("usage:") {
                bail!("ssh-keygen -Y sign is needed for ssh signing (available in openssh 8.2p1+)");
            }
            bail!("failed to sign the data:\n{}", text.trim());
        }
        std::fs::read(&signed).map_err(|e| anyhow!("failed to read the ssh signature: {e}"))
    }
}

fn expand_home(path: &str) -> std::path::PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => std::path::Path::new(&home).join(rest),
        _ => path.into(),
    }
}

/// The header git puts a commit signature in: `gpgsig`, or `gpgsig-sha256` in a SHA-256 repository.
pub(super) fn header_name(algorithm: Algorithm) -> &'static [u8] {
    match algorithm {
        Algorithm::Sha1 => b"gpgsig",
        _ => b"gpgsig-sha256",
    }
}

/// A signature as a header value: the text without its final newline (folding adds the leading blanks).
pub(super) fn header_value(sig: Vec<u8>) -> Vec<u8> {
    let mut sig = sig;
    if sig.last() == Some(&b'\n') {
        sig.pop();
    }
    sig
}

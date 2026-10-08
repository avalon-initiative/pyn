//! Stored sign-ins, kept per server in the user's configuration directory and readable only by the owner.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub user: String,
    pub token: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    servers: BTreeMap<String, Entry>,
}

/// `$PYN_CONFIG_DIR`, else `$XDG_CONFIG_HOME/pyn`, else `~/.config/pyn`.
fn dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("PYN_CONFIG_DIR") {
        return Ok(dir.into());
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("pyn"));
    }
    let home = std::env::var("HOME")
        .context("cannot find a configuration directory: set HOME or PYN_CONFIG_DIR")?;
    Ok(PathBuf::from(home).join(".config").join("pyn"))
}

fn file_path() -> Result<PathBuf> {
    Ok(dir()?.join("credentials.toml"))
}

fn read() -> File {
    file_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

fn write(file: &File) -> Result<()> {
    let dir = dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = file_path()?;
    let text = toml::to_string_pretty(file)?;
    write_private(&path, &text)
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(text.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, text: &str) -> Result<()> {
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn load(server: &str) -> Option<Entry> {
    read().servers.remove(server)
}

pub fn save(server: &str, entry: Entry) -> Result<()> {
    let mut file = read();
    file.servers.insert(server.to_string(), entry);
    write(&file)
}

/// Forgets the sign-in for `server`; false if there was none.
pub fn remove(server: &str) -> Result<bool> {
    let mut file = read();
    let had = file.servers.remove(server).is_some();
    if had {
        write(&file)?;
    }
    Ok(had)
}

/// The token id inside a `pyn_<id>_<secret>` string.
pub fn token_id(token: &str) -> Option<&str> {
    token
        .strip_prefix("pyn_")?
        .split_once('_')
        .map(|(id, _)| id)
}

/// A password from `--password-stdin` (one line) or a hidden prompt.
pub fn read_password(prompt: &str, from_stdin: bool) -> Result<String> {
    if from_stdin {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        let line = line.trim_end_matches(['\r', '\n']).to_string();
        if line.is_empty() {
            bail!("no password on standard input");
        }
        return Ok(line);
    }
    Ok(rpassword::prompt_password(prompt)?)
}

/// Asks for a new password twice.
pub fn read_new_password(from_stdin: bool) -> Result<String> {
    let first = read_password("New password: ", from_stdin)?;
    if !from_stdin && rpassword::prompt_password("Repeat it: ")? != first {
        bail!("the passwords do not match");
    }
    Ok(first)
}

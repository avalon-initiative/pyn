//! Stored sign-ins, kept per server in the user's configuration directory and readable only by the owner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
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

const FILE_NAME: &str = "credentials.toml";

/// `$PYN_CONFIG_DIR`, else `$XDG_CONFIG_HOME/pyn`, else `~/.config/pyn`.
pub fn dir() -> Result<PathBuf> {
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

/// A missing file is empty; an unreadable or corrupt one is an error so a save cannot overwrite it.
fn read_in(dir: &Path) -> Result<File> {
    let path = dir.join(FILE_NAME);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(File::default()),
        Err(e) => bail!("cannot read {}: {}", path.display(), e.kind()),
    };
    toml::from_str(&text).map_err(|e| {
        let line = e.span().map_or(String::new(), |s| {
            format!(" near line {}", text[..s.start].matches('\n').count() + 1)
        });
        anyhow!(
            "{} is not a valid credentials file{line}; fix or remove it (nothing was changed)",
            path.display()
        )
    })
}

fn write_in(dir: &Path, file: &File) -> Result<()> {
    create_private_dir(dir)?;
    let text = toml::to_string_pretty(file)?;
    write_private(dir, &dir.join(FILE_NAME), &text)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting {}", dir.display()))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

/// Writes a private temp file beside `path`, syncs it and renames it into place.
fn write_private(dir: &Path, path: &Path, text: &str) -> Result<()> {
    use std::io::Write;

    let tmp = dir.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let _ = std::fs::remove_file(&tmp);
    let result = (|| -> Result<()> {
        let mut file = opts
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return result;
    }
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

const LOCK_NAME: &str = ".credentials.lock";
const LOCK_WAIT: Duration = Duration::from_secs(10);
const LOCK_STALE: Duration = Duration::from_secs(30);

/// An advisory lock around the credentials read-modify-write: a lock file made with `create_new`,
/// removed on drop. One older than `LOCK_STALE` is taken as left by a crashed process and broken.
struct Lock {
    path: PathBuf,
}

impl Lock {
    fn acquire(dir: &Path) -> Result<Lock> {
        let path = dir.join(LOCK_NAME);
        let started = Instant::now();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Lock { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let age = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if age.is_some_and(|a| a > LOCK_STALE) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() > LOCK_WAIT {
                        bail!(
                            "another pyn command is updating {}; remove {} if none is running",
                            dir.join(FILE_NAME).display(),
                            path.display()
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    return Err(e).with_context(|| format!("locking {}", path.display()));
                }
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// No saved sign-in when there is no configuration directory; a damaged file is an error.
pub fn load(server: &str) -> Result<Option<Entry>> {
    let Ok(dir) = dir() else {
        return Ok(None);
    };
    Ok(read_in(&dir)?.servers.remove(server))
}

pub fn save(server: &str, entry: Entry) -> Result<()> {
    save_in(&dir()?, server, entry)
}

fn save_in(dir: &Path, server: &str, entry: Entry) -> Result<()> {
    create_private_dir(dir)?;
    let _lock = Lock::acquire(dir)?;
    let mut file = read_in(dir)?;
    file.servers.insert(server.to_string(), entry);
    write_in(dir, &file)
}

/// Forgets the sign-in for `server`; false if there was none.
pub fn remove(server: &str) -> Result<bool> {
    remove_in(&dir()?, server)
}

fn remove_in(dir: &Path, server: &str) -> Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    let _lock = Lock::acquire(dir)?;
    let mut file = read_in(dir)?;
    let had = file.servers.remove(server).is_some();
    if had {
        write_in(dir, &file)?;
    }
    Ok(had)
}

/// Whether sending secrets to `server` would travel in clear text to a non-loopback host.
pub fn is_cleartext_remote(server: &str) -> bool {
    use reqwest::Url;

    let Ok(url) = Url::parse(server) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    host.parse::<std::net::IpAddr>()
        .map_or(true, |ip| !ip.is_loopback())
}

/// Prints the clear-text warning for `server` when it applies.
pub fn warn_if_cleartext(server: &str) {
    if is_cleartext_remote(server) {
        eprintln!(
            "warning: {server} is not using TLS; passwords and tokens sent to it are visible on the network. Use an https:// server."
        );
    }
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

/// The token for `--token`: the value itself, or one line of standard input for `-`. A value given on the
/// command line (`on_command_line`) is visible in the process list, so it earns a warning.
pub fn resolve_token(
    value: Option<String>,
    on_command_line: bool,
    stdin: &mut impl std::io::BufRead,
    warn: &mut impl std::io::Write,
) -> Result<Option<String>> {
    match value.as_deref() {
        Some("-") => {
            let mut line = String::new();
            stdin.read_line(&mut line)?;
            let line = line.trim_end_matches(['\r', '\n']).to_string();
            if line.is_empty() {
                bail!("no token on standard input");
            }
            Ok(Some(line))
        }
        Some(_) if on_command_line => {
            writeln!(
                warn,
                "warning: a token on the command line is visible in the process list; use `--token -` to read it from standard input, or PYN_TOKEN"
            )?;
            Ok(value)
        }
        _ => Ok(value),
    }
}

/// Asks for a new password twice.
pub fn read_new_password(from_stdin: bool) -> Result<String> {
    let first = read_password("New password: ", from_stdin)?;
    if !from_stdin && rpassword::prompt_password("Repeat it: ")? != first {
        bail!("the passwords do not match");
    }
    Ok(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(token: &str) -> Entry {
        Entry {
            user: "alice".into(),
            token: token.into(),
        }
    }

    #[test]
    fn a_missing_file_is_empty_and_save_then_remove_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cfg");
        assert!(read_in(&dir).unwrap().servers.is_empty());
        save_in(&dir, "https://a", entry("pyn_1_x")).unwrap();
        save_in(&dir, "https://b", entry("pyn_2_y")).unwrap();
        assert_eq!(read_in(&dir).unwrap().servers.len(), 2);
        assert!(remove_in(&dir, "https://a").unwrap());
        assert!(!remove_in(&dir, "https://a").unwrap());
        assert_eq!(read_in(&dir).unwrap().servers["https://b"].token, "pyn_2_y");
    }

    #[test]
    fn a_corrupt_file_is_an_error_that_save_and_remove_leave_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(FILE_NAME);
        let bad = "[servers.\"https://a\"]\nuser = \"u\"\ntoken = pyn_SECRETVALUE\n";
        std::fs::write(&path, bad).unwrap();
        let e = read_in(tmp.path()).err().unwrap().to_string();
        assert!(e.contains(FILE_NAME), "{e}");
        assert!(!e.contains("SECRETVALUE"), "{e}");
        assert!(save_in(tmp.path(), "https://b", entry("t")).is_err());
        assert!(remove_in(tmp.path(), "https://a").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
    }

    #[test]
    fn an_unreadable_path_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(FILE_NAME)).unwrap();
        assert!(read_in(tmp.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_and_file_are_private_and_existing_ones_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a").join("cfg");
        save_in(&dir, "https://a", entry("t")).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(FILE_NAME)), 0o600);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(dir.join(FILE_NAME), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        save_in(&dir, "https://b", entry("t")).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join(FILE_NAME)), 0o600);
    }

    #[test]
    fn writes_replace_the_file_and_leave_no_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..3 {
            save_in(tmp.path(), &format!("https://s{i}"), entry("t")).unwrap();
        }
        let names: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, [FILE_NAME]);
    }

    #[test]
    fn a_failed_write_keeps_the_previous_file() {
        let tmp = tempfile::tempdir().unwrap();
        save_in(tmp.path(), "https://a", entry("t")).unwrap();
        let before = std::fs::read_to_string(tmp.path().join(FILE_NAME)).unwrap();
        let blocker = tmp
            .path()
            .join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
        std::fs::create_dir(&blocker).unwrap();
        assert!(save_in(tmp.path(), "https://b", entry("t")).is_err());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(FILE_NAME)).unwrap(),
            before
        );
    }

    #[test]
    fn concurrent_saves_lose_no_entry_and_leave_no_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let dir = dir.clone();
                std::thread::spawn(move || {
                    save_in(&dir, &format!("https://s{i}"), entry("t")).unwrap()
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(read_in(&dir).unwrap().servers.len(), 8);
        assert!(!dir.join(LOCK_NAME).exists());
    }

    #[test]
    fn a_stale_lock_is_broken_and_a_fresh_one_is_waited_for() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join(LOCK_NAME);
        let stale = std::fs::File::create(&lock).unwrap();
        stale
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(120))
            .unwrap();
        save_in(tmp.path(), "https://a", entry("t")).unwrap();
        assert!(!lock.exists());

        std::fs::File::create(&lock).unwrap();
        let path = lock.clone();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            std::fs::remove_file(path).unwrap();
        });
        save_in(tmp.path(), "https://b", entry("t")).unwrap();
        release.join().unwrap();
        assert_eq!(read_in(tmp.path()).unwrap().servers.len(), 2);
    }

    #[test]
    fn a_token_of_dash_is_read_from_stdin_and_a_command_line_value_warns() {
        let mut warn = Vec::new();
        let got = resolve_token(
            Some("-".into()),
            true,
            &mut "pyn_1_x\nrest".as_bytes(),
            &mut warn,
        );
        assert_eq!(got.unwrap().as_deref(), Some("pyn_1_x"));
        assert!(warn.is_empty());
        assert!(resolve_token(Some("-".into()), true, &mut "".as_bytes(), &mut warn).is_err());

        let got = resolve_token(Some("pyn_2_y".into()), true, &mut "".as_bytes(), &mut warn);
        assert_eq!(got.unwrap().as_deref(), Some("pyn_2_y"));
        assert!(
            String::from_utf8(warn.clone())
                .unwrap()
                .contains("--token -")
        );

        warn.clear();
        let got = resolve_token(Some("pyn_3_z".into()), false, &mut "".as_bytes(), &mut warn);
        assert_eq!(got.unwrap().as_deref(), Some("pyn_3_z"));
        assert!(warn.is_empty());
        assert_eq!(
            resolve_token(None, true, &mut "".as_bytes(), &mut warn).unwrap(),
            None
        );
    }

    #[test]
    fn only_non_loopback_http_is_flagged() {
        for s in [
            "http://localhost:7878",
            "http://127.0.0.1:7878",
            "http://127.1.2.3",
            "http://[::1]:7878",
            "https://example.com",
            "not a url",
        ] {
            assert!(!is_cleartext_remote(s), "{s}");
        }
        for s in [
            "http://example.com",
            "http://10.0.0.5:7878",
            "http://[2001:db8::1]",
        ] {
            assert!(is_cleartext_remote(s), "{s}");
        }
    }
}

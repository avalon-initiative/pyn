//! The `store` credential helper in process: git's `~/.git-credentials` format, one URL per line.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::Credential;
use crate::env::Env;
use crate::error::Result;
use crate::url::decode;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Entry {
    protocol: String,
    host: String,
    path: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

/// The files `store` reads: the one named by `--file`, else `~/.git-credentials` then `$XDG_CONFIG_HOME/git/credentials`.
pub struct StoreFiles(Vec<PathBuf>);

impl StoreFiles {
    pub fn new(file: Option<&str>, env: &Env) -> Self {
        let home = env.non_empty("HOME").map(PathBuf::from);
        if let Some(f) = file {
            let path = match (f.strip_prefix("~/"), &home) {
                (Some(rest), Some(h)) => h.join(rest),
                _ => PathBuf::from(f),
            };
            return Self(vec![path]);
        }
        let mut files = Vec::new();
        if let Some(h) = &home {
            files.push(h.join(".git-credentials"));
        }
        let xdg = env
            .non_empty("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        if let Some(x) = xdg {
            files.push(x.join("git/credentials"));
        }
        Self(files)
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.0
    }

    /// The first stored entry that covers `query`, as a credential.
    pub fn get(&self, query: &Credential) -> Option<Credential> {
        for path in &self.0 {
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            for entry in text.lines().filter_map(parse_line) {
                if matches(query, &entry, false) {
                    let mut c = query.clone();
                    c.username = entry.username;
                    c.password = entry.password;
                    return Some(c);
                }
            }
        }
        None
    }

    /// Replaces any entry for the same protocol, host, path and user, in the first file that exists (else the first).
    pub fn store(&self, cred: &Credential) -> Result<()> {
        let (Some(_), Some(_)) = (&cred.username, &cred.password) else {
            return Ok(());
        };
        let Some(target) = self.0.iter().find(|p| p.exists()).or(self.0.first()) else {
            return Ok(());
        };
        let new = entry_of(cred);
        let mut lines = read_kept(target, |e| !same_target(&new, e));
        lines.push(format_entry(&new));
        write_private(target, &lines)
    }

    /// Removes the entries equal to `cred` (password included) from every file.
    pub fn erase(&self, cred: &Credential) -> Result<()> {
        for path in self.0.iter().filter(|p| p.exists()) {
            let before = fs::read_to_string(path).unwrap_or_default().lines().count();
            let kept = read_kept(path, |e| !matches(cred, e, true));
            if kept.len() != before {
                write_private(path, &kept)?;
            }
        }
        Ok(())
    }
}

fn entry_of(c: &Credential) -> Entry {
    Entry {
        protocol: c.protocol.clone(),
        host: c.host.clone(),
        path: c.path.clone(),
        username: c.username.clone(),
        password: c.password.clone(),
    }
}

/// Lines of `path` that are not entries, plus the entries `keep` accepts.
fn read_kept(path: &Path, keep: impl Fn(&Entry) -> bool) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|l| parse_line(l).is_none_or(|e| keep(&e)))
        .map(str::to_owned)
        .collect()
}

fn same_target(a: &Entry, b: &Entry) -> bool {
    a.protocol == b.protocol && a.host == b.host && a.path == b.path && a.username == b.username
}

/// Git's `credential_match`: each field of the query must equal the entry's, and a field the query lacks is free.
fn matches(q: &Credential, e: &Entry, with_password: bool) -> bool {
    let field = |want: &Option<String>, have: &Option<String>| match want {
        None => true,
        Some(w) => have.as_ref() == Some(w),
    };
    q.protocol == e.protocol
        && q.host == e.host
        && field(&q.path, &e.path)
        && field(&q.username, &e.username)
        && (!with_password || field(&q.password, &e.password))
}

fn parse_line(line: &str) -> Option<Entry> {
    let (protocol, rest) = line.trim().split_once("://")?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], Some(decode(&rest[i + 1..]))),
        None => (rest, None),
    };
    let (userinfo, host) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    if protocol.is_empty() || host.is_empty() {
        return None;
    }
    let (username, password) = match userinfo {
        None => (None, None),
        Some(u) => match u.split_once(':') {
            Some((n, p)) => (Some(decode(n)), Some(decode(p))),
            None => (Some(decode(u)), None),
        },
    };
    Some(Entry {
        protocol: protocol.to_owned(),
        host: host.to_owned(),
        path: path.filter(|p| !p.is_empty()),
        username,
        password,
    })
}

fn encode(s: &str, keep: impl Fn(u8) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || keep(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn format_entry(e: &Entry) -> String {
    let mut out = format!("{}://", e.protocol);
    if let Some(u) = &e.username {
        out.push_str(&encode(u, |_| false));
        if let Some(p) = &e.password {
            out.push(':');
            out.push_str(&encode(p, |_| false));
        }
        out.push('@');
    }
    out.push_str(&e.host);
    if let Some(p) = &e.path {
        out.push('/');
        out.push_str(&encode(p, |b| b"/!$&'()*+,;=:@".contains(&b)));
    }
    out
}

#[cfg(unix)]
fn write_private(path: &Path, lines: &[String]) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    for l in lines {
        writeln!(f, "{l}")?;
    }
    f.flush()?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, lines: &[String]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut text = lines.join("\n");
    text.push('\n');
    fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(user: &str, pw: &str) -> Credential {
        Credential {
            protocol: "https".into(),
            host: "example.com".into(),
            username: Some(user.into()),
            password: Some(pw.into()),
            ..Credential::default()
        }
    }

    fn files(dir: &Path) -> StoreFiles {
        StoreFiles::new(Some(dir.join("creds").to_str().unwrap()), &Env::default())
    }

    #[test]
    fn lines_parse_and_format_with_percent_encoding() {
        let e = parse_line("https://a%40b:p%3Ass%2F@example.com:8443/team/r.git").unwrap();
        assert_eq!(e.username.as_deref(), Some("a@b"));
        assert_eq!(e.password.as_deref(), Some("p:ss/"));
        assert_eq!(e.host, "example.com:8443");
        assert_eq!(e.path.as_deref(), Some("team/r.git"));
        assert_eq!(
            format_entry(&e),
            "https://a%40b:p%3Ass%2F@example.com:8443/team/r.git"
        );
        assert_eq!(parse_line("https://example.com").unwrap().username, None);
        assert!(parse_line("not a url").is_none());
        assert!(parse_line("# comment").is_none());
    }

    #[test]
    fn store_get_replace_and_erase() {
        let tmp = tempfile::tempdir().unwrap();
        let f = files(tmp.path());
        let query = Credential {
            protocol: "https".into(),
            host: "example.com".into(),
            ..Credential::default()
        };
        assert!(f.get(&query).is_none());
        f.store(&cred("alice", "one")).unwrap();
        f.store(&cred("bob", "two")).unwrap();
        f.store(&cred("alice", "three")).unwrap();
        let got = f.get(&query).unwrap();
        assert_eq!(got.username.as_deref(), Some("bob"));
        let mut q = query.clone();
        q.username = Some("alice".into());
        assert_eq!(f.get(&q).unwrap().password.as_deref(), Some("three"));
        let text = fs::read_to_string(&f.paths()[0]).unwrap();
        assert_eq!(text.lines().count(), 2);

        f.erase(&cred("alice", "wrong")).unwrap();
        assert!(f.get(&q).is_some());
        f.erase(&cred("alice", "three")).unwrap();
        assert!(f.get(&q).is_none());
        assert!(f.get(&query).is_some());
    }

    #[test]
    fn path_in_the_query_must_match_the_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let f = files(tmp.path());
        let mut c = cred("alice", "pw");
        c.path = Some("team/a.git".into());
        f.store(&c).unwrap();
        let mut q = Credential {
            protocol: "https".into(),
            host: "example.com".into(),
            ..Credential::default()
        };
        assert!(f.get(&q).is_some());
        q.path = Some("team/b.git".into());
        assert!(f.get(&q).is_none());
        q.path = Some("team/a.git".into());
        assert!(f.get(&q).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let f = files(tmp.path());
        f.store(&cred("a", "b")).unwrap();
        let mode = fs::metadata(&f.paths()[0]).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn default_files_follow_home_and_xdg() {
        let env = Env::default()
            .with("HOME", "/h")
            .with("XDG_CONFIG_HOME", "/x");
        let f = StoreFiles::new(None, &env);
        assert_eq!(
            f.paths(),
            [
                PathBuf::from("/h/.git-credentials"),
                PathBuf::from("/x/git/credentials")
            ]
        );
        let env = Env::default().with("HOME", "/h");
        assert_eq!(
            StoreFiles::new(None, &env).paths()[1],
            PathBuf::from("/h/.config/git/credentials")
        );
        assert_eq!(
            StoreFiles::new(Some("~/c"), &env).paths(),
            [PathBuf::from("/h/c")]
        );
    }
}

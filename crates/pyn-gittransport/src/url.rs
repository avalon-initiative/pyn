//! Remote URLs: parsing, redaction, `insteadOf` rewriting and `<section>.<url>.<key>` scoping.

use std::path::PathBuf;

use pyn_git::Config;
use reqwest::Url;

use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshUrl {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    /// The path as the remote command receives it (`~user/x` loses the URL's leading slash).
    pub path: String,
    pub scp_like: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteUrl {
    Http(Url),
    Ssh(SshUrl),
    /// A directory on this machine, from a plain path or a `file://` URL; `shown` is the text as given.
    Local {
        path: PathBuf,
        shown: String,
    },
}

impl RemoteUrl {
    pub fn parse(s: &str) -> Result<Self> {
        if let Some((scheme, _)) = s.split_once("://") {
            return parse_scheme(&scheme.to_ascii_lowercase(), s);
        }
        match scp_split(s) {
            Some((userhost, path)) => ssh_from_parts(userhost, None, path.to_owned(), true),
            None => Ok(Self::Local {
                path: PathBuf::from(s),
                shown: s.to_owned(),
            }),
        }
    }

    /// The URL without credentials, for messages.
    pub fn display(&self) -> String {
        match self {
            Self::Http(u) => anonymized(u),
            Self::Ssh(s) => s.display(),
            Self::Local { shown, .. } => shown.clone(),
        }
    }
}

impl SshUrl {
    pub fn display(&self) -> String {
        let user = self
            .user
            .as_deref()
            .map(|u| format!("{u}@"))
            .unwrap_or_default();
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.scp_like {
            return format!("{user}{host}:{}", self.path);
        }
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let slash = if self.path.starts_with('/') { "" } else { "/" };
        format!("ssh://{user}{host}{port}{slash}{}", self.path)
    }
}

fn anonymized(u: &Url) -> String {
    let mut u = u.clone();
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.set_fragment(None);
    u.to_string()
}

fn parse_scheme(scheme: &str, s: &str) -> Result<RemoteUrl> {
    match scheme {
        "http" | "https" => {
            let url = Url::parse(s).map_err(|_| invalid(s))?;
            if url.host_str().is_none_or(str::is_empty) {
                return Err(invalid(s));
            }
            Ok(RemoteUrl::Http(url))
        }
        "ssh" | "git+ssh" | "ssh+git" => {
            let url = Url::parse(s).map_err(|_| invalid(s))?;
            let host = url
                .host_str()
                .filter(|h| !h.is_empty())
                .ok_or_else(|| invalid(s))?;
            let host = host.trim_start_matches('[').trim_end_matches(']');
            let user = Some(decode(url.username())).filter(|u| !u.is_empty());
            let decoded = decode(url.path());
            let path = match decoded.strip_prefix("/~") {
                Some(rest) => format!("~{rest}"),
                None => decoded,
            };
            ssh_from_parts(
                &match &user {
                    Some(u) => format!("{u}@{host}"),
                    None => host.to_owned(),
                },
                url.port(),
                path,
                false,
            )
        }
        "file" => {
            let url = Url::parse(s).map_err(|_| invalid(s))?;
            let local_host = url.host_str().is_none_or(|h| h.is_empty() || h == "localhost");
            let path = url.to_file_path().ok().filter(|_| local_host);
            match path {
                Some(path) => Ok(RemoteUrl::Local {
                    path,
                    shown: s.to_owned(),
                }),
                None => Err(invalid(s)),
            }
        }
        "git" => Err(Error::Url(
            "the git:// protocol is not supported: it is unauthenticated and unencrypted; use https:// or ssh://"
                .into(),
        )),
        other => Err(Error::Url(format!("unsupported URL scheme '{other}'"))),
    }
}

fn invalid(s: &str) -> Error {
    Error::Url(format!("invalid URL '{}'", redact_text(s)))
}

fn ssh_from_parts(userhost: &str, port: Option<u16>, path: String, scp: bool) -> Result<RemoteUrl> {
    let (user, host) = match userhost.rsplit_once('@') {
        Some((u, h)) => (Some(u.to_owned()), h),
        None => (None, userhost),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || path.is_empty() {
        return Err(Error::Url(format!(
            "invalid ssh remote '{}': it needs a host and a path",
            redact_text(userhost)
        )));
    }
    if host.starts_with('-') || user.as_deref().is_some_and(|u| u.starts_with('-')) {
        return Err(Error::Url(
            "refusing an ssh host or user name that starts with '-'".into(),
        ));
    }
    Ok(RemoteUrl::Ssh(SshUrl {
        user,
        host: host.to_owned(),
        port,
        path,
        scp_like: scp,
    }))
}

/// `host:path` split, when the part before the first colon holds no slash (git's scp-like rule).
fn scp_split(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s
        .strip_prefix('[')
        .or_else(|| s.split_once('@').and_then(|(_, r)| r.strip_prefix('[')))
    {
        let close = rest.find("]:")?;
        let start = s.len() - rest.len();
        return Some((&s[..start + close + 1], &s[start + close + 2..]));
    }
    let colon = s.find(':')?;
    let slash = s.find('/');
    if slash.is_some_and(|i| i < colon) {
        return None;
    }
    Some((&s[..colon], &s[colon + 1..]))
}

pub(crate) fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Replaces the credentials of every `scheme://userinfo@` in `text`, for messages that may quote a URL.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(at) => out.push_str(&authority[at + 1..]),
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Applies `url.<base>.insteadOf` (and `pushInsteadOf` first when pushing): the longest matching prefix wins.
pub fn rewrite(config: &Config, url: &str, push: bool) -> String {
    let pick = |key: &str| {
        config
            .entries()
            .iter()
            .filter(|e| e.section == "url" && e.key == key)
            .filter_map(|e| Some((e.subsection.as_deref()?, e.value.as_deref()?)))
            .filter(|(_, prefix)| !prefix.is_empty() && url.starts_with(prefix))
            .max_by_key(|(_, prefix)| prefix.len())
            .map(|(base, prefix)| format!("{base}{}", &url[prefix.len()..]))
    };
    (push.then(|| pick("pushinsteadof")).flatten())
        .or_else(|| pick("insteadof"))
        .unwrap_or_else(|| url.to_owned())
}

/// How specifically a config subsection such as `https://example.com/team` covers `url`, or `None`.
pub fn scope_score(pattern: &str, url: &Url) -> Option<usize> {
    let (scheme, rest) = match pattern.split_once("://") {
        Some((s, r)) => (Some(s.to_ascii_lowercase()), r),
        None => (None, pattern),
    };
    if scheme.as_deref().is_some_and(|s| s != url.scheme()) {
        return None;
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (user, hostport) = match authority.rsplit_once('@') {
        Some((u, h)) => (Some(u), h),
        None => (None, authority),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            (h, p.parse::<u16>().ok())
        }
        _ => (hostport, None),
    };
    let url_host = url.host_str()?.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    let host_ok = match host.strip_prefix("*.") {
        Some(suffix) => url_host.ends_with(&format!(".{suffix}")),
        None => host == url_host,
    };
    if !host_ok {
        return None;
    }
    let port_ok = match port {
        Some(p) => Some(p) == url.port_or_known_default(),
        None => url.port().is_none(),
    };
    if !port_ok {
        return None;
    }
    if let Some(u) = user
        && u != decode(url.username())
    {
        return None;
    }
    let path = path.trim_end_matches('/');
    let url_path = url.path();
    if !path.is_empty()
        && !(url_path == path
            || url_path
                .strip_prefix(path)
                .is_some_and(|r| r.starts_with('/')))
    {
        return None;
    }
    Some(1 + path.len() * 2 + usize::from(user.is_some()))
}

/// The values of `<section>.<key>` and of `<section>.<url>.<key>` for each `<url>` covering `url`, in file order;
/// an empty value clears what came before.
pub fn scoped_values<'a>(config: &'a Config, section: &str, key: &str, url: &Url) -> Vec<&'a str> {
    let mut out = Vec::new();
    for e in config.entries() {
        if e.section != section || e.key != key {
            continue;
        }
        if let Some(sub) = &e.subsection
            && scope_score(sub, url).is_none()
        {
            continue;
        }
        let v = e.value.as_deref().unwrap_or("true");
        if v.is_empty() {
            out.clear();
        } else {
            out.push(v);
        }
    }
    out
}

/// The single value of `<section>.<key>` for `url`: the most specific scope wins, the last of equals.
pub fn scoped_value<'a>(
    config: &'a Config,
    section: &str,
    key: &str,
    url: &Url,
) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for e in config.entries() {
        if e.section != section || e.key != key {
            continue;
        }
        let score = match &e.subsection {
            None => 0,
            Some(sub) => match scope_score(sub, url) {
                Some(s) => s,
                None => continue,
            },
        };
        if best.is_none_or(|(b, _)| score >= b) {
            best = Some((score, e.value.as_deref().unwrap_or("true")));
        }
    }
    best.map(|(_, v)| v)
}

pub(crate) fn is_false(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "false" | "no" | "off" | "0" | ""
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(s: &str) -> SshUrl {
        match RemoteUrl::parse(s).unwrap() {
            RemoteUrl::Ssh(u) => u,
            other => panic!("not ssh: {other:?}"),
        }
    }

    #[test]
    fn paths_and_file_urls_are_local() {
        let local = |s: &str| match RemoteUrl::parse(s).unwrap() {
            RemoteUrl::Local { path, shown } => (path, shown),
            other => panic!("not local: {other:?}"),
        };
        assert_eq!(local("/srv/r").0, PathBuf::from("/srv/r"));
        assert_eq!(local("../r").0, PathBuf::from("../r"));
        let (path, shown) = local("file://localhost/srv/my%20repo");
        assert_eq!(path, PathBuf::from("/srv/my repo"));
        assert_eq!(shown, "file://localhost/srv/my%20repo");
        assert!(RemoteUrl::parse("file://example.com/srv/r").is_err());
    }

    #[test]
    fn http_urls_parse_and_never_display_credentials() {
        let u = RemoteUrl::parse("https://user:s3cret@example.com:8443/a/b.git").unwrap();
        assert_eq!(u.display(), "https://example.com:8443/a/b.git");
        let u = RemoteUrl::parse("https://tok3n@example.com/a.git").unwrap();
        assert_eq!(u.display(), "https://example.com/a.git");
        assert!(RemoteUrl::parse("http://").is_err());
    }

    #[test]
    fn ssh_forms() {
        let u = ssh("ssh://git@host.example:2222/srv/repo.git");
        assert_eq!(
            (u.user.as_deref(), u.host.as_str(), u.port, u.path.as_str()),
            (Some("git"), "host.example", Some(2222), "/srv/repo.git")
        );
        assert_eq!(u.display(), "ssh://git@host.example:2222/srv/repo.git");
        let u = ssh("git+ssh://host/x");
        assert_eq!((u.user, u.path.as_str()), (None, "/x"));
        assert_eq!(ssh("ssh+git://host/x").host, "host");
        let u = ssh("git@github.com:owner/repo.git");
        assert!(u.scp_like);
        assert_eq!(
            (u.user.as_deref(), u.host.as_str(), u.path.as_str()),
            (Some("git"), "github.com", "owner/repo.git")
        );
        assert_eq!(u.display(), "git@github.com:owner/repo.git");
        assert_eq!(ssh("host:/abs/path").path, "/abs/path");
        assert_eq!(ssh("ssh://host/~alice/repo").path, "~alice/repo");
        assert_eq!(ssh("ssh://host/a%20b").path, "/a b");
    }

    #[test]
    fn ssh_ipv6_hosts() {
        let u = ssh("ssh://[::1]:2200/r");
        assert_eq!((u.host.as_str(), u.port), ("::1", Some(2200)));
        assert_eq!(u.display(), "ssh://[::1]:2200/r");
        let u = ssh("git@[fe80::1]:repo.git");
        assert_eq!((u.host.as_str(), u.path.as_str()), ("fe80::1", "repo.git"));
    }

    #[test]
    fn ssh_options_cannot_be_smuggled_in() {
        assert!(RemoteUrl::parse("ssh://-oProxyCommand=x/p").is_err());
        assert!(RemoteUrl::parse("-oProxyCommand=x:path").is_err());
        assert!(RemoteUrl::parse("host:").is_err());
    }

    #[test]
    fn paths_parse_as_local_and_other_schemes_are_refused() {
        for local in [
            "/srv/repo",
            "./repo",
            "../repo",
            "repo",
            "file:///srv/repo",
            "a/b:c",
        ] {
            assert!(
                matches!(RemoteUrl::parse(local), Ok(RemoteUrl::Local { .. })),
                "{local}"
            );
        }
        assert!(matches!(
            RemoteUrl::parse("git://host/x"),
            Err(Error::Url(_))
        ));
        assert!(RemoteUrl::parse("ftp://host/x").is_err());
    }

    #[test]
    fn invalid_url_errors_do_not_echo_credentials() {
        let e = RemoteUrl::parse("https://user:pw@/x")
            .unwrap_err()
            .to_string();
        assert!(!e.contains("pw"), "{e}");
    }

    #[test]
    fn redact_text_strips_userinfo_everywhere() {
        assert_eq!(
            redact_text("GET https://u:p@h.example/x failed; see 'http://tok@h2/y'"),
            "GET https://h.example/x failed; see 'http://h2/y'"
        );
        assert_eq!(redact_text("no urls here"), "no urls here");
        assert_eq!(redact_text("https://host/a@b"), "https://host/a@b");
    }

    #[test]
    fn instead_of_picks_the_longest_prefix_and_push_variant() {
        let config = Config::parse(
            b"[url \"git@github.com:\"]\n\tinsteadOf = https://github.com/\n\
              [url \"git@github.com:org/\"]\n\tinsteadOf = https://github.com/org/\n\
              [url \"ssh://push.example/\"]\n\tpushInsteadOf = https://github.com/\n",
        )
        .unwrap();
        assert_eq!(
            rewrite(&config, "https://github.com/org/r.git", false),
            "git@github.com:org/r.git"
        );
        assert_eq!(
            rewrite(&config, "https://github.com/x/r.git", false),
            "git@github.com:x/r.git"
        );
        assert_eq!(
            rewrite(&config, "https://github.com/x/r.git", true),
            "ssh://push.example/x/r.git"
        );
        assert_eq!(rewrite(&config, "https://other/x", true), "https://other/x");
    }

    #[test]
    fn scopes_match_by_host_port_and_path() {
        let url = Url::parse("https://git.example.com:8443/team/repo.git").unwrap();
        let score = |p: &str| scope_score(p, &url);
        assert!(score("https://git.example.com:8443").is_some());
        assert!(score("https://git.example.com").is_none());
        assert!(score("http://git.example.com:8443").is_none());
        assert!(score("https://*.example.com:8443").is_some());
        assert!(
            score("https://git.example.com:8443/team").unwrap()
                > score("https://git.example.com:8443").unwrap()
        );
        assert!(score("https://git.example.com:8443/tea").is_none());
        let plain = Url::parse("https://example.com/x").unwrap();
        assert!(scope_score("example.com", &plain).is_some());
        assert!(scope_score("https://example.com:443", &plain).is_some());
    }

    #[test]
    fn scoped_values_follow_file_order_and_resets() {
        let url = Url::parse("https://example.com/r").unwrap();
        let config = Config::parse(
            b"[credential]\n\thelper = a\n[credential \"https://example.com\"]\n\thelper = b\n\
              [credential \"https://other.com\"]\n\thelper = c\n",
        )
        .unwrap();
        assert_eq!(
            scoped_values(&config, "credential", "helper", &url),
            ["a", "b"]
        );
        let config = Config::parse(
            b"[credential]\n\thelper = a\n[credential \"https://example.com\"]\n\thelper =\n\thelper = b\n",
        )
        .unwrap();
        assert_eq!(scoped_values(&config, "credential", "helper", &url), ["b"]);
        let config = Config::parse(
            b"[http]\n\tproxy = p0\n[http \"https://example.com/r\"]\n\tproxy = p2\n[http \"https://example.com\"]\n\tproxy = p1\n",
        )
        .unwrap();
        assert_eq!(scoped_value(&config, "http", "proxy", &url), Some("p2"));
    }
}

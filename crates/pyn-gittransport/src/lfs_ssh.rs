//! The LFS endpoint of an ssh remote, found the way git-lfs does: the remote's `git-lfs-authenticate` answers with the
//! URL and the headers to use, which lapse after `expires_in` or `expires_at`.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pyn_git::Config;
use reqwest::Url;
use serde::Deserialize;

use crate::env::Env;
use crate::error::Result;
use crate::lfs::{endpoint, lfs};
use crate::ssh::{SshTransport, shell_quote};
use crate::url::{RemoteUrl, SshUrl, rewrite};

/// Headers are renewed this long before they lapse.
const RENEW_BEFORE: Duration = Duration::from_secs(5);

/// Runs a command on the remote and returns its standard output.
pub trait Exec: Send + Sync {
    fn exec(&self, command: &str) -> Result<Vec<u8>>;
}

impl Exec for SshTransport {
    fn exec(&self, command: &str) -> Result<Vec<u8>> {
        SshTransport::exec(self, command)
    }
}

/// What `git-lfs-authenticate` granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub href: Url,
    pub header: Vec<(String, String)>,
    pub expires: Option<SystemTime>,
}

#[derive(Deserialize)]
struct Answer {
    href: String,
    #[serde(default)]
    header: BTreeMap<String, String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    expires_at: Option<String>,
}

/// Reads the JSON of `git-lfs-authenticate`; `expires_in` counts from `now`, and a lapse of zero means none.
pub fn parse_grant(json: &[u8], now: SystemTime) -> Result<Grant> {
    let a: Answer = serde_json::from_slice(json).map_err(|e| {
        lfs(format!(
            "git-lfs-authenticate sent an unreadable answer: {e}"
        ))
    })?;
    let href = Url::parse(&a.href)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .ok_or_else(|| lfs("git-lfs-authenticate sent an href that is not an http(s) URL"))?;
    let expires = match (a.expires_in.filter(|n| *n > 0), a.expires_at.as_deref()) {
        (Some(n), _) => Some(now + Duration::from_secs(n)),
        (None, Some(at)) => Some(
            parse_rfc3339(at).ok_or_else(|| lfs("git-lfs-authenticate sent a bad expires_at"))?,
        ),
        (None, None) => None,
    };
    Ok(Grant {
        href,
        header: a.header.into_iter().collect(),
        expires,
    })
}

/// `YYYY-MM-DDTHH:MM:SS[.fff](Z|+HH:MM|-HH:MM)`.
fn parse_rfc3339(s: &str) -> Option<SystemTime> {
    let b = s.as_bytes();
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse().ok() };
    if b.len() < 20 || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let (y, mo, d, h, mi, sec) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    let mut i = 19;
    if b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let offset = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() => {
            let secs = num(i + 1, i + 3)? * 3600 + num(i + 4, i + 6)? * 60;
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };
    let (y, m) = if mo <= 2 {
        (y - 1, mo + 9)
    } else {
        (y, mo - 3)
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + h * 3600 + mi * 60 + sec - offset;
    u64::try_from(secs)
        .ok()
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

/// Asks the remote for a grant for `operation` (`download` or `upload`).
pub fn authenticate(
    exec: &dyn Exec,
    url: &SshUrl,
    operation: &str,
    now: SystemTime,
) -> Result<Grant> {
    let out = exec.exec(&format!(
        "git-lfs-authenticate {} {operation}",
        shell_quote(url.path.strip_prefix('/').unwrap_or(&url.path))
    ))?;
    parse_grant(&out, now)
}

/// `https://<host>/<path>.git/info/lfs`: what git-lfs uses when the remote cannot authenticate.
pub fn https_fallback(url: &SshUrl) -> Result<Url> {
    let path = url.path.trim_matches('/');
    let git = if path.ends_with(".git") { "" } else { ".git" };
    let host = if url.host.contains(':') {
        format!("[{}]", url.host)
    } else {
        url.host.clone()
    };
    Url::parse(&format!("https://{host}/{path}{git}/info/lfs")).map_err(|_| {
        lfs(format!(
            "no LFS endpoint can be derived from '{}'",
            url.display()
        ))
    })
}

/// How a [`Grant`] is renewed.
pub struct Renewal {
    exec: Box<dyn Exec>,
    url: SshUrl,
    operation: String,
}

impl Renewal {
    pub fn new(exec: Box<dyn Exec>, url: SshUrl, operation: &str) -> Self {
        Self {
            exec,
            url,
            operation: operation.to_owned(),
        }
    }

    pub fn grant(&self, now: SystemTime) -> Result<Grant> {
        authenticate(&*self.exec, &self.url, &self.operation, now)
    }
}

/// True once `expires` is within the renewal margin of `now`.
pub fn lapsing(expires: Option<SystemTime>, now: SystemTime) -> bool {
    expires.is_some_and(|e| now + RENEW_BEFORE >= e)
}

/// An LFS endpoint with what the remote granted for it, if anything.
pub struct Resolved {
    pub base: Url,
    pub grant: Option<Grant>,
    pub renewal: Option<Renewal>,
}

/// Like [`endpoint`], and for an ssh remote with no explicit setting asks `git-lfs-authenticate` through
/// `exec_for`, falling back to the https derivation when that fails.
pub fn resolve_with(
    config: &Config,
    remote: Option<&str>,
    remote_url: Option<&str>,
    push: bool,
    operation: &str,
    exec_for: &dyn Fn(&SshUrl) -> Box<dyn Exec>,
) -> Result<Resolved> {
    let plain = |base| Resolved {
        base,
        grant: None,
        renewal: None,
    };
    let first = endpoint(config, remote, remote_url, push);
    let ssh = match remote_url
        .map(|u| RemoteUrl::parse(&rewrite(config, u, push)))
        .transpose()?
    {
        Some(RemoteUrl::Ssh(s)) if !has_explicit(config, remote, push) => s,
        _ => return first.map(plain),
    };
    let renewal = Renewal::new(exec_for(&ssh), ssh.clone(), operation);
    match renewal.grant(SystemTime::now()) {
        Ok(grant) => Ok(Resolved {
            base: grant.href.clone(),
            grant: Some(grant),
            renewal: Some(renewal),
        }),
        Err(_) => https_fallback(&ssh).map(plain),
    }
}

/// [`resolve_with`] over the system ssh client, chosen as the git transport chooses it.
pub fn resolve(
    config: &Config,
    env: &Env,
    remote: Option<&str>,
    remote_url: Option<&str>,
    push: bool,
    operation: &str,
) -> Result<Resolved> {
    let exec_for =
        |u: &SshUrl| -> Box<dyn Exec> { Box::new(SshTransport::new(u.clone(), config, env)) };
    resolve_with(config, remote, remote_url, push, operation, &exec_for)
}

fn has_explicit(config: &Config, remote: Option<&str>, push: bool) -> bool {
    let keyed = |k: &str| remote.and_then(|r| config.get(&format!("remote.{r}.{k}")));
    let set = |v: Option<&str>| v.is_some_and(|v| !v.is_empty());
    (push && (set(keyed("lfspushurl")) || set(config.get("lfs.pushurl"))))
        || set(keyed("lfsurl"))
        || set(config.get("lfs.url"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    const T0: SystemTime = UNIX_EPOCH;

    struct Fake {
        answer: std::result::Result<String, String>,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Exec for Fake {
        fn exec(&self, command: &str) -> Result<Vec<u8>> {
            self.log.lock().unwrap().push(command.to_owned());
            self.answer.clone().map(String::into_bytes).map_err(lfs)
        }
    }

    fn cfg(s: &str) -> Config {
        Config::parse(s.as_bytes()).unwrap()
    }

    fn run(
        config: &str,
        url: &str,
        push: bool,
        answer: std::result::Result<&str, &str>,
    ) -> (Result<Resolved>, Vec<String>, SshUrl) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l = log.clone();
        let answer = answer.map(str::to_owned).map_err(str::to_owned);
        let op = if push { "upload" } else { "download" };
        let r = resolve_with(
            &cfg(config),
            Some("origin"),
            Some(url),
            push,
            op,
            &move |_| {
                Box::new(Fake {
                    answer: answer.clone(),
                    log: l.clone(),
                })
            },
        );
        let u = match RemoteUrl::parse(url).unwrap() {
            RemoteUrl::Ssh(s) => s,
            _ => SshUrl {
                user: None,
                host: String::new(),
                port: None,
                path: String::new(),
                scp_like: false,
            },
        };
        let calls = log.lock().unwrap().clone();
        (r, calls, u)
    }

    const OK: &str = r#"{"href":"https://lfs.example/org/repo.git/info/lfs","header":{"Authorization":"RemoteAuth abc"},"expires_in":600}"#;

    #[test]
    fn parses_href_headers_and_expiry() {
        let g = parse_grant(OK.as_bytes(), T0).unwrap();
        assert_eq!(g.href.as_str(), "https://lfs.example/org/repo.git/info/lfs");
        assert_eq!(
            g.header,
            vec![("Authorization".into(), "RemoteAuth abc".into())]
        );
        assert_eq!(g.expires, Some(T0 + Duration::from_secs(600)));
        let g = parse_grant(br#"{"href":"https://h/x"}"#, T0).unwrap();
        assert!(g.header.is_empty() && g.expires.is_none());
    }

    #[test]
    fn expires_at_is_rfc3339_and_expires_in_wins() {
        let at = |s: &str| {
            parse_grant(
                format!(r#"{{"href":"https://h/x","expires_at":"{s}"}}"#).as_bytes(),
                T0,
            )
            .map(|g| g.expires)
        };
        let secs = |t: Option<SystemTime>| t.unwrap().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs(at("2038-01-19T03:14:07Z").unwrap()), 2_147_483_647);
        assert_eq!(
            secs(at("2038-01-19T05:14:07.250+02:00").unwrap()),
            2_147_483_647
        );
        assert_eq!(secs(at("2000-03-01T00:00:00-01:00").unwrap()), 951_872_400);
        assert!(at("tomorrow").is_err());
        let both = br#"{"href":"https://h/x","expires_in":30,"expires_at":"2038-01-19T03:14:07Z"}"#;
        assert_eq!(
            parse_grant(both, T0).unwrap().expires,
            Some(T0 + Duration::from_secs(30))
        );
    }

    #[test]
    fn rejects_bad_answers() {
        assert!(parse_grant(b"not json", T0).is_err());
        assert!(parse_grant(br#"{"header":{}}"#, T0).is_err());
        assert!(parse_grant(br#"{"href":"ssh://h/x"}"#, T0).is_err());
    }

    #[test]
    fn lapsing_keeps_a_margin() {
        let e = Some(T0 + Duration::from_secs(100));
        assert!(!lapsing(e, T0 + Duration::from_secs(90)));
        assert!(lapsing(e, T0 + Duration::from_secs(95)));
        assert!(!lapsing(None, T0 + Duration::from_secs(1_000_000)));
    }

    #[test]
    fn the_command_quotes_the_path_for_both_url_forms() {
        for url in [
            "git@host.example:org/my repo.git",
            "ssh://git@host.example/org/my repo.git",
        ] {
            let (r, calls, _) = run("", url, false, Ok(OK));
            let r = r.unwrap();
            assert_eq!(
                calls,
                vec!["git-lfs-authenticate 'org/my repo.git' download"]
            );
            assert_eq!(r.base.as_str(), "https://lfs.example/org/repo.git/info/lfs");
            assert_eq!(r.grant.unwrap().header[0].0, "Authorization");
        }
        let (_, calls, _) = run("", "ssh://git@host.example:2222/org/repo.git", true, Ok(OK));
        assert_eq!(calls, vec!["git-lfs-authenticate 'org/repo.git' upload"]);
    }

    #[test]
    fn explicit_settings_skip_the_command() {
        for c in [
            "[lfs]\n\turl = https://lfs.example/a\n",
            "[remote \"origin\"]\n\tlfsurl = https://lfs.example/a\n",
        ] {
            let (r, calls, _) = run(c, "git@host.example:org/repo.git", false, Ok(OK));
            assert_eq!(r.unwrap().base.as_str(), "https://lfs.example/a");
            assert!(calls.is_empty());
        }
        let c = "[lfs]\n\tpushurl = https://push.example/a\n";
        let (r, calls, _) = run(c, "git@host.example:org/repo.git", true, Ok(OK));
        assert_eq!(r.unwrap().base.as_str(), "https://push.example/a");
        assert!(calls.is_empty());
        let (_, calls, _) = run(c, "git@host.example:org/repo.git", false, Ok(OK));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn instead_of_can_turn_a_remote_into_ssh_or_http() {
        let c = "[url \"git@host.example:\"]\n\tinsteadOf = h:\n";
        let (r, calls, _) = run(c, "h:org/repo.git", false, Ok(OK));
        assert!(r.unwrap().grant.is_some());
        assert_eq!(calls.len(), 1);
        let c = "[url \"https://web.example/\"]\n\tinsteadOf = git@host.example:\n";
        let (r, calls, _) = run(c, "git@host.example:org/repo", false, Ok(OK));
        let r = r.unwrap();
        assert_eq!(r.base.as_str(), "https://web.example/org/repo.git/info/lfs");
        assert!(r.grant.is_none() && calls.is_empty());
    }

    #[test]
    fn http_remotes_never_run_a_command() {
        let (r, calls, _) = run("", "https://h.example/team/game", false, Ok(OK));
        assert_eq!(
            r.unwrap().base.as_str(),
            "https://h.example/team/game.git/info/lfs"
        );
        assert!(calls.is_empty());
    }

    #[test]
    fn a_failing_command_falls_back_to_https() {
        for url in [
            "git@host.example:org/repo",
            "ssh://git@host.example:2222/org/repo.git",
        ] {
            let (r, calls, _) = run("", url, false, Err("no such command"));
            let r = r.unwrap();
            assert_eq!(calls.len(), 1);
            assert!(r.grant.is_none() && r.renewal.is_none());
            assert!(
                r.base.as_str() == "https://host.example/org/repo.git/info/lfs",
                "{}",
                r.base
            );
        }
        let (r, _, _) = run("", "git@host.example:org/repo", false, Ok("garbage"));
        assert_eq!(
            r.unwrap().base.as_str(),
            "https://host.example/org/repo.git/info/lfs"
        );
    }

    #[test]
    fn a_renewal_asks_again() {
        let (r, calls, _) = run("", "git@host.example:org/repo.git", true, Ok(OK));
        let g = r
            .unwrap()
            .renewal
            .unwrap()
            .grant(T0 + Duration::from_secs(700))
            .unwrap();
        assert_eq!(g.expires, Some(T0 + Duration::from_secs(1300)));
        assert_eq!(calls.len(), 1);
    }
}

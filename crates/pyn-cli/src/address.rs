//! Parsing `owner/name` repository names and `<server>/owner/name` clone sources.

use std::path::Path;

use anyhow::{Result, bail};

/// Where a clone comes from: the server it names (if any) and the repository.
#[derive(Debug, PartialEq, Eq)]
pub struct Source {
    pub server: Option<String>,
    pub repo: String,
}

/// Splits `owner/name`, refusing anything else.
pub fn split_repo(text: &str) -> Result<(&str, &str)> {
    match text.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
            Ok((owner, name))
        }
        _ => bail!("{text:?} is not a repository name; use owner/name"),
    }
}

/// Splits `org/team`, refusing anything else.
pub fn split_team(text: &str) -> Result<(&str, &str)> {
    match text.split_once('/') {
        Some((org, slug)) if !org.is_empty() && !slug.is_empty() && !slug.contains('/') => {
            Ok((org, slug))
        }
        _ => bail!("{text:?} is not a team name; use org/team"),
    }
}

/// `http(s)://host[:port][/prefix]/owner/name` or a bare `owner/name`; the last two path segments are the repository.
pub fn parse_source(text: &str) -> Result<Source> {
    let text = text.trim().trim_end_matches('/');
    let Some((scheme, rest)) = text.split_once("://") else {
        split_repo(text)?;
        return Ok(Source {
            server: None,
            repo: text.to_string(),
        });
    };
    if !matches!(scheme, "http" | "https") {
        bail!("{text:?}: only http and https servers are supported");
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() < 3 || segments.iter().any(|s| s.is_empty()) {
        bail!("{text:?} does not name a repository; use <server>/owner/name");
    }
    let (server, repo) = segments.split_at(segments.len() - 2);
    Ok(Source {
        server: Some(format!("{scheme}://{}", server.join("/"))),
        repo: repo.join("/"),
    })
}

/// How a clone source is read before any network use.
#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    /// `owner/name` on the configured server.
    Pyn(Source),
    /// An `http(s)` address that is a pyn server's when the server answers its API.
    Http(Source),
    /// A git remote: `ssh://`, `git+ssh://`, scp-like, a local path or any other URL.
    Git,
}

/// `host:path`: a colon before the first slash, with no scheme.
pub fn is_scp_like(text: &str) -> bool {
    match text.find([':', '/']) {
        Some(i) => text.as_bytes()[i] == b':' && i > 0,
        None => false,
    }
}

pub fn kind(text: &str) -> Result<Kind> {
    let text = text.trim();
    if let Some((scheme, _)) = text.split_once("://") {
        return Ok(
            match (scheme.to_ascii_lowercase().as_str(), parse_source(text)) {
                ("http" | "https", Ok(source)) => Kind::Http(source),
                _ => Kind::Git,
            },
        );
    }
    if is_scp_like(text) || text.starts_with(['/', '.', '~']) {
        return Ok(Kind::Git);
    }
    if is_local_repository(text) {
        return Ok(Kind::Git);
    }
    parse_source(text).map(Kind::Pyn)
}

/// A relative path naming a git repository on this machine, which git clones before any name.
fn is_local_repository(text: &str) -> bool {
    let dir = Path::new(text);
    dir.join(".git").exists()
        || (dir.join("HEAD").is_file() && dir.join("objects").is_dir())
        || dir.extension().is_some_and(|e| e == "bundle") && dir.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_is_a_server_and_a_repository_or_just_a_repository() {
        let parse = |s: &str| parse_source(s).unwrap();
        assert_eq!(
            parse("http://127.0.0.1:7878/alice/game"),
            Source {
                server: Some("http://127.0.0.1:7878".into()),
                repo: "alice/game".into()
            }
        );
        assert_eq!(
            parse("https://pyn.example.com/pyn/alice/game/"),
            Source {
                server: Some("https://pyn.example.com/pyn".into()),
                repo: "alice/game".into()
            }
        );
        assert_eq!(
            parse("alice/game"),
            Source {
                server: None,
                repo: "alice/game".into()
            }
        );
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            "",
            "game",
            "a/b/c",
            "/a",
            "http://host",
            "http://host/game",
            "http://host//game",
            "ssh://host/alice/game",
            "ftp://host/alice/game",
        ] {
            assert!(parse_source(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_clone_source_is_a_pyn_name_an_http_address_or_a_git_remote() {
        let pyn = |s: &str| Kind::Pyn(parse_source(s).unwrap());
        assert_eq!(kind("alice/game").unwrap(), pyn("alice/game"));
        assert!(matches!(
            kind("https://h/alice/game").unwrap(),
            Kind::Http(_)
        ));
        assert!(matches!(
            kind("https://github.com/org/r.git").unwrap(),
            Kind::Http(_)
        ));
        for git in [
            "https://h/repo.git",
            "ssh://git@h/a/b.git",
            "git+ssh://h/a/b",
            "git@host:org/r.git",
            "host:r",
            "/srv/r",
            "./r",
            "../r",
            "~/r",
            "file:///srv/r",
        ] {
            assert_eq!(kind(git).unwrap(), Kind::Git, "{git}");
        }
        assert!(kind("game").is_err());
        assert!(kind("a/b/c").is_err());
    }
}

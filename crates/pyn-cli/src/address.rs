//! Parsing `owner/name` repository names and `<server>/owner/name` clone sources.

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
}

//! Parsing and friendly validation of `pyn token create` input; the server stays the authority.

use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use pyn_proto as api;

const REPOSITORY: &[&str] = &[
    "metadata",
    "contents",
    "locks",
    "policy",
    "members",
    "administration",
    "audit",
];
const ORGANIZATION: &[&str] = &[
    "org_members",
    "org_teams",
    "org_administration",
    "org_audit",
];
const ACCOUNT: &[&str] = &["account_profile", "account_ssh_keys", "account_locks"];

/// Parses `resource:level` and checks the pair against the levels the resource takes.
pub fn permission(text: &str) -> Result<api::TokenPermission> {
    let (resource, level) = text.split_once(':').with_context(|| {
        format!("permission {text:?} must be resource:level, like contents:write")
    })?;
    ensure!(
        REPOSITORY.contains(&resource)
            || ORGANIZATION.contains(&resource)
            || ACCOUNT.contains(&resource),
        "unknown resource {resource:?}; the resources are {}",
        [REPOSITORY, ORGANIZATION, ACCOUNT].concat().join(", ")
    );
    ensure!(
        matches!(level, "read" | "write" | "admin"),
        "unknown level {level:?} for {resource}; use read, write or admin"
    );
    match (resource, level) {
        ("locks", _) => {}
        (_, "admin") => bail!("{resource} has no admin level; only locks does"),
        ("policy" | "account_profile", "read") => bail!("{resource} takes write only"),
        ("metadata", "write") => bail!("metadata takes read only"),
        ("org_audit" | "account_locks", "write") => bail!("{resource} takes read only"),
        _ => {}
    }
    Ok(api::TokenPermission {
        resource: resource.into(),
        level: level.into(),
    })
}

pub fn permissions(texts: &[String]) -> Result<Vec<api::TokenPermission>> {
    texts.iter().map(|t| permission(t)).collect()
}

fn is_repository(resource: &str) -> bool {
    REPOSITORY.contains(&resource)
}

/// The owner of every named repository, which must be one account or organization.
pub fn owner_of(repos: &[String]) -> Result<String> {
    let mut owners = repos.iter().map(|r| {
        r.split_once('/')
            .filter(|(o, n)| !o.is_empty() && !n.is_empty())
            .map(|(o, _)| o)
            .with_context(|| format!("repository {r:?} must be owner/name"))
    });
    let first = owners.next().context("no repositories named")??;
    for owner in owners {
        ensure!(
            owner? == first,
            "a token covers the repositories of one owner; make one token per owner"
        );
    }
    Ok(first.to_string())
}

/// Repository access and the resource owner (`None`: the server defaults to the caller).
pub fn access(
    mode: Option<&str>,
    repos: Vec<String>,
    owner: Option<String>,
    perms: &[api::TokenPermission],
    current_repo: Result<&str, &str>,
) -> Result<(api::RepositoryAccess, Option<String>)> {
    let has_repository = perms.iter().any(|p| is_repository(&p.resource));
    let mode = match mode {
        Some(m) => m,
        None if !repos.is_empty() => "selected",
        None if !has_repository => "public",
        None => "selected",
    };
    ensure!(
        matches!(mode, "public" | "all" | "selected"),
        "unknown access {mode:?}; use public, all or selected"
    );
    if mode != "selected" {
        ensure!(repos.is_empty(), "--repos goes with --access selected only");
    }
    if mode == "public" {
        ensure!(
            !has_repository,
            "a public token takes no repository permissions; use --access all or selected"
        );
    } else {
        ensure!(
            has_repository,
            "{mode} repository access needs at least one repository permission, like contents:read"
        );
    }
    let mut repos = repos;
    if mode == "selected" {
        if repos.is_empty() {
            repos.push(
                current_repo
                    .map_err(|m| anyhow::anyhow!(m.to_string()))?
                    .to_string(),
            );
        }
        let from_repos = owner_of(&repos)?;
        if let Some(o) = &owner {
            ensure!(
                *o == from_repos,
                "the selected repositories belong to {from_repos}, not {o}"
            );
        }
        return Ok((
            api::RepositoryAccess {
                mode: mode.into(),
                repos,
            },
            Some(from_repos),
        ));
    }
    Ok((
        api::RepositoryAccess {
            mode: mode.into(),
            repos,
        },
        owner,
    ))
}

/// Checks that organization and account resources fit the owner, when it is known to be the caller's account or not.
pub fn check_owner_kind(owner: Option<&str>, perms: &[api::TokenPermission]) -> Result<()> {
    let org = perms
        .iter()
        .any(|p| ORGANIZATION.contains(&p.resource.as_str()));
    let account = perms.iter().any(|p| ACCOUNT.contains(&p.resource.as_str()));
    ensure!(
        !(org && account),
        "organization and account permissions need different owners; make one token for each"
    );
    if org {
        ensure!(
            owner.is_some(),
            "organization permissions need --owner <organization>"
        );
    }
    Ok(())
}

/// The `expires_at` and `never` fields from the flags; neither set leaves the server's 30 days.
pub fn expiry(
    days: Option<i64>,
    date: Option<&str>,
    never: bool,
    now: DateTime<Utc>,
) -> Result<(Option<DateTime<Utc>>, bool)> {
    if let Some(d) = days {
        ensure!(d > 0, "--expires-days must be positive");
        return Ok((Some(now + Duration::days(d)), false));
    }
    if let Some(text) = date {
        let day = NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .with_context(|| format!("--expires {text:?} must be a date like 2027-01-31"))?;
        let at = day.and_hms_opt(23, 59, 59).unwrap().and_utc();
        ensure!(at > now, "--expires {text} is in the past");
        return Ok((Some(at), false));
    }
    Ok((None, never))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(t: &str) -> Result<api::TokenPermission> {
        permission(t)
    }

    #[test]
    fn permissions_are_checked_per_resource() {
        assert!(p("contents:write").is_ok());
        assert!(p("locks:admin").is_ok());
        assert!(p("org_members:read").is_ok());
        assert!(p("account_ssh_keys:write").is_ok());
        assert!(p("account_profile:write").is_ok());
        for bad in [
            "contents",
            "files:read",
            "contents:own",
            "policy:read",
            "contents:admin",
            "account_locks:write",
            "org_audit:write",
            "account_profile:read",
        ] {
            assert!(p(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn access_defaults_follow_the_permissions() {
        let repo = [p("contents:read").unwrap()];
        let acct = [p("account_ssh_keys:read").unwrap()];
        let (a, o) = access(None, vec![], None, &repo, Ok("alice/game")).unwrap();
        assert_eq!(
            (a.mode.as_str(), a.repos, o.as_deref()),
            ("selected", vec!["alice/game".to_string()], Some("alice"))
        );
        let (a, o) = access(None, vec![], None, &acct, Err("x")).unwrap();
        assert_eq!((a.mode.as_str(), o), ("public", None));
        assert!(access(None, vec![], None, &repo, Err("x")).is_err());
        assert!(access(Some("public"), vec![], None, &repo, Err("x")).is_err());
        assert!(access(Some("all"), vec![], None, &acct, Err("x")).is_err());
        assert!(access(Some("all"), vec!["a/b".into()], None, &repo, Err("x")).is_err());
        assert!(
            access(
                None,
                vec!["a/x".into(), "b/y".into()],
                None,
                &repo,
                Err("x")
            )
            .is_err()
        );
        assert!(access(None, vec!["a/x".into()], Some("b".into()), &repo, Err("x")).is_err());
    }

    #[test]
    fn expiry_forms() {
        let now = "2026-11-18T00:00:00Z".parse().unwrap();
        assert_eq!(expiry(None, None, false, now).unwrap(), (None, false));
        assert_eq!(expiry(None, None, true, now).unwrap(), (None, true));
        let (at, never) = expiry(Some(7), None, false, now).unwrap();
        assert_eq!(
            (at.unwrap().to_rfc3339(), never),
            ("2026-11-25T00:00:00+00:00".into(), false)
        );
        assert!(
            expiry(None, Some("2027-01-31"), false, now)
                .unwrap()
                .0
                .is_some()
        );
        assert!(expiry(None, Some("2026-01-31"), false, now).is_err());
        assert!(expiry(None, Some("soon"), false, now).is_err());
        assert!(expiry(Some(0), None, false, now).is_err());
    }

    #[test]
    fn owner_kind_is_checked() {
        let org = [p("org_teams:write").unwrap()];
        let both = [
            p("org_teams:write").unwrap(),
            p("account_locks:read").unwrap(),
        ];
        assert!(check_owner_kind(None, &org).is_err());
        assert!(check_owner_kind(Some("acme"), &org).is_ok());
        assert!(check_owner_kind(Some("acme"), &both).is_err());
    }
}

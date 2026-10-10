//! Owner limits and usage. Limits are opt-in: a server enforces none until an operator sets one.

use std::str::FromStr;

use anyhow::{Result, bail};
use clap::Subcommand;
use pyn_proto as api;
use reqwest::Method;

use crate::client::Api;
use crate::table;

#[derive(Subcommand)]
pub enum LimitsCommand {
    /// List the owners that have limits of their own, and the server default when one is set.
    List,
    /// Show one owner's limits and where each comes from.
    Show { owner: String },
    /// Change an owner's own limits; fields you leave out stay as they are.
    Set {
        owner: String,
        /// Repositories the owner may have: a number, or `default` to follow the server default.
        #[arg(long)]
        repos: Option<LimitValue>,
        /// Members an organization may have; organizations only.
        #[arg(long)]
        members: Option<LimitValue>,
        /// Stored content: bytes, or a number with a K, M, G or T suffix (1024-based).
        #[arg(long)]
        storage: Option<LimitValue>,
    },
}

/// A number, or `default` to drop the owner's own value.
#[derive(Clone, Debug)]
pub struct LimitValue(Option<u64>);

impl FromStr for LimitValue {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let s = s.trim().to_ascii_lowercase();
        if s == "default" {
            return Ok(Self(None));
        }
        let digits = s.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let suffix = &s[digits.len()..];
        let unit: u64 = match suffix {
            "" | "b" => 1,
            "k" | "kb" | "kib" => 1 << 10,
            "m" | "mb" | "mib" => 1 << 20,
            "g" | "gb" | "gib" => 1 << 30,
            "t" | "tb" | "tib" => 1 << 40,
            _ => {
                return Err(format!(
                    "{s:?} is not a number, a size like 500M, or `default`"
                ));
            }
        };
        digits
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(unit))
            .map(|n| Self(Some(n)))
            .ok_or_else(|| format!("{s:?} is not a number, a size like 500M, or `default`"))
    }
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn cell(value: Option<u64>) -> String {
    value.map_or("-".into(), |v| v.to_string())
}

fn size_cell(value: Option<u64>) -> String {
    value.map_or("-".into(), bytes)
}

fn kind(k: api::OwnerKind) -> &'static str {
    match k {
        api::OwnerKind::User => "user",
        api::OwnerKind::Org => "org",
    }
}

fn source(own: Option<u64>, effective: Option<u64>) -> &'static str {
    match (own, effective) {
        (Some(_), _) => "owner",
        (None, Some(_)) => "server default",
        (None, None) => "-",
    }
}

pub fn run(api: &Api, cmd: LimitsCommand) -> Result<()> {
    match cmd {
        LimitsCommand::List => {
            let listing: api::LimitsListing = api.send(api.get("/v1/admin/limits"))?.json()?;
            let d = listing.defaults;
            if d != api::Limits::default() {
                println!(
                    "server default: repositories {}, members {}, storage {}",
                    cell(d.repositories),
                    cell(d.members),
                    size_cell(d.storage_bytes)
                );
            }
            let rows: Vec<Vec<String>> = listing
                .owners
                .into_iter()
                .map(|o| {
                    vec![
                        cell(o.limits.own.repositories),
                        cell(o.limits.own.members),
                        size_cell(o.limits.own.storage_bytes),
                        kind(o.limits.kind).into(),
                        o.owner,
                    ]
                })
                .collect();
            table::show(
                &["REPOSITORIES", "MEMBERS", "STORAGE", "KIND", "OWNER"],
                &rows,
                "no owner has limits of its own",
            );
        }
        LimitsCommand::Show { owner } => {
            let limits: api::OwnerLimits = api
                .send(api.get(&format!("/v1/owners/{owner}/limits")))?
                .json()?;
            println!("{owner} ({})", kind(limits.kind));
            print_limits(&limits);
        }
        LimitsCommand::Set {
            owner,
            repos,
            members,
            storage,
        } => {
            if repos.is_none() && members.is_none() && storage.is_none() {
                bail!("nothing to change: pass --repos, --members or --storage");
            }
            let body = api::SetLimitsRequest {
                repositories: repos.map(|v| v.0),
                members: members.map(|v| v.0),
                storage_bytes: storage.map(|v| v.0),
            };
            let limits: api::OwnerLimits = api
                .send(
                    api.request(Method::PATCH, &format!("/v1/admin/owners/{owner}/limits"))
                        .json(&body),
                )?
                .json()?;
            println!("updated limits for {owner}");
            print_limits(&limits);
        }
    }
    Ok(())
}

fn print_limits(limits: &api::OwnerLimits) {
    let (e, o) = (limits.effective, limits.own);
    let mut rows = vec![vec![
        cell(e.repositories),
        source(o.repositories, e.repositories).into(),
        "repositories".to_string(),
    ]];
    if limits.kind == api::OwnerKind::Org {
        rows.push(vec![
            cell(e.members),
            source(o.members, e.members).into(),
            "members".to_string(),
        ]);
    }
    rows.push(vec![
        size_cell(e.storage_bytes),
        source(o.storage_bytes, e.storage_bytes).into(),
        "storage".to_string(),
    ]);
    println!("{}", table::render(&["LIMIT", "SET BY", "RESOURCE"], &rows));
}

/// An owner's usage next to its limits and each repository's share; the signed-in user by default.
pub fn usage(api: &Api, owner: Option<String>) -> Result<()> {
    let owner = match owner {
        Some(o) => o,
        None => api.send(api.get("/v1/me"))?.json::<api::Account>()?.user,
    };
    let report: api::OwnerUsage = api
        .send(api.get(&format!("/v1/owners/{owner}/usage")))?
        .json()?;
    let (u, l) = (report.usage, report.limits.effective);
    println!("{owner} ({})", kind(report.limits.kind));
    let mut rows = vec![vec![
        u.repositories.to_string(),
        cell(l.repositories),
        "repositories".to_string(),
    ]];
    if let Some(members) = u.members {
        rows.push(vec![members.to_string(), cell(l.members), "members".into()]);
    }
    rows.push(vec![
        bytes(u.stored_bytes),
        size_cell(l.storage_bytes),
        "storage".into(),
    ]);
    println!("{}", table::render(&["USED", "LIMIT", "RESOURCE"], &rows));
    if !report.repositories.is_empty() {
        println!();
        println!("{}", repo_table(&report.repositories));
    }
    Ok(())
}

fn repo_table(repos: &[api::RepoUsage]) -> String {
    let rows: Vec<Vec<String>> = repos
        .iter()
        .map(|r| {
            vec![
                bytes(r.stored_bytes),
                r.files.to_string(),
                r.revisions.to_string(),
                r.repository.clone(),
            ]
        })
        .collect();
    table::render(&["STORED", "FILES", "REVISIONS", "REPOSITORY"], &rows)
}

pub fn repo_usage(api: &Api, owner: &str, name: &str) -> Result<()> {
    let used: api::RepoUsage = api
        .send(api.get(&format!("/v1/repos/{owner}/{name}/usage")))?
        .json()?;
    println!("{}", repo_table(&[used]));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_values_take_numbers_sizes_and_default() {
        let parse = |s: &str| s.parse::<LimitValue>().map(|v| v.0);
        assert_eq!(parse("7"), Ok(Some(7)));
        assert_eq!(parse("500M"), Ok(Some(500 << 20)));
        assert_eq!(parse("2gib"), Ok(Some(2 << 30)));
        assert_eq!(parse("DEFAULT"), Ok(None));
        for bad in ["", "x", "-1", "5 parsecs", "99999999999T"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn sizes_read_back_in_binary_units() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1023), "1023 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(5 << 30), "5.0 GiB");
    }
}

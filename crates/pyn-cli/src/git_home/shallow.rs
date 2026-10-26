//! The shallow flags of `clone` and `fetch`, resolved to a wire request.

use anyhow::{Result, bail};
use chrono::Local;
use clap::Args;
use pyn_git::INFINITE_DEPTH;
use pyn_gitwire::Deepen;

use super::log_match::parse_date;

#[derive(Debug, Args, Default, PartialEq, Eq)]
pub struct ShallowFlags {
    /// Keep only this many commits from each branch tip.
    #[arg(long, value_name = "DEPTH", allow_hyphen_values = true)]
    pub(super) depth: Option<String>,
    /// Keep only commits newer than this date.
    #[arg(long, value_name = "DATE")]
    pub(super) shallow_since: Option<String>,
    /// Keep only commits not reachable from this revision (repeatable).
    #[arg(long, value_name = "REV")]
    pub(super) shallow_exclude: Vec<String>,
}

/// The extra flags of `fetch`, which moves an existing boundary.
#[derive(Debug, Args, Default)]
pub struct FetchShallow {
    #[command(flatten)]
    pub(super) flags: ShallowFlags,
    /// Deepen history by this many commits from the current boundary.
    #[arg(long, value_name = "N", allow_hyphen_values = true)]
    deepen: Option<String>,
    /// Fetch the full history and drop the shallow boundary.
    #[arg(long)]
    unshallow: bool,
}

fn count(flag: &str, value: &str) -> Result<u32> {
    match value.parse::<u32>() {
        Ok(n) if n > 0 => Ok(n),
        _ => bail!(
            "{} {value} is not a positive number",
            flag.trim_start_matches('-')
        ),
    }
}

impl ShallowFlags {
    pub(super) fn given(&self) -> bool {
        self.depth.is_some() || self.shallow_since.is_some() || !self.shallow_exclude.is_empty()
    }

    pub(super) fn request(&self) -> Result<Option<Deepen>> {
        if !self.given() {
            return Ok(None);
        }
        let depth = self
            .depth
            .as_deref()
            .map(|d| count("--depth", d))
            .transpose()?;
        let since = self
            .shallow_since
            .as_deref()
            .map(|d| parse_date(d, Local::now()))
            .transpose()?;
        Ok(Some(Deepen {
            depth,
            relative: false,
            since,
            not: self.shallow_exclude.clone(),
        }))
    }
}

impl FetchShallow {
    /// `None` for a plain fetch. `shallow` is whether the repository is shallow now.
    pub(super) fn request(&self, shallow: bool) -> Result<Option<Deepen>> {
        let f = &self.flags;
        let clash = |a: &str, b: &str| -> Result<()> {
            bail!("options '{a}' and '{b}' cannot be used together")
        };
        if self.unshallow && f.depth.is_some() {
            clash("--depth", "--unshallow")?;
        }
        if self.deepen.is_some() && f.depth.is_some() {
            clash("--deepen", "--depth")?;
        }
        if self.deepen.is_some() && f.shallow_since.is_some() {
            clash("--deepen", "--shallow-since")?;
        }
        if self.deepen.is_some() && !f.shallow_exclude.is_empty() {
            clash("--deepen", "--shallow-exclude")?;
        }
        if self.unshallow {
            if !shallow {
                bail!("--unshallow on a complete repository does not make sense");
            }
            return Ok(Some(Deepen {
                depth: Some(INFINITE_DEPTH),
                ..Deepen::default()
            }));
        }
        if let Some(n) = &self.deepen {
            let n = count("--deepen", n)?;
            return Ok(shallow.then(|| Deepen {
                depth: Some(n),
                relative: true,
                ..Deepen::default()
            }));
        }
        f.request()
    }
}

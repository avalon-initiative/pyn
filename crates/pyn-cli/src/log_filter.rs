//! `log` filters on a pyn server: options and revision arguments mapped to the `/commits` query.

use std::path::Path;

use anyhow::{Result, bail};

use crate::backend::Backend;
use crate::git_home::GitLogArgs;
use crate::snapshot;

/// The server-side narrowing of a commit listing; every field is a `/commits` parameter.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommitFilter {
    pub author: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub grep: Option<String>,
    pub path: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

impl CommitFilter {
    pub fn query(&self) -> Vec<(&'static str, &str)> {
        [
            ("author", &self.author),
            ("since", &self.since),
            ("until", &self.until),
            ("grep", &self.grep),
            ("path", &self.path),
            ("from", &self.from),
            ("to", &self.to),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.as_deref().map(|v| (k, v)))
        .collect()
    }
}

/// Whether `arg` reads as a revision (a range, `^rev` or a commit reference that is not a file here).
fn is_rev(arg: &str, cwd: &Path) -> bool {
    arg.starts_with('^')
        || arg.contains("..")
        || (snapshot::parse_commit(arg).is_some() && !cwd.join(arg).exists())
}

/// Whether the arguments ask for a filtered commit listing rather than the revision table; a path does when
/// `listing` (a format or diff output) already means commits.
pub fn selects(path: Option<&str>, args: &GitLogArgs, cwd: &Path, listing: bool) -> bool {
    args.has_server_filter()
        || (listing && (path.is_some() || !args.dashed().is_empty()))
        || !args.positionals().is_empty()
        || path.is_some_and(|p| !args.dashed().is_empty() || is_rev(p, cwd))
}

/// More than one path, which the server's single `path` filter cannot take.
pub fn too_many_paths(path: Option<&str>, args: &GitLogArgs, cwd: &Path) -> bool {
    if !args.dashed().is_empty() {
        return false;
    }
    let items = path
        .into_iter()
        .chain(args.positionals().iter().map(String::as_str));
    items.skip_while(|a| is_rev(a, cwd)).count() > 1
}

/// A revision argument resolved to a full commit id; the empty side of `..` is `HEAD`.
fn resolve(backend: &dyn Backend, text: &str) -> Result<String> {
    snapshot::resolve(backend, if text.is_empty() { "HEAD" } else { text })
}

/// Builds the `/commits` filter from the options, the revision arguments and the path.
pub fn build(
    backend: &dyn Backend,
    args: &GitLogArgs,
    path: Option<&str>,
    cwd: &Path,
    repo_path: &dyn Fn(&str) -> Result<String>,
) -> Result<CommitFilter> {
    let mut filter = args.commit_filter()?;
    let mut items: Vec<&str> = path
        .into_iter()
        .chain(args.positionals().iter().map(String::as_str))
        .collect();
    let mut paths: Vec<&str> = args.dashed().iter().map(String::as_str).collect();
    if paths.is_empty() {
        let revs = items.iter().take_while(|a| is_rev(a, cwd)).count();
        paths = items.split_off(revs);
    }
    let (mut from, mut to) = (None, None);
    let set = |slot: &mut Option<String>, id: String, what: &str| -> Result<()> {
        if slot.replace(id).is_some() {
            bail!("`pyn log` takes one {what} revision on a pyn server");
        }
        Ok(())
    };
    for item in items {
        if item.contains("...") {
            bail!("`pyn log {item}` (symmetric difference) is not available on a pyn server");
        } else if let Some((a, b)) = item.split_once("..") {
            set(&mut to, resolve(backend, a)?, "excluded")?;
            set(&mut from, resolve(backend, b)?, "starting")?;
        } else if let Some(rest) = item.strip_prefix('^') {
            set(&mut to, resolve(backend, rest)?, "excluded")?;
        } else {
            set(&mut from, resolve(backend, item)?, "starting")?;
        }
    }
    filter.from = from;
    filter.to = to;
    filter.path = paths.first().map(|p| repo_path(p)).transpose()?;
    Ok(filter)
}

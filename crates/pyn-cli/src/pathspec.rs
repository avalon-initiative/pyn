//! Command-line pathspecs for both homes: git's magic and globs, resolved against the working folder.

use std::path::Path;

use anyhow::{Result, bail};
use pyn_gitwork::{MagicSet, Pathspec, PathspecContext, PathspecError, PathspecOptions};

use crate::workspace::Workspace;

/// Parses `args` as pathspecs for a command run in `cwd`. With `prefer_cwd`, none (or only excludes) means the
/// working folder rather than the whole repository. A magic that is valid in git but not built here goes to `usage`.
pub fn parse(
    ws: &Workspace,
    cwd: &Path,
    args: &[String],
    (options, unsupported): (PathspecOptions, MagicSet),
    prefer_cwd: bool,
    usage: fn(&str) -> !,
) -> Result<Pathspec> {
    let prefix = ws.repo_prefix(cwd, ".")?;
    let cx = PathspecContext {
        root: ws.root.as_os_str().as_encoded_bytes(),
        prefix: prefix.as_bytes(),
        options,
        unsupported,
        prefer_cwd,
    };
    match Pathspec::parse(args, &cx) {
        Ok(spec) => Ok(spec),
        Err(PathspecError::Fatal(message)) => bail!(message),
        Err(PathspecError::Unsupported(message)) => usage(&message),
    }
}

/// Reads the `GIT_*_PATHSPECS` variables, which git parses as booleans.
pub fn options_from_env() -> Result<PathspecOptions> {
    let flag = |name: &str| -> Result<bool> {
        let Ok(value) = std::env::var(name) else {
            return Ok(false);
        };
        match value.to_ascii_lowercase().as_str() {
            "" | "false" | "no" | "off" => Ok(false),
            "true" | "yes" | "on" => Ok(true),
            other => match other.parse::<i64>() {
                Ok(n) => Ok(n != 0),
                Err(_) => bail!("bad boolean environment value '{value}' for '{name}'"),
            },
        }
    };
    Ok(PathspecOptions {
        literal: flag("GIT_LITERAL_PATHSPECS")?,
        glob: flag("GIT_GLOB_PATHSPECS")?,
        noglob: flag("GIT_NOGLOB_PATHSPECS")?,
        icase: flag("GIT_ICASE_PATHSPECS")?,
    })
}

/// Parses `args` as repository paths from the root, for a command run outside a workspace.
pub fn parse_from_root(args: &[String], usage: fn(&str) -> !) -> Result<Pathspec> {
    let cx = PathspecContext {
        root: b"/",
        prefix: b"",
        options: PathspecOptions::default(),
        unsupported: MagicSet::default(),
        prefer_cwd: false,
    };
    match Pathspec::parse(args, &cx) {
        Ok(spec) => Ok(spec),
        Err(PathspecError::Fatal(message)) => bail!(message),
        Err(PathspecError::Unsupported(message)) => usage(&message),
    }
}

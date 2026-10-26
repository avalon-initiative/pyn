//! Command-line pathspecs for both homes: git's magic and globs, resolved against the working folder.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, bail};
use pyn_gitwork::{
    Attributes, MagicSet, Pathspec, PathspecContext, PathspecError, PathspecOptions,
};

use crate::workspace::Workspace;

/// What a command lets pathspecs do: the `GIT_*_PATHSPECS` settings, the magic it refuses, and the attributes
/// that `:(attr:...)` reads (none off a git home, which makes that magic a usage error).
pub type Settings = (PathspecOptions, MagicSet, Option<Arc<Attributes>>);

/// Parses `args` as pathspecs for a command run in `cwd`. With `prefer_cwd`, none (or only excludes) means the
/// working folder rather than the whole repository. A magic that is valid in git but not built here goes to `usage`.
pub fn parse(
    ws: &Workspace,
    cwd: &Path,
    args: &[String],
    (options, unsupported, attributes): Settings,
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
        attributes: attributes.as_ref(),
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
        attributes: None,
    };
    match Pathspec::parse(args, &cx) {
        Ok(spec) => Ok(spec),
        Err(PathspecError::Fatal(message)) => bail!(message),
        Err(PathspecError::Unsupported(message)) => usage(&message),
    }
}

/// `--pathspec-from-file` and `--pathspec-file-nul`, for the commands git gives them.
#[derive(Debug, Default, clap::Args)]
pub struct PathspecFile {
    /// Read the pathspecs from this file (`-` for standard input) instead of the command line, one per line.
    #[arg(long, value_name = "FILE")]
    pub pathspec_from_file: Option<String>,
    /// Separate the pathspecs in that file with NUL and read them as written, without quoting.
    #[arg(long)]
    pub pathspec_file_nul: bool,
}

impl PathspecFile {
    /// Appends the file's pathspecs to `paths`, which must be empty; `conflict` names the option (if any) that
    /// git refuses to combine with the file.
    pub fn expand(&self, paths: &mut Vec<String>, conflict: Option<&str>) -> Result<()> {
        let Some(file) = &self.pathspec_from_file else {
            if self.pathspec_file_nul {
                bail!("the option '--pathspec-file-nul' requires '--pathspec-from-file'");
            }
            return Ok(());
        };
        if let Some(option) = conflict {
            bail!("options '--pathspec-from-file' and '{option}' cannot be used together");
        }
        if !paths.is_empty() {
            bail!("'--pathspec-from-file' and pathspec arguments cannot be used together");
        }
        let data = if file == "-" {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)
                .map_err(|e| anyhow::anyhow!("could not read '-': {e}"))?;
            buf
        } else {
            std::fs::read(file).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::anyhow!(
                        "could not open '{file}' for reading: No such file or directory"
                    )
                } else {
                    anyhow::anyhow!("could not read '{file}': {e}")
                }
            })?
        };
        for line in pyn_gitwork::read_pathspec_file(&data, self.pathspec_file_nul)
            .map_err(|e| anyhow::anyhow!("{e}"))?
        {
            paths.push(
                String::from_utf8(line)
                    .map_err(|_| anyhow::anyhow!("a pathspec in '{file}' is not valid UTF-8"))?,
            );
        }
        Ok(())
    }
}

//! `pyn init-git`: starts a repository used like git, a real `.git` written without git plus pyn's own folder (#562).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::Args;
use pyn_gittransport::{Env, user_config};
use pyn_object::Algorithm;

use super::{GitHome, commands, git_root_of};
use crate::workspace::Workspace;

/// Options of `pyn init-git`; the git options pyn does not build are refused as usage errors.
#[derive(Debug, Args, Default, PartialEq, Eq)]
pub struct InitGitArgs {
    /// The folder to start the repository in; created if missing. The current folder if omitted.
    directory: Option<PathBuf>,
    /// Name the first branch this instead of `init.defaultBranch` (else `main`).
    #[arg(short = 'b', long, value_name = "NAME")]
    initial_branch: Option<String>,
    /// Hash for object ids: sha1 or sha256.
    #[arg(long, value_name = "FORMAT", default_value = "sha1", value_parser = ["sha1", "sha256"])]
    object_format: String,
    /// Print nothing but errors.
    #[arg(short, long)]
    quiet: bool,
    #[arg(long, hide = true)]
    bare: bool,
    #[arg(long, hide = true, value_name = "DIR")]
    template: Option<String>,
    #[arg(long, hide = true, value_name = "DIR")]
    separate_git_dir: Option<String>,
    #[arg(long, hide = true, value_name = "PERMISSIONS", num_args = 0..=1, require_equals = true)]
    shared: Option<Option<String>>,
}

impl InitGitArgs {
    fn refuse_unsupported(&self) {
        let given = [
            ("--bare", self.bare, "a repository always has a work tree"),
            (
                "--template",
                self.template.is_some(),
                "repositories are created without templates",
            ),
            (
                "--separate-git-dir",
                self.separate_git_dir.is_some(),
                "the git directory is always `.git` in the work tree",
            ),
            (
                "--shared",
                self.shared.is_some(),
                "shared permissions are not supported",
            ),
        ];
        if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
            crate::usage_error(&format!("`pyn init-git {flag}`: {why}"));
        }
    }
}

/// Whether a git repository or a pyn workspace already covers `target`.
pub(crate) fn is_tracked(target: &Path) -> Result<bool> {
    Ok(git_root_of(target)?.is_some()
        || target.ancestors().any(|p| p.join(".git").exists())
        || Workspace::discover(target).is_some())
}

fn refuse_tracked(target: &Path) -> Result<()> {
    if is_tracked(target)? {
        bail!(
            "{} is already in a git repository or a pyn workspace; `pyn convert` adopts an existing repository",
            target.display()
        );
    }
    Ok(())
}

pub fn init_git(args: &InitGitArgs, cwd: &Path) -> Result<()> {
    args.refuse_unsupported();
    let target = match &args.directory {
        Some(d) => cwd.join(d),
        None => cwd.to_path_buf(),
    };
    refuse_tracked(&target)?;
    let branch = match &args.initial_branch {
        Some(b) => b.clone(),
        None => user_config(&Env::process())?
            .get("init.defaultbranch")
            .filter(|b| !b.is_empty())
            .unwrap_or("main")
            .to_owned(),
    };
    GitHome::check_branch_name(&branch)?;
    let algorithm = match args.object_format.as_str() {
        "sha256" => Algorithm::Sha256,
        _ => Algorithm::Sha1,
    };
    std::fs::create_dir_all(&target)
        .with_context(|| format!("cannot mkdir {}", target.display()))?;
    let root = target.canonicalize()?;
    pyn_git::init(&root.join(".git"), false, algorithm, &branch)?;
    commands::hide_pyn(&GitHome::open_at(&root)?)?;
    if !args.quiet {
        println!(
            "Initialized empty Git repository in {}/.git/",
            root.display()
        );
    }
    Ok(())
}

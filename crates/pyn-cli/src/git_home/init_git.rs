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
    /// Make the folder the git directory itself, with no work tree.
    #[arg(long)]
    bare: bool,
    /// Keep the git directory here and leave a `.git` file pointing at it in the work tree.
    #[arg(long, value_name = "DIR")]
    separate_git_dir: Option<PathBuf>,
    #[arg(long, hide = true, value_name = "DIR")]
    template: Option<String>,
    #[arg(long, hide = true, value_name = "PERMISSIONS", num_args = 0..=1, require_equals = true)]
    shared: Option<Option<String>>,
}

impl InitGitArgs {
    fn refuse_unsupported(&self) {
        let given = [
            (
                "--template",
                self.template.is_some(),
                "repositories are created without templates",
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
    if args.bare && args.separate_git_dir.is_some() {
        bail!("options '--separate-git-dir' and '--bare' cannot be used together");
    }
    std::fs::create_dir_all(&target)
        .with_context(|| format!("cannot mkdir {}", target.display()))?;
    let root = target.canonicalize()?;
    let git_dir = if args.bare {
        root.clone()
    } else if let Some(dir) = &args.separate_git_dir {
        separate_dir(cwd, dir)?
    } else {
        root.join(".git")
    };
    pyn_git::init(&git_dir, args.bare, algorithm, &branch)?;
    if !args.bare {
        if args.separate_git_dir.is_some() {
            let link = root.join(".git");
            std::fs::write(&link, format!("gitdir: {}\n", git_dir.display()))
                .with_context(|| format!("cannot write {}", link.display()))?;
        }
        commands::hide_pyn(&GitHome::open_at(&root)?)?;
    }
    if !args.quiet {
        println!("Initialized empty Git repository in {}/", git_dir.display());
    }
    Ok(())
}

/// The absolute separate git directory: its parent must exist and it must be absent or empty.
pub(super) fn separate_dir(cwd: &Path, dir: &Path) -> Result<PathBuf> {
    let joined = cwd.join(dir);
    let name = joined.file_name().map(PathBuf::from).unwrap_or_default();
    let parent = joined.parent().unwrap_or(Path::new("/"));
    let Ok(parent) = parent.canonicalize() else {
        bail!(
            "Invalid path '{}': No such file or directory",
            parent.display()
        );
    };
    let full = parent.join(name);
    if full
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_some())
    {
        bail!("{} already exists and is not empty", full.display());
    }
    Ok(full)
}

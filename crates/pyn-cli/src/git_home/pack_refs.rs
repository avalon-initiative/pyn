//! `pyn pack-refs` on a git repository: move loose refs into `packed-refs`.

use anyhow::Result;
use clap::Args;

use super::GitHome;

/// Pack refs into the single `packed-refs` file.
#[derive(Debug, Args)]
pub struct PackRefsArgs {
    /// Pack every ref, not only tags and refs that are already packed.
    #[arg(long)]
    all: bool,
    /// Pack the refs but keep the loose files.
    #[arg(long)]
    no_prune: bool,
    /// Remove loose files after packing (the default).
    #[arg(long, overrides_with = "no_prune")]
    prune: bool,
    /// Pack the refs matching this pattern too (repeatable); replaces the default of tags.
    #[arg(long)]
    include: Vec<String>,
    /// Leave the refs matching this pattern loose (repeatable).
    #[arg(long)]
    exclude: Vec<String>,
}

pub(super) fn pack_refs(git: &GitHome, args: PackRefsArgs) -> Result<()> {
    let repo = git.repo.git();
    let mut include: Vec<&str> = args.include.iter().map(String::as_str).collect();
    if args.all {
        include.push("refs/*");
    } else if include.is_empty() {
        include.push("refs/tags/*");
    }
    let exclude: Vec<&str> = args.exclude.iter().map(String::as_str).collect();
    repo.refs()
        .pack_refs_matching(&repo.objects()?, &include, &exclude, !args.no_prune)?;
    Ok(())
}

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
}

pub(super) fn pack_refs(git: &GitHome, args: PackRefsArgs) -> Result<()> {
    let repo = git.repo.git();
    repo.refs()
        .pack_refs(&repo.objects()?, args.all, !args.no_prune)?;
    Ok(())
}

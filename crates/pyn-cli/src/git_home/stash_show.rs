//! `pyn stash show`: what a stash changed, as the diff of the commit it was made on against the work tree it saved.

use anyhow::Result;
use clap::Args;
use pyn_gitwork::Pathspec;

use super::GitHome;
use super::commands::{add_changes, add_unchanged, emit, wants_harder};
use super::stash::{commit_tree, locate};
use crate::diff::{self, Tree};

#[derive(Debug, Args)]
pub(super) struct ShowArgs {
    /// Include the untracked files the stash saved.
    #[arg(short = 'u', long)]
    include_untracked: bool,
    /// Show only the untracked files the stash saved.
    #[arg(long)]
    only_untracked: bool,
    #[command(flatten)]
    format: Box<crate::DiffFormat>,
    /// The stash to show (the newest if none is named).
    stash: Vec<String>,
}

pub(super) fn show(git: &GitHome, args: ShowArgs) -> Result<()> {
    git.repo.validate_diff_ignore_submodules()?;
    if args.stash.len() > 1 {
        let named: Vec<String> = args.stash.iter().map(|s| format!("'{s}'")).collect();
        eprintln!("Too many revisions specified: {}", named.join(" "));
        std::process::exit(1);
    }
    let found = locate(git, args.stash.first().map(String::as_str))?;
    let stash = git.repo.commit(&found.id)?;
    let only_untracked = args.only_untracked;
    let untracked = only_untracked
        || args.include_untracked
        || git.repo.bool_setting("stash.showIncludeUntracked", false);
    let all = Pathspec::default();
    let (mut old, mut new) = (Tree::new(), Tree::new());
    if !only_untracked {
        let base = commit_tree(git, &stash.parents[0])?;
        let changes = git.repo.diff_trees(Some(&base), Some(&stash.tree), &all)?;
        add_changes(git, &mut old, &mut new, &changes)?;
        if wants_harder(git, &args.format.build()) {
            add_unchanged(
                git,
                (&mut old, &mut new),
                (Some(&base), Some(&stash.tree)),
                &all,
            )?;
        }
    }
    if untracked && let Some(u) = stash.parents.get(2) {
        let tree = commit_tree(git, u)?;
        add_changes(
            git,
            &mut old,
            &mut new,
            &git.repo.diff_trees(None, Some(&tree), &all)?,
        )?;
    }
    let f = &args.format;
    let flagged = f.stat_given()
        || f.shortstat
        || f.numstat
        || f.name_only
        || f.name_status
        || f.check
        || f.summary
        || f.extra.raw()
        || f.extra.dirstat_given();
    let (summary, patch) = if flagged || f.extra.patch() {
        (flagged, f.extra.patch())
    } else if diff_option_given() {
        (false, true)
    } else {
        (
            git.repo.bool_setting("stash.showStat", true),
            git.repo.bool_setting("stash.showPatch", false),
        )
    };
    let mut out = String::new();
    if summary {
        let kind = if flagged { f.build() } else { stat_default(f) };
        diff::compare(git, &old, &new, &[], &kind, &mut out)?;
    }
    if patch {
        let mut body = String::new();
        let format = diff::Format {
            kind: diff::Kind::Patch,
            summary: false,
            ..f.build()
        };
        diff::compare(git, &old, &new, &[], &format, &mut body)?;
        if !out.is_empty() && !body.is_empty() {
            out.push('\n');
        }
        out.push_str(&body);
    }
    emit(&out);
    Ok(())
}

/// Whether the command line has a diff option, which makes git show the patch rather than the configured default.
fn diff_option_given() -> bool {
    std::env::args()
        .skip_while(|a| a != "show")
        .skip(1)
        .take_while(|a| a != "--")
        .any(|a| {
            a.starts_with('-')
                && !matches!(
                    a.as_str(),
                    "-u" | "--include-untracked" | "--only-untracked"
                )
        })
}

fn stat_default(f: &crate::DiffFormat) -> diff::Format {
    diff::Format {
        kind: diff::Kind::Stat,
        ..f.build()
    }
}

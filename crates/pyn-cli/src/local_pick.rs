//! `pyn cherry-pick`: the arguments both homes share, the commits they name, and the run in a `local_store` workspace,
//! where each picked commit becomes a new local commit (the git home is `git_home/pick.rs`).

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use clap::{ArgGroup, Args};
use pyn_gitwork::{Pathspec, Repo};
use pyn_object::ObjectId;

use crate::backend::Backend;
use crate::git_home::append_trailer;
use crate::local_commit::signature;
use crate::local_merge;
use crate::local_store::LocalStore;
use crate::staging::Index;
use crate::workspace::Workspace;
use crate::{branch, merging};

/// Apply the changes of existing commits as new commits on the current branch.
#[derive(Args, Debug, Default)]
#[command(group(ArgGroup::new("control").args(["continue_pick", "abort", "skip", "quit"])))]
pub struct PickArgs {
    /// Commits, or ranges `<a>..<b>` (the commits after `a` up to `b`), applied oldest first. A workspace names
    /// them by hash, `HEAD~N` or branch.
    #[arg(required_unless_present = "control", conflicts_with = "control")]
    pub(crate) commits: Vec<String>,
    /// Git repository: apply the changes to the index and work tree without committing.
    #[arg(short = 'n', long, conflicts_with = "control")]
    pub(crate) no_commit: bool,
    /// Add a line `(cherry picked from commit <id>)` to the message.
    #[arg(short = 'x', conflicts_with = "control")]
    pub(crate) record_origin: bool,
    /// Git repository: add a `Signed-off-by:` line for the committer.
    #[arg(short = 's', long, conflicts_with = "control")]
    pub(crate) signoff: bool,
    /// Git repository: the parent (1, 2, ...) a merge commit is picked relative to.
    #[arg(short = 'm', long, value_name = "PARENT", conflicts_with = "control", value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) mainline: Option<u32>,
    /// Keep the message (accepted for scripts; pyn never opens an editor).
    #[arg(long, conflicts_with = "control")]
    pub(crate) no_edit: bool,
    /// Commit what was resolved and go on with the remaining commits.
    #[arg(long = "continue")]
    pub(crate) continue_pick: bool,
    /// Give up and put everything back as it was before the first commit.
    #[arg(long)]
    pub(crate) abort: bool,
    /// Drop the commit that stopped on conflicts and go on with the rest.
    #[arg(long)]
    pub(crate) skip: bool,
    /// Git repository: forget the operation in progress, keeping the files as they are.
    #[arg(long)]
    pub(crate) quit: bool,
}

/// The commits the arguments name, in the order to apply them: ranges oldest first, anything else as given.
pub(crate) fn select(
    repo: &Repo,
    resolve: &dyn Fn(&str) -> Result<ObjectId>,
    head: &str,
    specs: &[String],
) -> Result<Vec<ObjectId>> {
    let mut out = Vec::new();
    for spec in specs {
        if spec.starts_with('^') {
            bail!("'{spec}': cherry-pick takes commits and `<a>..<b>` or `<a>...<b>` ranges only");
        }
        let name = |s: &str| {
            if s.is_empty() {
                head.to_string()
            } else {
                s.to_string()
            }
        };
        if let Some((a, b)) = spec.split_once("...") {
            let (a, b) = (resolve(&name(a))?, resolve(&name(b))?);
            let common: HashSet<ObjectId> = ancestors(repo, &a)?
                .intersection(&ancestors(repo, &b)?)
                .copied()
                .collect();
            out.extend(walk(repo, &[a, b], common)?);
        } else if let Some((low, high)) = spec.split_once("..") {
            let (low, high) = (resolve(&name(low))?, resolve(&name(high))?);
            out.extend(walk(repo, &[high], ancestors(repo, &low)?)?);
        } else {
            out.push(resolve(spec)?);
        }
    }
    Ok(out)
}

fn ancestors(repo: &Repo, from: &ObjectId) -> Result<HashSet<ObjectId>> {
    let mut seen = HashSet::new();
    let mut queue = vec![*from];
    while let Some(id) = queue.pop() {
        if seen.insert(id) {
            queue.extend(repo.commit(&id)?.parents);
        }
    }
    Ok(seen)
}

/// The commits reachable from `start` and not from `hidden`, parents before children.
fn walk(repo: &Repo, start: &[ObjectId], hidden: HashSet<ObjectId>) -> Result<Vec<ObjectId>> {
    let mut log = repo.log_hiding(start, hidden, Pathspec::default())?;
    let mut parents: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut newest_first = Vec::new();
    while let Some(entry) = log.next_entry()? {
        parents.insert(entry.id, entry.commit.parents);
        newest_first.push(entry.id);
    }
    let (mut done, mut order) = (HashSet::new(), Vec::new());
    for start in newest_first.iter().rev() {
        let mut stack = vec![(*start, false)];
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                order.push(id);
                continue;
            }
            if !done.insert(id) {
                continue;
            }
            stack.push((id, true));
            for p in parents[&id].iter().rev() {
                if parents.contains_key(p) && !done.contains(p) {
                    stack.push((*p, false));
                }
            }
        }
    }
    Ok(order)
}

/// The message with the `(cherry picked from commit ...)` line git's `-x` adds.
pub(crate) fn with_origin(message: Vec<u8>, id: &ObjectId) -> Vec<u8> {
    append_trailer(message, &format!("(cherry picked from commit {id})"))
}

fn refuse_git_only(args: &PickArgs) -> Result<()> {
    let given = [
        ("-n", args.no_commit),
        ("-s", args.signoff),
        ("-m", args.mainline.is_some()),
        ("--quit", args.quit),
    ];
    if let Some((flag, _)) = given.into_iter().find(|(_, on)| *on) {
        bail!(
            "`pyn cherry-pick {flag}` needs a git repository; a workspace picks whole commits and gives up with `--abort`"
        );
    }
    Ok(())
}

pub fn pick(backend: &dyn Backend, ws: &Workspace, args: PickArgs) -> Result<()> {
    refuse_git_only(&args)?;
    let Some(store) = LocalStore::open(ws) else {
        bail!(
            "`pyn cherry-pick` works on the local commits of a workspace that keeps them; `pyn migrate` converts an older one"
        );
    };
    if args.abort || args.continue_pick || args.skip {
        local_merge::require_pick(ws)?;
        return if args.abort {
            local_merge::abort(backend, ws)
        } else if args.skip {
            local_merge::skip(backend, ws)
        } else {
            local_merge::finish(backend, ws, None)
        };
    }
    merging::refuse_during_merge(ws, "cherry-pick")?;
    if !Index::load(ws)?.entries.is_empty() {
        bail!(
            "you have staged changes; commit them or take them out with `pyn restore --staged` before you cherry-pick"
        );
    }
    let branch = branch::name(Some(ws));
    let Some(tip) = store.tip(&branch)? else {
        bail!("branch '{branch}' has no local commits");
    };
    let Some(onto) = store.published(&branch)? else {
        bail!("branch '{branch}' has not been fetched yet; run `pyn fetch` first");
    };
    let own = format!("refs/heads/{branch}");
    let resolve = |s: &str| -> Result<ObjectId> {
        let full = match s.strip_prefix("HEAD") {
            Some(rest) => format!("{own}{rest}"),
            None => s.to_string(),
        };
        store
            .repo()
            .resolve(&full)
            .map_err(|_| anyhow::anyhow!("bad revision '{s}'"))
    };
    let picks = select(store.repo(), &resolve, "HEAD", &args.commits)?;
    if picks.is_empty() {
        bail!("empty commit set passed");
    }
    for id in &picks {
        let commit = store.repo().commit(id)?;
        if commit.parents.len() > 1 {
            bail!(
                "commit {id} is a merge, which a workspace cannot cherry-pick; pick the commits it merged instead"
            );
        }
        let files = store
            .repo()
            .flatten(Some(&commit.tree), &Pathspec::default())?;
        pyn_gitwork::check_paths(files.keys().map(Vec::as_slice))?;
    }
    let who = signature(&backend.whoami()?);
    local_merge::begin_pick(
        backend,
        ws,
        &store,
        &branch,
        local_merge::Pick {
            commits: picks,
            tip,
            onto,
            origin: args.record_origin,
        },
        &who,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: PickArgs,
    }

    fn parse(args: &[&str]) -> Result<PickArgs, clap::Error> {
        Cli::try_parse_from(std::iter::once("pyn").chain(args.iter().copied())).map(|c| c.args)
    }

    #[test]
    fn commits_or_one_control_flag() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["abc", "--continue"]).is_err());
        assert!(parse(&["--abort", "--skip"]).is_err());
        assert!(parse(&["--abort", "-x"]).is_err());
        assert!(parse(&["--quit"]).unwrap().quit);
        let args = parse(&["-x", "-n", "-m", "2", "a..b", "c"]).unwrap();
        assert_eq!(args.commits, ["a..b", "c"]);
        assert_eq!(args.mainline, Some(2));
    }
}

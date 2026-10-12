//! `pyn stash` on a git repository: a stash is a commit on `refs/stash` (the work tree, with the index commit and
//! an optional untracked-files commit as further parents), and `logs/refs/stash` is the list, as git keeps them.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use pyn_git::{Expect, Index, IndexEntry, ReflogEntry};
use pyn_gitwork::{Entry, Head, Kind, Pathspec, fs_path};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId};

use super::stage::{
    Entries, entries_of, identity, index_of, put_entry, raw_mode, refuse_in_progress, stage_file,
    untracked_files, write_tree,
};
use super::switch::subject;
use super::treemerge::{Labels, Resolved, Worktree, merge_trees};
use super::worktree::{
    Blocked, Mode, in_the_way, move_tree, remove_entry_file, write_bytes, write_entry,
};
use super::{GitHome, Refused, convert, path_string};

const STASH: &str = "refs/stash";
const ABBREV: usize = 7;

/// Save, list and restore local changes.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct StashArgs {
    #[command(subcommand)]
    action: Option<StashAction>,
    #[command(flatten)]
    push: PushArgs,
}

#[derive(Debug, Args, Default)]
struct PushArgs {
    /// Stash untracked files too (they are removed from the work tree).
    #[arg(short = 'u', long)]
    include_untracked: bool,
    /// The stash's message.
    #[arg(short = 'm', long)]
    message: Option<String>,
    /// Paths to stash; not supported yet.
    #[arg(last = true)]
    paths: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum StashAction {
    /// Save the local changes (the default).
    Push(PushArgs),
    /// List the stashes, newest first.
    List,
    /// Apply a stash and drop it (the newest if none is named).
    Pop { stash: Option<String> },
    /// Apply a stash and keep it.
    Apply { stash: Option<String> },
    /// Remove a stash.
    Drop { stash: Option<String> },
    /// Remove every stash.
    Clear,
}

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

pub fn stash(git: &GitHome, args: StashArgs) -> Result<()> {
    match args.action {
        None => push(git, args.push),
        Some(StashAction::Push(p)) => push(git, p),
        Some(StashAction::List) => list(git),
        Some(StashAction::Pop { stash }) => apply(git, stash.as_deref(), true),
        Some(StashAction::Apply { stash }) => apply(git, stash.as_deref(), false),
        Some(StashAction::Drop { stash }) => drop_entry(git, stash.as_deref()),
        Some(StashAction::Clear) => clear(git),
    }
}

/// The stashes, newest first, with the commit each names.
fn entries(git: &GitHome) -> Result<Vec<ReflogEntry>> {
    let mut log = git.repo.refs().read_reflog(STASH)?;
    log.reverse();
    Ok(log)
}

fn list(git: &GitHome) -> Result<()> {
    for (n, e) in entries(git)?.iter().enumerate() {
        println!("stash@{{{n}}}: {}", e.message);
    }
    Ok(())
}

/// The position and commit a stash argument names: `stash@{N}`, `N`, or the newest.
fn find(git: &GitHome, name: Option<&str>) -> Result<(usize, ObjectId)> {
    let all = entries(git)?;
    let n = match name {
        None => 0,
        Some(s) => s
            .strip_prefix("stash@{")
            .and_then(|r| r.strip_suffix('}'))
            .unwrap_or(s)
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("'{s}' is not a stash reference"))?,
    };
    match all.get(n) {
        Some(e) => Ok((n, e.new)),
        None if all.is_empty() => return Err(Refused("No stash entries found.".into()).into()),
        None => bail!("log for 'stash' only has {} entries.", all.len()),
    }
}

fn branch_label(git: &GitHome) -> Result<String> {
    Ok(match git.repo.head()? {
        Head::Branch { name, .. } => name,
        Head::Detached(_) => "(no branch)".into(),
    })
}

fn commit_with(
    git: &GitHome,
    tree: ObjectId,
    parents: Vec<ObjectId>,
    message: String,
) -> Result<ObjectId> {
    let record = Commit {
        tree,
        parents,
        author: identity(git, "author")?,
        committer: identity(git, "committer")?,
        extra_headers: Vec::new(),
        message: message.into_bytes(),
    };
    Ok(git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode())?)
}

fn tree_of_entries(git: &GitHome, entries: Entries, version: u32) -> Result<ObjectId> {
    write_tree(git, &index_of(version, entries))
}

fn push(git: &GitHome, args: PushArgs) -> Result<()> {
    if !args.paths.is_empty() {
        bail!(
            "`pyn stash` of named paths is not supported yet; stash everything, or use `pyn add` and `pyn commit`"
        );
    }
    refuse_in_progress(git).map_err(|e| Refused(e.to_string()))?;
    let Some(head) = git.repo.head_commit()? else {
        bail!("You do not have the initial commit yet");
    };
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        return Err(Refused(
            "Cannot save the current index state: the index has unmerged paths".into(),
        )
        .into());
    }
    let head_tree = git.repo.commit(&head)?.tree;
    let committed = git.repo.flatten(Some(&head_tree), &Pathspec::default())?;
    let leaves = git.repo.index_leaves(&opened.index);
    let unstaged = git.repo.unstaged_changes(&opened.index)?;
    let version = opened.index.version;

    let index_tree = tree_of_entries(git, entries_of(&opened.index), version)?;
    let mut work = entries_of(&opened.index);
    let paths: std::collections::BTreeSet<&Vec<u8>> =
        committed.keys().chain(leaves.keys()).collect();
    for path in paths {
        if leaves.get(path) == committed.get(path) && !unstaged.contains_key(path) {
            continue;
        }
        let file = fs_path(git.repo.root()?, path);
        if std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
        {
            let existing = work.get(&(path.clone(), 0)).cloned();
            work.insert((path.clone(), 0), stage_file(git, path, existing.as_ref())?);
        } else {
            work.remove(&(path.clone(), 0));
        }
    }
    let work_tree = tree_of_entries(git, work, version)?;
    let untracked = if args.include_untracked {
        untracked_files(git, &opened.index)?
    } else {
        Vec::new()
    };
    if index_tree == head_tree && work_tree == head_tree && untracked.is_empty() {
        println!("No local changes to save");
        return Ok(());
    }
    let branch = branch_label(git)?;
    let base = format!("{branch}: {} {}", short(&head), subject(git, &head)?);
    let index_commit = commit_with(git, index_tree, vec![head], format!("index on {base}\n"))?;
    let mut parents = vec![head, index_commit];
    if !untracked.is_empty() {
        let mut entries = Entries::new();
        for path in &untracked {
            entries.insert((path.clone(), 0), stage_file(git, path, None)?);
        }
        let tree = tree_of_entries(git, entries, version)?;
        parents.push(commit_with(
            git,
            tree,
            Vec::new(),
            format!("untracked files on {base}\n"),
        )?);
    }
    let message = match &args.message {
        Some(m) => format!("On {branch}: {m}"),
        None => format!("WIP on {base}"),
    };
    let stash = commit_with(git, work_tree, parents, message.clone())?;
    let old = git.repo.refs().resolve(STASH)?;
    git.repo
        .refs()
        .update_checked(STASH, old.map_or(Expect::Absent, Expect::Id), &stash)?;
    git.log_ref(STASH, old.as_ref(), &stash, &message, true)?;

    if let Err(blocked) = move_tree(
        git,
        &opened,
        Some(&head_tree),
        Some(&head_tree),
        Mode::Force,
    )? {
        blocked.report("stash", "stash", 1);
    }
    std::fs::write(
        git.repo.git().git_dir().join("ORIG_HEAD"),
        format!("{head}\n"),
    )?;
    git.log_ref("HEAD", Some(&head), &head, "reset: moving to HEAD", false)?;
    for path in &untracked {
        remove_entry_file(git, path)?;
    }
    println!("Saved working directory and index state {message}");
    Ok(())
}

fn commit_tree(git: &GitHome, id: &ObjectId) -> Result<ObjectId> {
    Ok(git.repo.commit(id)?.tree)
}

fn apply(git: &GitHome, name: Option<&str>, pop: bool) -> Result<()> {
    refuse_in_progress(git).map_err(|e| Refused(e.to_string()))?;
    let (n, id) = find(git, name)?;
    let stash = git.repo.commit(&id)?;
    if stash.parents.len() < 2 {
        bail!("{} is not a stash commit", id);
    }
    if git.repo.head_commit()?.is_none() {
        bail!("You do not have the initial commit yet");
    }
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        bail!("Cannot apply a stash: the index has unmerged paths");
    }
    let version = opened.index.version;
    let base_tree = commit_tree(git, &stash.parents[0])?;
    let current_tree = tree_of_entries(git, entries_of(&opened.index), version)?;
    let merged = merge_trees(
        git,
        Some(&base_tree),
        Some(&current_tree),
        Some(&stash.tree),
        &Labels {
            ours: "Updated upstream",
            theirs: "Stashed changes",
            base: "Stash base",
        },
    )?;
    let ours = git.repo.index_leaves(&opened.index);
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let ignores = super::stage::Ignores {
        ignore: git.repo.ignore()?,
    };
    let none = HashSet::new();
    let mut blocked = Blocked::default();
    let mut writes: Vec<&[u8]> = Vec::new();
    for (path, resolved) in &merged.paths {
        let here = ours.get(path);
        let changes = match resolved {
            Resolved::Take(e) => e.as_ref() != here,
            Resolved::Conflict(c) => !matches!(c.worktree, Worktree::Keep),
        };
        if !changes {
            continue;
        }
        if !matches!(resolved, Resolved::Take(None)) {
            writes.push(path);
        }
        match here {
            Some(_) if matches!(dirty.get(path), Some(Kind::Modified | Kind::TypeChange)) => {
                blocked.local.push(path.clone());
            }
            None if in_the_way(git, &ignores, path, &none)? => blocked.untracked.push(path.clone()),
            _ => {}
        }
    }
    let restore_untracked: BTreeMap<Vec<u8>, Entry> = match stash.parents.get(2) {
        Some(u) => {
            let tree = commit_tree(git, u)?;
            git.repo.flatten(Some(&tree), &Pathspec::default())?
        }
        None => BTreeMap::new(),
    };
    if !blocked.local.is_empty() || !blocked.untracked.is_empty() {
        blocked.report("merge", "merge", 1);
    }
    convert::refuse_conversions(
        git,
        writes
            .into_iter()
            .chain(restore_untracked.keys().map(Vec::as_slice)),
    )?;
    let in_the_way: Vec<&Vec<u8>> = restore_untracked
        .keys()
        .filter(|p| {
            std::fs::symlink_metadata(fs_path(git.repo.root().expect("a work tree"), p)).is_ok()
        })
        .collect();

    let mut entries = entries_of(&opened.index);
    let mut added: Vec<IndexEntry> = Vec::new();
    let mut conflicted = false;
    for (path, resolved) in &merged.paths {
        if merged.line_merged.contains(path) {
            println!("Auto-merging {}", path_string(path));
        }
        match resolved {
            Resolved::Take(Some(entry)) => {
                if ours.get(path) != Some(entry) {
                    let written = write_entry(git, path, entry)?;
                    if !ours.contains_key(path) {
                        added.push(written.clone());
                    }
                    put_entry(&mut entries, written);
                }
            }
            Resolved::Take(None) => {
                if ours.contains_key(path) {
                    remove_entry_file(git, path)?;
                    super::stage::remove_entry(&mut entries, path);
                }
            }
            Resolved::Conflict(c) => {
                conflicted = true;
                println!(
                    "CONFLICT (content): Merge conflict in {}",
                    path_string(path)
                );
                if let Worktree::Write { mode, bytes } = &c.worktree {
                    write_bytes(git, path, *mode, bytes)?;
                }
                super::stage::remove_entry(&mut entries, path);
                for (stage, side) in [(1u8, &c.base), (2, &c.ours), (3, &c.theirs)] {
                    if let Some(e) = side {
                        let mut entry = IndexEntry::new(path.clone(), raw_mode(e.mode), e.id);
                        entry.stage = stage;
                        entries.insert((path.clone(), stage), entry);
                    }
                }
            }
        }
    }
    for (path, entry) in &restore_untracked {
        if !in_the_way.contains(&path) {
            write_entry(git, path, entry)?;
        }
    }
    let index: Index = if conflicted {
        index_of(version, entries)
    } else {
        let mut keep = entries_of(&opened.index);
        for entry in added {
            put_entry(&mut keep, entry);
        }
        index_of(version, keep)
    };
    git.save_index(&opened, &index)?;
    if !in_the_way.is_empty() {
        for path in &in_the_way {
            eprintln!("{} already exists, no checkout", path_string(path));
        }
        return Err(Refused("could not restore untracked files from stash".into()).into());
    }
    if conflicted {
        println!("The stash entry is kept in case you need it again.");
        std::process::exit(1);
    }
    super::commands::status(git)?;
    if pop {
        drop_at(git, n)?;
    }
    Ok(())
}

/// Removes entry `n` (0 is the newest) and rewrites the reflog chain, as `git stash drop` does.
fn drop_at(git: &GitHome, n: usize) -> Result<()> {
    let mut log = git.repo.refs().read_reflog(STASH)?;
    let at = log.len() - 1 - n;
    let gone = log.remove(at);
    let mut previous = git.zero_id();
    for e in &mut log {
        e.old = previous;
        previous = e.new;
    }
    match log.last() {
        Some(top) => {
            let top = top.new;
            git.repo.refs().update_checked(STASH, Expect::Any, &top)?;
            git.repo.refs().write_reflog(STASH, &log)?;
        }
        None => {
            git.repo.refs().delete(STASH, Expect::Any)?;
            git.repo.refs().write_reflog(STASH, &[])?;
        }
    }
    println!("Dropped {STASH}@{{{n}}} ({})", gone.new);
    Ok(())
}

fn drop_entry(git: &GitHome, name: Option<&str>) -> Result<()> {
    let (n, _) = find(git, name)?;
    drop_at(git, n)
}

fn clear(git: &GitHome) -> Result<()> {
    if git.repo.refs().resolve(STASH)?.is_some() {
        git.repo.refs().delete(STASH, Expect::Any)?;
    }
    git.repo.refs().write_reflog(STASH, &[])?;
    Ok(())
}

//! `pyn reset` on a git repository: moves the current branch (or a detached `HEAD`) and resets the index and work tree
//! by mode, or resets only the index entries of the named paths, as `git reset` does.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use pyn_git::{Expect, IndexEntry};
use pyn_gitwork::{Entry, Kind, Pathspec, fs_path};
use pyn_object::ObjectId;

use super::stage::{
    Entries, Ignores, OpenIndex, clear_merge_state, entries_of, index_of, merge_heads, raw_mode,
    resolve_specs,
};
use super::switch::subject;
use super::worktree::{
    Blocked, Mode, check_writes, in_the_way, move_tree, remove_entry_file, write_entry,
};
use super::{GitHome, convert, path_string};
use crate::local_reset::ResetArgs;

const ABBREV: usize = 7;
const BRANCH_STATE: [&str; 4] = ["MERGE_RR", "SQUASH_MSG", "CHERRY_PICK_HEAD", "REVERT_HEAD"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    Soft,
    Mixed,
    Hard,
    Keep,
    Merge,
}

impl ResetKind {
    fn word(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Mixed => "mixed",
            Self::Hard => "hard",
            Self::Keep => "keep",
            Self::Merge => "merge",
        }
    }
}

enum Act {
    Write(Entry),
    Remove,
}

fn exists(cwd: &Path, name: &str) -> bool {
    std::fs::symlink_metadata(cwd.join(name)).is_ok()
}

/// The revision (if named) and the paths a command line means, as git decides between them.
fn split(git: &GitHome, cwd: &Path, args: &ResetArgs) -> Result<(Option<String>, Vec<String>)> {
    let mut list = args.revs.clone();
    if !args.paths.is_empty() || std::env::args_os().any(|a| a == "--") {
        list.push("--".into());
        list.extend(args.paths.iter().cloned());
    }
    let Some(first) = list.first() else {
        return Ok((None, Vec::new()));
    };
    if first == "--" {
        return Ok((None, list[1..].to_vec()));
    }
    if list.get(1).is_some_and(|s| s == "--") {
        return Ok((Some(first.clone()), list[2..].to_vec()));
    }
    if git.repo.resolve(first).is_ok() {
        if exists(cwd, first) {
            bail!(
                "ambiguous argument '{first}': both revision and filename\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'pyn <command> [<revision>...] -- [<file>...]'"
            );
        }
        return Ok((Some(first.clone()), list[1..].to_vec()));
    }
    if !exists(cwd, first) {
        bail!(
            "ambiguous argument '{first}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'pyn <command> [<revision>...] -- [<file>...]'"
        );
    }
    Ok((None, list))
}

fn tree_entries(
    git: &GitHome,
    commit: Option<&ObjectId>,
    spec: &Pathspec,
) -> Result<BTreeMap<Vec<u8>, Entry>> {
    let tree = match commit {
        Some(c) => Some(git.repo.commit(c)?.tree),
        None => None,
    };
    Ok(git.repo.flatten(tree.as_ref(), spec)?)
}

/// What `reset` prints for what is still modified in the work tree after the index changed.
fn report_unstaged(git: &GitHome, opened_index: &pyn_git::Index) -> Result<()> {
    let dirty = git.repo.unstaged_changes(opened_index)?;
    let mut first = true;
    for (path, kind) in &dirty {
        let letter = match kind {
            Kind::Modified => 'M',
            Kind::Deleted => 'D',
            Kind::TypeChange => 'T',
            _ => continue,
        };
        if first {
            println!("Unstaged changes after reset:");
            first = false;
        }
        println!("{letter}\t{}", path_string(path));
    }
    Ok(())
}

/// The index with the stage-0 entries of `target` (existing identical entries keep their stat data) in place of
/// every entry whose path `replace` covers; the others stay.
fn index_with(
    opened: &OpenIndex,
    replace: &Pathspec,
    target: &BTreeMap<Vec<u8>, Entry>,
    mut written: BTreeMap<Vec<u8>, IndexEntry>,
) -> pyn_git::Index {
    let old = entries_of(&opened.index);
    let mut entries: Entries = old
        .iter()
        .filter(|((p, _), _)| !replace.matches(p))
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    for (path, e) in target {
        let kept = old
            .get(&(path.clone(), 0))
            .filter(|x| x.id == e.id && x.mode == raw_mode(e.mode));
        let entry = written
            .remove(path)
            .or_else(|| kept.cloned())
            .unwrap_or_else(|| IndexEntry::new(path.clone(), raw_mode(e.mode), e.id));
        entries.insert((path.clone(), 0), entry);
    }
    index_of(opened.index.version, entries)
}

/// The paths `--keep` or `--merge` must write or remove, or the ones that stop it.
fn plan(
    git: &GitHome,
    opened: &OpenIndex,
    kind: ResetKind,
    from: &BTreeMap<Vec<u8>, Entry>,
    to: &BTreeMap<Vec<u8>, Entry>,
) -> Result<std::result::Result<BTreeMap<Vec<u8>, Act>, Blocked>> {
    let leaves = git.repo.index_leaves(&opened.index);
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let modified = |p: &Vec<u8>| matches!(dirty.get(p), Some(Kind::Modified | Kind::TypeChange));
    let unmerged: BTreeSet<&Vec<u8>> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.stage > 0)
        .map(|e| &e.path)
        .collect();
    let mut blocked = Blocked::default();
    if kind == ResetKind::Keep && !unmerged.is_empty() {
        blocked.local = unmerged.iter().map(|p| (*p).clone()).collect();
        return Ok(Err(blocked));
    }
    let mut acts: BTreeMap<Vec<u8>, Act> = BTreeMap::new();
    let mut absent: Vec<Vec<u8>> = Vec::new();
    let paths: BTreeSet<&Vec<u8>> = from
        .keys()
        .chain(to.keys())
        .chain(leaves.keys())
        .chain(unmerged.iter().copied())
        .collect();
    let on_disk = |p: &Vec<u8>| {
        git.repo
            .root()
            .is_ok_and(|r| std::fs::symlink_metadata(fs_path(r, p)).is_ok())
    };
    for path in paths {
        let (h, m, i) = (from.get(path), to.get(path), leaves.get(path));
        let act = match m {
            Some(e) => Act::Write(*e),
            None => Act::Remove,
        };
        if unmerged.contains(path) {
            if kind == ResetKind::Merge {
                acts.insert(path.clone(), act);
            }
            continue;
        }
        let keep = kind == ResetKind::Keep;
        match i {
            None => match (h, m) {
                (_, Some(_)) if !keep || h.is_none() => {
                    absent.push(path.clone());
                    acts.insert(path.clone(), act);
                }
                (Some(_), None) if keep && on_disk(path) => blocked.untracked.push(path.clone()),
                (Some(h), Some(m)) if keep && h != m => blocked.local.push(path.clone()),
                _ => {}
            },
            Some(i) => {
                if m == Some(i) || (keep && h == m) {
                    continue;
                }
                let free = !modified(path) && (!keep || h == Some(i));
                if free {
                    acts.insert(path.clone(), act);
                } else {
                    blocked.local.push(path.clone());
                }
            }
        }
    }
    let removed: HashSet<&[u8]> = acts
        .iter()
        .filter(|(_, a)| matches!(a, Act::Remove))
        .map(|(p, _)| p.as_slice())
        .collect();
    let ignores = Ignores {
        ignore: git.repo.ignore()?,
    };
    for path in &absent {
        if in_the_way(git, &ignores, path, &removed)? {
            blocked.untracked.push(path.clone());
        }
    }
    if blocked.local.is_empty() && blocked.untracked.is_empty() {
        Ok(Ok(acts))
    } else {
        Ok(Err(blocked))
    }
}

fn refuse(blocked: &Blocked, rev: &str) -> anyhow::Error {
    for p in blocked.local.iter().chain(&blocked.untracked) {
        eprintln!(
            "error: Entry '{}' would be overwritten by merge. Cannot merge.",
            path_string(p)
        );
    }
    anyhow::anyhow!("Could not reset index file to revision '{rev}'.")
}

fn reset_paths(
    git: &GitHome,
    cwd: &Path,
    paths: &[String],
    target: Option<&ObjectId>,
    quiet: bool,
) -> Result<()> {
    let opened = git.open_index()?;
    let specs = resolve_specs(git, cwd, paths)?;
    let entries = tree_entries(git, target, &specs.spec)?;
    let index = index_with(&opened, &specs.spec, &entries, BTreeMap::new());
    git.save_index(&opened, &index)?;
    if !quiet {
        report_unstaged(git, &index)?;
    }
    Ok(())
}

pub fn reset(git: &GitHome, cwd: &Path, args: &ResetArgs) -> Result<()> {
    reset_logging(git, cwd, args, true)
}

/// A reset that moves `HEAD` without a reflog line, for a caller that logs the move in its own words.
pub(super) fn reset_silently(git: &GitHome, cwd: &Path, args: &ResetArgs) -> Result<()> {
    reset_logging(git, cwd, args, false)
}

fn reset_logging(git: &GitHome, cwd: &Path, args: &ResetArgs, log: bool) -> Result<()> {
    if args.yes {
        bail!("`--yes` confirms `--hard` on a pyn server workspace; a git repository does not ask");
    }
    let kind = args.kind();
    let (rev, paths) = split(git, cwd, args)?;
    if args.patch {
        if args.has_mode() {
            bail!("options '--patch' and '--{{hard,mixed,soft}}' cannot be used together");
        }
        return super::patch_mode::reset(git, cwd, rev.as_deref(), &paths);
    }
    let rev = rev.unwrap_or_else(|| "HEAD".into());
    let head = git.repo.head_commit()?;
    let unborn = rev == "HEAD" && head.is_none();
    let target = if unborn {
        None
    } else {
        match git.repo.resolve(&rev) {
            Ok(id) => Some(id),
            Err(_) => bail!("Failed to resolve '{rev}' as a valid revision."),
        }
    };
    if !paths.is_empty() {
        if kind != ResetKind::Mixed {
            bail!("Cannot do {} reset with paths.", kind.word());
        }
        if args.mixed() {
            eprintln!(
                "warning: --mixed with paths is deprecated; use 'pyn reset -- <paths>' instead."
            );
        }
        return reset_paths(git, cwd, &paths, target.as_ref(), args.quiet);
    }
    let opened = git.open_index()?;
    if kind == ResetKind::Soft
        && (!merge_heads(git)?.is_empty() || opened.index.entries.iter().any(|e| e.stage > 0))
    {
        bail!("Cannot do a soft reset in the middle of a merge.");
    }
    if kind == ResetKind::Keep && unborn {
        eprintln!("error: You do not have a valid HEAD.");
        bail!("Could not reset index file to revision '{rev}'.");
    }
    if kind != ResetKind::Soft && opened.index.entries.iter().any(|e| e.skip_worktree) {
        bail!("this repository uses sparse checkout, which pyn does not support yet");
    }
    let all = Pathspec::default();
    let new_tree = match &target {
        Some(c) => Some(git.repo.commit(c)?.tree),
        None => None,
    };
    let to = git.repo.flatten(new_tree.as_ref(), &all)?;
    match kind {
        ResetKind::Soft => {}
        ResetKind::Mixed => {
            let index = index_with(&opened, &all, &to, BTreeMap::new());
            git.save_index(&opened, &index)?;
        }
        ResetKind::Hard => {
            let old_tree = match &head {
                Some(h) => Some(git.repo.commit(h)?.tree),
                None => None,
            };
            if let Err(blocked) = move_tree(
                git,
                &opened,
                old_tree.as_ref(),
                new_tree.as_ref(),
                Mode::Force,
            )? {
                return Err(refuse(&blocked, &rev));
            }
        }
        ResetKind::Keep | ResetKind::Merge => {
            let from = tree_entries(git, head.as_ref(), &all)?;
            let acts = match plan(git, &opened, kind, &from, &to)? {
                Ok(acts) => acts,
                Err(blocked) => return Err(refuse(&blocked, &rev)),
            };
            check_writes(to.keys().chain(acts.keys()))?;
            convert::refuse_conversions(
                git,
                acts.iter().filter_map(|(p, a)| match a {
                    Act::Write(e) => Some((p.as_slice(), e.mode)),
                    Act::Remove => None,
                }),
            )?;
            let mut written = BTreeMap::new();
            for (path, act) in &acts {
                match act {
                    Act::Remove => remove_entry_file(git, path)?,
                    Act::Write(e) => {
                        written.insert(path.clone(), write_entry(git, path, e)?);
                    }
                }
            }
            let index = index_with(&opened, &all, &to, written);
            git.save_index(&opened, &index)?;
        }
    }
    if let Some(old) = &head {
        super::merge::write_orig_head_as(git, old, Some("reset"))?;
    }
    if let Some(new) = &target
        && !unborn
    {
        let message = format!("reset: moving to {rev}");
        if !log {
            let name = git.head_target()?.unwrap_or_else(|| "HEAD".into());
            git.repo.refs().update_checked(&name, Expect::Any, new)?;
        } else if head.as_ref() == Some(new) {
            if git.head_target()?.is_some() {
                git.log_ref("HEAD", head.as_ref(), new, &message, false)?;
            }
        } else {
            git.move_head(new, &message)?;
        }
    }
    clear_merge_state(git);
    let dir = git.repo.git().git_dir();
    for name in BRANCH_STATE {
        let _ = std::fs::remove_file(dir.join(name));
    }
    if args.quiet {
        return Ok(());
    }
    match (kind, &target) {
        (ResetKind::Hard, Some(id)) if !unborn => {
            println!(
                "HEAD is now at {} {}",
                &id.to_hex()[..ABBREV],
                subject(git, id)?
            );
        }
        (ResetKind::Mixed, _) => report_unstaged(git, &git.open_index()?.index)?,
        _ => {}
    }
    Ok(())
}

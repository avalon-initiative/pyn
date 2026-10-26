//! `pyn rm` and `pyn mv` on a git repository: they change the index and the work tree together, as `git rm` and
//! `git mv` do, and refuse before writing anything.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};
use pyn_gitwork::{Kind, Match, Pathspec, fs_path};

use super::stage::{GITLINK, entries_of, index_of, put_entry, raw_mode, remove_entry};
use super::worktree::{check_writes, remove_entry_file_keeping};
use super::{GitHome, Refused, path_string};
use crate::mv_plan::{self, MvArgs};
use crate::rm_mv::RmArgs;

fn list(title: &str, advice: &str, paths: &[&Vec<u8>]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let names: String = paths
        .iter()
        .map(|p| format!("    {}\n", path_string(p)))
        .collect();
    Some(format!("{title}\n{names}({advice})"))
}

/// `pyn rm`: drops the paths from the index and, without `--cached`, from the work tree.
pub fn rm(git: &GitHome, cwd: &Path, args: &RmArgs) -> Result<()> {
    let opened = git.open_index()?;
    let spec = &git.pathspec(cwd, &args.paths, true)?;
    let (picked, seen) = spec.select(opened.index.entries.iter().map(|e| e.path.as_slice()));
    for (n, how) in seen
        .iter()
        .enumerate()
        .filter(|(n, _)| !spec.item_is_exclude(*n))
    {
        let arg = String::from_utf8_lossy(spec.original(n));
        if how.is_none() {
            if args.ignore_unmatch {
                continue;
            }
            bail!("pathspec '{arg}' did not match any files");
        }
        if !args.recursive && *how == Some(Match::Recursive) {
            let shown = if arg.is_empty() { ".".into() } else { arg };
            bail!("not removing '{shown}' recursively without -r");
        }
    }
    let outside = super::sparse::outside_only(spec, &opened.index, &[]);
    let skipped: BTreeSet<&[u8]> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.skip_worktree)
        .map(|e| e.path.as_slice())
        .collect();
    let matched: BTreeSet<Vec<u8>> = picked
        .into_iter()
        .filter(|p| !skipped.contains(p))
        .map(<[u8]>::to_vec)
        .collect();
    if matched.is_empty() {
        if super::sparse::refuse_outside(&outside) {
            std::process::exit(1);
        }
        return Ok(());
    }
    if !args.cached
        && let Some(e) = opened
            .index
            .entries
            .iter()
            .find(|e| e.mode == GITLINK && matched.contains(&e.path))
    {
        bail!(
            "'{}' is a submodule; pyn removes it from the index only, with --cached",
            path_string(&e.path)
        );
    }
    if !args.force {
        check_removable(git, &opened.index, &matched, args.cached)?;
    }
    if !args.quiet {
        for path in &matched {
            println!("rm '{}'", path_string(path));
        }
    }
    if args.dry_run {
        return Ok(());
    }
    let mut entries = entries_of(&opened.index);
    for path in &matched {
        remove_entry(&mut entries, path);
    }
    if !args.cached {
        check_writes(&matched)?;
        let root = git.repo.root()?;
        if let Some(dir) = matched
            .iter()
            .find(|p| std::fs::symlink_metadata(fs_path(root, p)).is_ok_and(|m| m.is_dir()))
        {
            bail!("git rm: '{}': Is a directory", path_string(dir));
        }
        for path in &matched {
            remove_entry_file_keeping(git, path, Some(cwd))?;
        }
    }
    git.save_index(&opened, &index_of(opened.index.version, entries))?;
    if super::sparse::refuse_outside(&outside) {
        std::process::exit(1);
    }
    Ok(())
}

/// Refuses, as git does, a path whose index entry differs from the work tree file or from `HEAD`.
fn check_removable(
    git: &GitHome,
    index: &pyn_git::Index,
    paths: &BTreeSet<Vec<u8>>,
    cached: bool,
) -> Result<()> {
    let tree = match git.repo.head_commit()? {
        Some(c) => Some(git.repo.commit(&c)?.tree),
        None => None,
    };
    let spec = Pathspec::new(paths.iter().cloned());
    let head = git.repo.flatten(tree.as_ref(), &spec)?;
    let dirty = git.repo.unstaged_changes(index)?;
    let (mut staged, mut cache, mut local) = (Vec::new(), Vec::new(), Vec::new());
    for path in paths {
        let Some(entry) = index
            .entries
            .iter()
            .find(|e| e.stage == 0 && &e.path == path)
        else {
            continue;
        };
        let changed = match dirty.get(path) {
            Some(Kind::Deleted) => continue,
            Some(Kind::Modified | Kind::TypeChange) => true,
            _ => false,
        };
        let differs = entry.intent_to_add
            || head
                .get(path)
                .is_none_or(|h| h.id != entry.id || raw_mode(h.mode) != entry.mode);
        match (changed, differs) {
            (true, true) => staged.push(path),
            _ if cached => {}
            (false, true) => cache.push(path),
            (true, false) => local.push(path),
            _ => {}
        }
    }
    let notes: Vec<String> = [
        list(
            "the following files have staged content different from both the file and the HEAD:",
            "use -f to force removal",
            &staged,
        ),
        list(
            "the following files have changes staged in the index:",
            "use --cached to keep the file, or -f to force removal",
            &cache,
        ),
        list(
            "the following files have local modifications:",
            "use --cached to keep the file, or -f to force removal",
            &local,
        ),
    ]
    .into_iter()
    .flatten()
    .collect();
    if notes.is_empty() {
        return Ok(());
    }
    Err(Refused(notes.join("\nerror: ")).into())
}

/// `pyn mv`: renames files and folders in the work tree and moves their index entries along, content untouched.
pub fn mv(git: &GitHome, cwd: &Path, args: &MvArgs) -> Result<()> {
    let opened = git.open_index()?;
    let named = args
        .paths
        .iter()
        .map(|a| Ok((a.clone(), git.ws.repo_prefix(cwd, a)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut tracked = BTreeSet::new();
    let mut unmerged = BTreeSet::new();
    for e in &opened.index.entries {
        if e.stage == 0 {
            tracked.insert(e.path.clone());
        } else {
            unmerged.insert(e.path.clone());
        }
    }
    let pairs = mv_plan::plan(git.repo.root()?, &tracked, &unmerged, args, &named)?;
    check_writes(pairs.iter().flat_map(|p| [&p.src, &p.dst]))?;
    let mut entries = entries_of(&opened.index);
    let mut failure = None;
    for pair in &pairs {
        if args.verbose || args.dry_run {
            println!(
                "Renaming {} to {}",
                path_string(&pair.src),
                path_string(&pair.dst)
            );
        }
        if args.dry_run {
            continue;
        }
        let root = git.repo.root()?;
        if let Err(e) = std::fs::rename(fs_path(root, &pair.src), fs_path(root, &pair.dst)) {
            if args.skip {
                continue;
            }
            failure = Some(anyhow::anyhow!(
                "renaming '{}' failed: {e}",
                path_string(&pair.src)
            ));
            break;
        }
        let moved: Vec<_> = pair
            .files
            .iter()
            .filter_map(|(from, to)| entries.remove(&(from.clone(), 0)).map(|e| (to, e)))
            .collect();
        for (to, mut entry) in moved {
            entry.path = to.clone();
            put_entry(&mut entries, entry);
        }
    }
    if !args.dry_run && !pairs.is_empty() {
        git.save_index(&opened, &index_of(opened.index.version, entries))?;
    }
    failure.map_or(Ok(()), Err)
}

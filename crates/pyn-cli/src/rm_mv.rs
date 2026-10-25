//! `pyn rm` and `pyn mv` in a workspace of a pyn server: they stage a deletion, or a deletion and an addition, and
//! change the files on disk. An exclusive path needs its lock first, as for `add`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::Args;
use pyn_gitwork::Match;

use crate::backend::Backend;
use crate::git_home::Refused;
use crate::mv_plan::{self, MvArgs};
use crate::staging::{Entry, Index, require_locks, store};
use crate::workspace::{FileState, Workspace, hash_of};
use crate::{localfile, merging};

#[derive(Args, Debug)]
pub struct RmArgs {
    /// Show what would be removed without removing it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    #[arg(short, long)]
    pub quiet: bool,
    /// Stage the removal and leave the files on disk.
    #[arg(long)]
    pub cached: bool,
    /// Remove files that have local or staged changes.
    #[arg(short, long)]
    pub force: bool,
    /// Allow a folder to be removed with what is in it.
    #[arg(short = 'r')]
    pub recursive: bool,
    /// Succeed when a path matches nothing.
    #[arg(long)]
    pub ignore_unmatch: bool,
    pub paths: Vec<String>,
}

type State = BTreeMap<String, FileState>;

/// The files a commit would keep: those the workspace knows without a staged deletion, and those staged as new.
fn tracked(state: &State, index: &Index) -> BTreeSet<String> {
    let kept = state
        .keys()
        .filter(|p| !index.entries.get(*p).is_some_and(|e| e.deleted));
    let added = index
        .entries
        .iter()
        .filter(|(_, e)| !e.deleted)
        .map(|(p, _)| p);
    kept.chain(added).cloned().collect()
}

fn bytes(set: impl IntoIterator<Item = String>) -> BTreeSet<Vec<u8>> {
    set.into_iter().map(String::into_bytes).collect()
}

fn text(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn stage_removal(index: &mut Index, state: &State, path: &str) {
    match state.get(path) {
        Some(k) => {
            index.entries.insert(
                path.to_string(),
                Entry {
                    hash: String::new(),
                    file_mode: k.file_mode,
                    base_revision: Some(k.revision),
                    deleted: true,
                },
            );
        }
        None => {
            index.entries.remove(path);
        }
    }
}

fn require_known_locks(backend: &dyn Backend, state: &State, paths: &[&String]) -> Result<()> {
    let known: Vec<&String> = paths
        .iter()
        .copied()
        .filter(|p| state.contains_key(*p))
        .collect();
    require_locks(backend, &known)
}

fn list(title: &str, advice: &str, paths: &[&String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let names: String = paths.iter().map(|p| format!("    {p}\n")).collect();
    Some(format!("{title}\n{names}({advice})"))
}

/// Refuses a file whose content differs from what is staged or committed, or whose staged content differs from
/// the last commit, as `git rm` does.
fn check_removable(
    ws: &Workspace,
    state: &State,
    index: &Index,
    paths: &BTreeSet<String>,
    cached: bool,
) -> Result<()> {
    let (mut staged, mut cache, mut local) = (Vec::new(), Vec::new(), Vec::new());
    for path in paths {
        let file = ws.abs(path);
        if !localfile::exists(&file) || file.is_dir() {
            continue;
        }
        let (mode, content) = localfile::read(&file)?;
        let entry = index.entries.get(path).filter(|e| !e.deleted);
        let known = state.get(path);
        let changed = match (entry, known) {
            (Some(e), _) => e.differs(mode, &content),
            (None, Some(k)) => k.changed(mode, &content),
            (None, None) => false,
        };
        let differs = entry.is_some_and(|e| {
            known.is_none_or(|k| {
                k.legacy || k.hash != e.hash || localfile::mode_differs(k.file_mode, e.file_mode)
            })
        });
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
            "the following files have staged content different from both the file and the last commit:",
            "use -f to force removal",
            &staged,
        ),
        list(
            "the following files have changes staged:",
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

fn remove_file(ws: &Workspace, path: &str) -> Result<()> {
    let file = ws.abs(path);
    if std::fs::symlink_metadata(&file).is_ok_and(|m| !m.is_dir()) {
        std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?;
    }
    let mut dir = file.parent();
    while let Some(d) = dir.filter(|d| *d != ws.root && d.starts_with(&ws.root)) {
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
    Ok(())
}

/// `pyn rm`: stages the deletion of the tracked files under the paths and removes them from disk.
pub fn rm(backend: &dyn Backend, ws: &Workspace, cwd: &Path, args: &RmArgs) -> Result<()> {
    let mut index = Index::load(ws)?;
    let state = ws.load_state()?;
    let known = tracked(&state, &index);
    let spec = crate::pathspec::parse(
        ws,
        cwd,
        &args.paths,
        Default::default(),
        true,
        crate::usage_error,
    )?;
    let (picked, seen) = spec.select(known.iter().map(|p| p.as_bytes()));
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
    let matched: BTreeSet<String> = picked
        .into_iter()
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    let conflicted = merging::conflicted(ws);
    if let Some(p) = matched.iter().find(|p| conflicted.contains(*p)) {
        bail!("{p} has a merge conflict; resolve it and stage it with `pyn add` first");
    }
    if matched.is_empty() {
        return Ok(());
    }
    if !args.force {
        check_removable(ws, &state, &index, &matched, args.cached)?;
    }
    let paths: Vec<&String> = matched.iter().collect();
    require_known_locks(backend, &state, &paths)?;
    if !args.quiet {
        for path in &matched {
            println!("rm '{path}'");
        }
    }
    if args.dry_run {
        return Ok(());
    }
    ws.check_paths(&matched)?;
    for path in &matched {
        stage_removal(&mut index, &state, path);
        if !args.cached {
            remove_file(ws, path)?;
        }
    }
    index.save(ws)
}

/// Stages the moved file at its new path (nothing when it matches the last commit there) and its old path's removal.
fn stage_move(
    ws: &Workspace,
    state: &State,
    index: &mut Index,
    from: &str,
    to: &str,
) -> Result<()> {
    let (mode, content) = localfile::read(&ws.abs(to))?;
    match state.get(to) {
        Some(k) if !k.changed(mode, &content) => {
            index.entries.remove(to);
        }
        known => {
            let hash = hash_of(&content);
            store(ws, &hash, &content)?;
            index.entries.insert(
                to.to_string(),
                Entry {
                    hash,
                    file_mode: mode,
                    base_revision: known.map(|k| k.revision),
                    deleted: false,
                },
            );
        }
    }
    stage_removal(index, state, from);
    Ok(())
}

/// `pyn mv`: renames on disk and stages each moved file as a deletion at the old path and an addition at the new.
pub fn mv(backend: &dyn Backend, ws: &Workspace, cwd: &Path, args: &MvArgs) -> Result<()> {
    let mut index = Index::load(ws)?;
    let state = ws.load_state()?;
    let conflicted = merging::conflicted(ws);
    let known: BTreeSet<String> = tracked(&state, &index)
        .into_iter()
        .filter(|p| !conflicted.contains(p))
        .collect();
    let named = args
        .paths
        .iter()
        .map(|a| Ok((a.clone(), ws.repo_prefix(cwd, a)?)))
        .collect::<Result<Vec<_>>>()?;
    let pairs = mv_plan::plan(&ws.root, &bytes(known), &bytes(conflicted), args, &named)?;
    let moved: Vec<(String, String)> = pairs
        .iter()
        .flat_map(|p| p.files.iter().map(|(a, b)| (text(a), text(b))))
        .collect();
    ws.check_paths(moved.iter().flat_map(|(a, b)| [a, b]))?;
    let touched: Vec<&String> = moved.iter().flat_map(|(a, b)| [a, b]).collect();
    require_known_locks(backend, &state, &touched)?;
    let mut failure = None;
    for pair in &pairs {
        if args.verbose || args.dry_run {
            println!("Renaming {} to {}", text(&pair.src), text(&pair.dst));
        }
        if args.dry_run {
            continue;
        }
        if let Err(e) = std::fs::rename(ws.abs(&text(&pair.src)), ws.abs(&text(&pair.dst))) {
            if args.skip {
                continue;
            }
            failure = Some(anyhow::anyhow!(
                "renaming '{}' failed: {e}",
                text(&pair.src)
            ));
            break;
        }
        for (from, to) in &pair.files {
            stage_move(ws, &state, &mut index, &text(from), &text(to))?;
        }
    }
    if !args.dry_run && !pairs.is_empty() {
        index.save(ws)?;
    }
    failure.map_or(Ok(()), Err)
}

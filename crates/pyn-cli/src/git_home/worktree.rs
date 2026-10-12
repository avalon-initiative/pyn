//! Moving the index and the work tree from one tree to another, as `git checkout`, `switch` and fast-forward
//! merges do (git's two-way merge): local changes that the move would overwrite stop it before anything is touched.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use pyn_git::{Index, IndexEntry};
use pyn_gitwork::{Entry, Kind, Pathspec, fs_path};
use pyn_object::{FileMode, ObjectId};

use super::stage::{
    Entries, Ignores, OpenIndex, entries_of, index_of, put_entry, raw_mode, remove_entry,
    stat_fields,
};
use super::{GitHome, convert, path_string};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Local changes are carried along, and a move that would lose any stops.
    Safe,
    /// The work tree and index become the target, local changes included.
    Force,
}

/// What stopped a move; nothing has been changed.
#[derive(Default)]
pub(super) struct Blocked {
    pub unmerged: Vec<Vec<u8>>,
    pub local: Vec<Vec<u8>>,
    pub untracked: Vec<Vec<u8>>,
}

impl Blocked {
    fn is_empty(&self) -> bool {
        self.unmerged.is_empty() && self.local.is_empty() && self.untracked.is_empty()
    }

    /// Prints git's messages for `verb` (`checkout`, `merge`...) and ends the command with status 1.
    pub(super) fn report(&self, verb: &str, advice: &str) -> ! {
        let list = |paths: &[Vec<u8>]| {
            for p in paths {
                eprintln!("\t{}", path_string(p));
            }
        };
        if !self.unmerged.is_empty() {
            eprintln!("error: you need to resolve your current index first");
            for p in &self.unmerged {
                eprintln!("{}: needs merge", path_string(p));
            }
        }
        if !self.local.is_empty() {
            eprintln!(
                "error: Your local changes to the following files would be overwritten by {verb}:"
            );
            list(&self.local);
            eprintln!("Please commit your changes or stash them before you {advice}.");
        }
        if !self.untracked.is_empty() {
            eprintln!(
                "error: The following untracked working tree files would be overwritten by {verb}:"
            );
            list(&self.untracked);
            eprintln!("Please move or remove them before you {advice}.");
        }
        eprintln!("Aborting");
        std::process::exit(1);
    }
}

enum Act {
    Remove,
    Write(Entry),
}

pub(super) type Moved = std::result::Result<Index, Blocked>;

fn lexists(path: &Path) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(path).ok()
}

/// Every file and link below `dir` (relative paths from the root), without following links.
fn files_under(root: &Path, dir: &[u8], out: &mut Vec<Vec<u8>>) -> Result<()> {
    for entry in std::fs::read_dir(fs_path(root, dir))? {
        let entry = entry?;
        #[cfg(unix)]
        let name = {
            use std::os::unix::ffi::OsStrExt;
            entry.file_name().as_bytes().to_vec()
        };
        #[cfg(not(unix))]
        let name = entry
            .file_name()
            .to_string_lossy()
            .into_owned()
            .into_bytes();
        let mut path = dir.to_vec();
        path.push(b'/');
        path.extend_from_slice(&name);
        if entry.file_type()?.is_dir() {
            files_under(root, &path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn ancestors(path: &[u8]) -> Vec<&[u8]> {
    path.iter()
        .enumerate()
        .filter(|(_, b)| **b == b'/')
        .map(|(i, _)| &path[..i])
        .collect()
}

/// Why writing `path` would lose something that is not tracked, if it would.
fn in_the_way(
    git: &GitHome,
    ignores: &Ignores,
    path: &[u8],
    removed: &HashSet<&[u8]>,
) -> Result<bool> {
    let root = git.repo.root()?;
    for a in ancestors(path) {
        match lexists(&fs_path(root, a)) {
            Some(m) if !m.is_dir() && !removed.contains(a) => return Ok(true),
            _ => {}
        }
    }
    let Some(meta) = lexists(&fs_path(root, path)) else {
        return Ok(false);
    };
    if meta.is_dir() {
        let mut below = Vec::new();
        files_under(root, path, &mut below)?;
        return Ok(below.iter().any(|p| !removed.contains(p.as_slice())));
    }
    Ok(!ignores.hides(git, path, false)?)
}

/// Computes what moving from `old` to `new` does, and refuses before any change when it would lose local work.
fn plan(
    git: &GitHome,
    opened: &OpenIndex,
    old: &BTreeMap<Vec<u8>, Entry>,
    new: &BTreeMap<Vec<u8>, Entry>,
    mode: Mode,
) -> Result<std::result::Result<BTreeMap<Vec<u8>, Act>, Blocked>> {
    let repo = &git.repo;
    let mut blocked = Blocked::default();
    let entries = entries_of(&opened.index);
    let unmerged: BTreeSet<&Vec<u8>> = entries
        .keys()
        .filter(|(_, s)| *s > 0)
        .map(|(p, _)| p)
        .collect();
    if mode == Mode::Safe {
        blocked.unmerged = unmerged.iter().map(|p| (*p).clone()).collect();
        if !blocked.unmerged.is_empty() {
            return Ok(Err(blocked));
        }
    }
    let leaves = repo.index_leaves(&opened.index);
    let dirty = repo.unstaged_changes(&opened.index)?;
    let clean = |p: &Vec<u8>| !matches!(dirty.get(p), Some(Kind::Modified | Kind::TypeChange));
    let mut acts: BTreeMap<Vec<u8>, Act> = BTreeMap::new();
    let mut absent_checks: Vec<Vec<u8>> = Vec::new();
    let paths: BTreeSet<&Vec<u8>> = old.keys().chain(new.keys()).chain(leaves.keys()).collect();
    for path in paths {
        let (h, m, i) = (old.get(path), new.get(path), leaves.get(path));
        if mode == Mode::Force {
            match m {
                Some(m) => {
                    if i != Some(m)
                        || !clean(path)
                        || dirty.contains_key(path)
                        || unmerged.contains(path)
                    {
                        acts.insert(path.clone(), Act::Write(*m));
                    }
                }
                None if i.is_some() || unmerged.contains(path) => {
                    acts.insert(path.clone(), Act::Remove);
                }
                None => {}
            }
            continue;
        }
        match (i, h, m) {
            (Some(_), None, None) | (None, _, None) => {}
            (Some(i), None, Some(m)) => {
                if i != m {
                    blocked.local.push(path.clone());
                }
            }
            (Some(i), Some(h), Some(m)) => {
                if h == m || i == m {
                    continue;
                }
                if i == h {
                    if clean(path) {
                        acts.insert(path.clone(), Act::Write(*m));
                    } else {
                        blocked.local.push(path.clone());
                    }
                } else {
                    blocked.local.push(path.clone());
                }
            }
            (Some(i), Some(h), None) => {
                if i != h || !clean(path) {
                    blocked.local.push(path.clone());
                } else {
                    acts.insert(path.clone(), Act::Remove);
                }
            }
            (None, Some(h), Some(m)) => {
                if h != m {
                    blocked.local.push(path.clone());
                }
            }
            (None, None, Some(m)) => {
                acts.insert(path.clone(), Act::Write(*m));
                absent_checks.push(path.clone());
            }
        }
    }
    let removed: HashSet<&[u8]> = acts
        .iter()
        .filter(|(_, a)| matches!(a, Act::Remove))
        .map(|(p, _)| p.as_slice())
        .collect();
    let ignores = Ignores {
        ignore: repo.ignore()?,
    };
    for path in &absent_checks {
        if in_the_way(git, &ignores, path, &removed)? {
            blocked.untracked.push(path.clone());
        }
    }
    for (path, act) in &acts {
        if let Act::Write(_) = act
            && !absent_checks.contains(path)
        {
            let root = repo.root()?;
            if lexists(&fs_path(root, path)).is_some_and(|m| m.is_dir()) {
                let mut below = Vec::new();
                files_under(root, path, &mut below)?;
                if below.iter().any(|p| !removed.contains(p.as_slice())) {
                    blocked.untracked.push(path.clone());
                }
            }
        }
    }
    if blocked.is_empty() {
        Ok(Ok(acts))
    } else {
        Ok(Err(blocked))
    }
}

#[cfg(unix)]
fn create_file(path: &Path, bytes: &[u8], executable: bool) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if executable { 0o777 } else { 0o666 })
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(not(unix))]
fn create_file(path: &Path, bytes: &[u8], _executable: bool) -> std::io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(unix)]
fn create_link(target: &[u8], path: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), path)
}

#[cfg(not(unix))]
fn create_link(target: &[u8], path: &Path) -> std::io::Result<()> {
    std::fs::write(path, target)
}

/// Takes `path` out of the way: a file or link is removed, an empty folder too.
fn clear(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir(path),
        Ok(_) => std::fs::remove_file(path),
        Err(_) => Ok(()),
    }
}

fn prune_empty_parents(root: &Path, path: &[u8]) {
    for a in ancestors(path).into_iter().rev() {
        if std::fs::remove_dir(fs_path(root, a)).is_err() {
            break;
        }
    }
}

/// Writes one tree entry into the work tree and returns its index entry with the file's stat data.
pub(super) fn write_entry(git: &GitHome, path: &[u8], entry: &Entry) -> Result<IndexEntry> {
    let root = git.repo.root()?;
    let file = fs_path(root, path);
    for a in ancestors(path) {
        let dir = fs_path(root, a);
        match lexists(&dir) {
            Some(m) if m.is_dir() => {}
            Some(_) => {
                std::fs::remove_file(&dir)
                    .with_context(|| format!("replacing {}", dir.display()))?;
                std::fs::create_dir(&dir)?;
            }
            None => {
                std::fs::create_dir(&dir).with_context(|| format!("creating {}", dir.display()))?
            }
        }
    }
    clear(&file).with_context(|| format!("replacing {}", file.display()))?;
    match entry.mode {
        FileMode::Gitlink => std::fs::create_dir_all(&file)?,
        FileMode::Symlink => {
            let target = git.repo.blob(&entry.id)?;
            if git.repo.bool_setting("core.symlinks", true) {
                create_link(&target, &file)
            } else {
                create_file(&file, &target, false)
            }
            .with_context(|| format!("writing {}", file.display()))?;
        }
        mode => {
            let bytes = git.repo.blob(&entry.id)?;
            create_file(&file, &bytes, mode == FileMode::Executable)
                .with_context(|| format!("writing {}", file.display()))?;
        }
    }
    let mut written = IndexEntry::new(path, raw_mode(entry.mode), entry.id);
    if entry.mode != FileMode::Gitlink {
        let meta = std::fs::symlink_metadata(&file)?;
        stat_fields(&mut written, &meta);
    }
    Ok(written)
}

/// Removes a tracked path from the work tree and any folder it leaves empty.
pub(super) fn remove_entry_file(git: &GitHome, path: &[u8]) -> Result<()> {
    let root = git.repo.root()?;
    let file = fs_path(root, path);
    match lexists(&file) {
        Some(m) if m.is_dir() => {
            let _ = std::fs::remove_dir(&file);
        }
        Some(_) => {
            std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?
        }
        None => {}
    }
    prune_empty_parents(root, path);
    Ok(())
}

/// Moves the index and work tree from the tree `old` to the tree `new` and writes the index. Returns what
/// stopped the move, with nothing changed, or the new index.
pub(super) fn move_tree(
    git: &GitHome,
    opened: &OpenIndex,
    old: Option<&ObjectId>,
    new: Option<&ObjectId>,
    mode: Mode,
) -> Result<Moved> {
    if opened.index.entries.iter().any(|e| e.skip_worktree) {
        bail!("this repository uses sparse checkout, which pyn does not support yet");
    }
    let spec = Pathspec::default();
    let old = git.repo.flatten(old, &spec)?;
    let new = git.repo.flatten(new, &spec)?;
    let acts = match plan(git, opened, &old, &new, mode)? {
        Ok(acts) => acts,
        Err(blocked) => return Ok(Err(blocked)),
    };
    convert::refuse_conversions(
        git,
        acts.iter()
            .filter(|(_, a)| matches!(a, Act::Write(_)))
            .map(|(p, _)| p.as_slice()),
    )?;
    let mut entries: Entries = entries_of(&opened.index);
    if mode == Mode::Force {
        entries.retain(|(p, _), _| new.contains_key(p));
    }
    for (path, act) in &acts {
        if let Act::Remove = act {
            remove_entry_file(git, path)?;
            remove_entry(&mut entries, path);
        }
    }
    for (path, act) in &acts {
        if let Act::Write(entry) = act {
            let written = write_entry(git, path, entry)?;
            put_entry(&mut entries, written);
        }
    }
    let index = index_of(opened.index.version, entries);
    git.save_index(opened, &index)?;
    Ok(Ok(index))
}

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
    /// Folders that hold untracked files a write would lose.
    pub directories: Vec<Vec<u8>>,
}

impl Blocked {
    fn is_empty(&self) -> bool {
        self.unmerged.is_empty()
            && self.local.is_empty()
            && self.untracked.is_empty()
            && self.directories.is_empty()
    }

    /// Prints git's messages for `verb` (`checkout`, `merge`...) and ends the command with `status`.
    pub(super) fn report(&self, verb: &str, advice: &str, status: i32) -> ! {
        self.print(verb, advice);
        eprintln!("Aborting");
        std::process::exit(status);
    }

    /// Records an untracked file in the way, or a folder holding untracked files, once.
    pub(super) fn add_way(&mut self, root: &Path, way: Vec<u8>) {
        let dir = lexists(&fs_path(root, &way)).is_some_and(|m| m.is_dir());
        let list = if dir {
            &mut self.directories
        } else {
            &mut self.untracked
        };
        if !list.contains(&way) {
            list.push(way);
        }
    }

    pub(super) fn print(&self, verb: &str, advice: &str) {
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
        if !self.directories.is_empty() {
            eprintln!(
                "error: Updating the following directories would lose untracked files in them:"
            );
            list(&self.directories);
            eprintln!();
        }
        if !self.untracked.is_empty() {
            eprintln!(
                "error: The following untracked working tree files would be overwritten by {verb}:"
            );
            list(&self.untracked);
            eprintln!("Please move or remove them before you {advice}.");
        }
    }
}

enum Act {
    Remove,
    Write(Entry),
}

pub(super) type Moved = std::result::Result<Index, Blocked>;

/// Refuses the whole set when any path is not safe to write, remove or rename in the work tree.
pub(super) fn check_writes<P: AsRef<[u8]>>(paths: impl IntoIterator<Item = P>) -> Result<()> {
    paths
        .into_iter()
        .try_for_each(|p| pyn_gitwork::check_path(p.as_ref()))
        .map_err(|e| Unsafe(e).into())
}

/// A path the work tree must not receive; nothing has been written.
#[derive(Debug)]
pub(super) struct Unsafe(pub pyn_gitwork::UnsafePath);

impl std::fmt::Display for Unsafe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}; nothing was changed", self.0)
    }
}

impl std::error::Error for Unsafe {}

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

/// The untracked file or folder that writing `path` would lose, as git names it: a file in the way is reported, not the
/// path below it.
pub(super) fn blocking_path(
    git: &GitHome,
    ignores: &Ignores,
    path: &[u8],
    removed: &HashSet<&[u8]>,
) -> Result<Option<Vec<u8>>> {
    let root = git.repo.root()?;
    for a in ancestors(path) {
        match lexists(&fs_path(root, a)) {
            Some(m) if !m.is_dir() && !removed.contains(a) => return Ok(Some(a.to_vec())),
            _ => {}
        }
    }
    let Some(meta) = lexists(&fs_path(root, path)) else {
        return Ok(None);
    };
    if meta.is_dir() {
        let mut below = Vec::new();
        files_under(root, path, &mut below)?;
        let lost = below.iter().any(|p| !removed.contains(p.as_slice()));
        return Ok(lost.then(|| path.to_vec()));
    }
    Ok((!ignores.hides(git, path, false)?).then(|| path.to_vec()))
}

pub(super) fn in_the_way(
    git: &GitHome,
    ignores: &Ignores,
    path: &[u8],
    removed: &HashSet<&[u8]>,
) -> Result<bool> {
    Ok(blocking_path(git, ignores, path, removed)?.is_some())
}

/// Whether `inner` is `outer` or below it.
pub(super) fn within(outer: &[u8], inner: &[u8]) -> bool {
    inner == outer || (inner.starts_with(outer) && inner.get(outer.len()) == Some(&b'/'))
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
        if let Some(way) = blocking_path(git, &ignores, path, &removed)? {
            blocked.add_way(repo.root()?, way);
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
                    blocked.add_way(root, path.clone());
                }
            }
        }
    }
    let local = blocked.local.clone();
    for list in [&mut blocked.untracked, &mut blocked.directories] {
        list.sort();
        list.retain(|u| !local.iter().any(|l| within(l, u) || within(u, l)));
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

fn prune_empty_parents(root: &Path, path: &[u8], keep: Option<&Path>) {
    for a in ancestors(path).into_iter().rev() {
        let dir = fs_path(root, a);
        if keep == Some(dir.as_path()) || std::fs::remove_dir(dir).is_err() {
            break;
        }
    }
}

/// Drops the cached attributes when `path` is an attributes file, which a write or removal changes.
fn touches_attributes(git: &GitHome, path: &[u8]) {
    if path.ends_with(b".gitattributes") {
        git.repo.reset_attributes();
    }
}

/// Orders paths so that `.gitattributes` files come first, since the files after them are converted by their rules.
pub(super) fn attributes_first<'a, T>(
    items: impl Iterator<Item = (&'a Vec<u8>, T)>,
) -> Vec<(&'a Vec<u8>, T)> {
    let mut items: Vec<_> = items.collect();
    items.sort_by_key(|(p, _)| !p.ends_with(b".gitattributes"));
    items
}

/// Writes `bytes` as the file, link or (for a submodule) folder at `path`, replacing what is there; regular files
/// are converted to their work tree form.
pub(super) fn write_bytes(
    git: &GitHome,
    path: &[u8],
    mode: FileMode,
    bytes: &[u8],
) -> Result<std::fs::Metadata> {
    let meta = write_converted(git, path, mode, bytes)?;
    // An attributes file is converted by its own rules, which only exist once it is written.
    if path.ends_with(b".gitattributes") && matches!(mode, FileMode::Regular | FileMode::Executable)
    {
        let algorithm = git.repo.algorithm();
        let before = std::fs::read(fs_path(git.repo.root()?, path))?;
        let after = git.repo.conversion(path)?.to_worktree(algorithm, bytes);
        if before != *after {
            return write_converted(git, path, mode, bytes);
        }
    }
    Ok(meta)
}

fn write_converted(
    git: &GitHome,
    path: &[u8],
    mode: FileMode,
    bytes: &[u8],
) -> Result<std::fs::Metadata> {
    check_writes([path])?;
    let root = git.repo.root()?;
    let file = fs_path(root, path);
    let converted;
    let bytes = if matches!(mode, FileMode::Regular | FileMode::Executable) {
        converted = git
            .repo
            .conversion(path)?
            .to_worktree(git.repo.algorithm(), bytes);
        &*converted
    } else {
        bytes
    };
    touches_attributes(git, path);
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
    match mode {
        FileMode::Gitlink => std::fs::create_dir_all(&file)?,
        FileMode::Symlink if git.repo.bool_setting("core.symlinks", true) => {
            create_link(bytes, &file).with_context(|| format!("writing {}", file.display()))?
        }
        _ => create_file(&file, bytes, mode == FileMode::Executable)
            .with_context(|| format!("writing {}", file.display()))?,
    }
    Ok(std::fs::symlink_metadata(&file)?)
}

/// Writes one tree entry into the work tree and returns its index entry with the file's stat data.
pub(super) fn write_entry(git: &GitHome, path: &[u8], entry: &Entry) -> Result<IndexEntry> {
    let bytes = match entry.mode {
        FileMode::Gitlink => Vec::new(),
        _ => git.repo.blob(&entry.id)?,
    };
    let meta = write_bytes(git, path, entry.mode, &bytes)?;
    let mut written = IndexEntry::new(path, raw_mode(entry.mode), entry.id);
    if entry.mode != FileMode::Gitlink {
        stat_fields(&mut written, &meta);
    }
    Ok(written)
}

/// Removes a tracked path from the work tree and any folder it leaves empty.
pub(super) fn remove_entry_file(git: &GitHome, path: &[u8]) -> Result<()> {
    remove_entry_file_keeping(git, path, None)
}

/// Like [`remove_entry_file`], but leaves the folder `keep` (the current one) in place even when it ends up empty.
pub(super) fn remove_entry_file_keeping(
    git: &GitHome,
    path: &[u8],
    keep: Option<&Path>,
) -> Result<()> {
    check_writes([path])?;
    touches_attributes(git, path);
    let root = git.repo.root()?;
    let file = fs_path(root, path);
    if ancestors(path)
        .into_iter()
        .any(|a| lexists(&fs_path(root, a)).is_some_and(|m| !m.is_dir()))
    {
        return Ok(());
    }
    match lexists(&file) {
        Some(m) if m.is_dir() => {
            let _ = std::fs::remove_dir(&file);
        }
        Some(_) => {
            std::fs::remove_file(&file).with_context(|| format!("removing {}", file.display()))?
        }
        None => {}
    }
    prune_empty_parents(root, path, keep);
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
    check_writes(new.keys())?;
    let acts = match plan(git, opened, &old, &new, mode)? {
        Ok(acts) => acts,
        Err(blocked) => return Ok(Err(blocked)),
    };
    check_writes(acts.keys())?;
    convert::refuse_unsupported(
        git,
        acts.iter().filter_map(|(p, a)| match a {
            Act::Write(e) => Some((p.as_slice(), e.mode)),
            Act::Remove => None,
        }),
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
    for (path, act) in attributes_first(acts.iter()) {
        if let Act::Write(entry) = act {
            let written = write_entry(git, path, entry)?;
            put_entry(&mut entries, written);
        }
    }
    let index = index_of(opened.index.version, entries);
    git.save_index(opened, &index)?;
    Ok(Ok(index))
}

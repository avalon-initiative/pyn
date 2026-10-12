//! `git status` semantics: HEAD against the index (staged), the index against the work tree (unstaged) and the
//! files git does not know about (untracked), with ignore rules applied.

use std::collections::{BTreeMap, HashSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};

use pyn_git::{Index, IndexEntry};
use pyn_object::{Algorithm, FileMode, Kind as ObjectKind, ObjectHasher, ObjectId};

use crate::error::{Error, Result, io_at};
use crate::ignore::{Ignore, Level, parse_rules};
use crate::pathspec::Pathspec;
use crate::repo::{Head, Repo};
use crate::tree::Entry;

const GITLINK: u32 = 0o160000;
const SYMLINK: u32 = 0o120000;
const EXECUTABLE: u32 = 0o100755;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
    TypeChange,
    Untracked,
    Unmerged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: Vec<u8>,
    /// HEAD against the index.
    pub staged: Option<Kind>,
    /// The index against the work tree, or `Untracked` or `Unmerged`.
    pub unstaged: Option<Kind>,
}

#[derive(Debug)]
pub struct Status {
    pub head: Head,
    pub entries: Vec<StatusEntry>,
}

/// The path as the file system sees it.
pub fn fs_path(root: &Path, path: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        root.join(std::ffi::OsStr::from_bytes(path))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(path).as_ref())
    }
}

fn is_missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

#[cfg(unix)]
fn is_executable(meta: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &Metadata) -> bool {
    false
}

/// The git file mode of a work tree file as it stands, symlinks not followed; `None` for a folder or special file.
pub fn worktree_mode(meta: &Metadata) -> Option<FileMode> {
    if meta.file_type().is_symlink() {
        Some(FileMode::Symlink)
    } else if meta.is_file() {
        Some(if is_executable(meta) {
            FileMode::Executable
        } else {
            FileMode::Regular
        })
    } else {
        None
    }
}

/// The blob id of what git would store for the file at `path` (a symlink's target), without filters applied.
pub fn hash_worktree_file(algorithm: Algorithm, path: &Path) -> Result<(FileMode, ObjectId)> {
    let meta = std::fs::symlink_metadata(path).map_err(io_at(path))?;
    let mode = worktree_mode(&meta)
        .ok_or_else(|| Error::Invalid(format!("{} is not a file", path.display())))?;
    if mode == FileMode::Symlink {
        let target = read_link_bytes(path)?;
        return Ok((
            mode,
            pyn_object::hash_object(algorithm, ObjectKind::Blob, &target)?,
        ));
    }
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(io_at(path))?;
    let mut hasher = ObjectHasher::blob(algorithm, meta.len());
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(io_at(path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n])?;
    }
    Ok((mode, hasher.finish()?))
}

fn read_link_bytes(path: &Path) -> Result<Vec<u8>> {
    let target = std::fs::read_link(path).map_err(io_at(path))?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(target.into_os_string().into_vec())
    }
    #[cfg(not(unix))]
    {
        Ok(target.to_string_lossy().into_owned().into_bytes())
    }
}

#[cfg(unix)]
fn stat_matches(meta: &Metadata, e: &IndexEntry) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.len() as u32 == e.size
        && meta.mtime() as u32 == e.mtime.seconds
        && meta.mtime_nsec() as u32 == e.mtime.nanos
}

#[cfg(not(unix))]
fn stat_matches(_: &Metadata, _: &IndexEntry) -> bool {
    false
}

fn row(rows: &mut BTreeMap<Vec<u8>, StatusEntry>, path: Vec<u8>) -> &mut StatusEntry {
    rows.entry(path.clone()).or_insert_with(|| StatusEntry {
        path,
        staged: None,
        unstaged: None,
    })
}

fn leaf_mode(raw: u32) -> FileMode {
    match raw {
        GITLINK => FileMode::Gitlink,
        SYMLINK => FileMode::Symlink,
        m if m & 0o111 != 0 => FileMode::Executable,
        _ => FileMode::Regular,
    }
}

fn is_link(mode: FileMode) -> bool {
    mode == FileMode::Symlink
}

impl Repo {
    pub fn status(&self) -> Result<Status> {
        let head = self.head()?;
        let index = self.read_index()?;
        let mut rows: BTreeMap<Vec<u8>, StatusEntry> = BTreeMap::new();
        for (path, kind) in self.staged_changes(&head, &index)? {
            row(&mut rows, path).staged = Some(kind);
        }
        for (path, kind) in self.unstaged_changes(&index)? {
            row(&mut rows, path).unstaged = Some(kind);
        }
        for path in self.untracked(&index)? {
            row(&mut rows, path).unstaged = Some(Kind::Untracked);
        }
        Ok(Status {
            head,
            entries: rows.into_values().collect(),
        })
    }

    /// The index, empty when there is none yet.
    pub fn read_index(&self) -> Result<Index> {
        Ok(self.git().read_index()?.unwrap_or_else(|| Index::new(2)))
    }

    /// The index's resolved entries (stage 0) as tree leaves, without intent-to-add placeholders.
    pub fn index_leaves(&self, index: &Index) -> BTreeMap<Vec<u8>, Entry> {
        index
            .entries
            .iter()
            .filter(|e| e.stage == 0 && !e.intent_to_add)
            .map(|e| {
                (
                    e.path.clone(),
                    Entry {
                        mode: leaf_mode(e.mode),
                        id: e.id,
                    },
                )
            })
            .collect()
    }

    /// Paths whose index entry differs from HEAD's tree.
    pub fn staged_changes(&self, head: &Head, index: &Index) -> Result<BTreeMap<Vec<u8>, Kind>> {
        let tree = match head.id() {
            Some(id) => Some(self.commit(&id)?.tree),
            None => None,
        };
        let committed = self.flatten(tree.as_ref(), &Pathspec::default())?;
        let staged = self.index_leaves(index);
        let unmerged: HashSet<&[u8]> = index
            .entries
            .iter()
            .filter(|e| e.stage > 0)
            .map(|e| e.path.as_slice())
            .collect();
        let mut out = BTreeMap::new();
        for path in committed.keys().chain(staged.keys()) {
            if unmerged.contains(path.as_slice()) {
                continue;
            }
            let kind = match (committed.get(path), staged.get(path)) {
                (None, Some(_)) => Kind::Added,
                (Some(_), None) => Kind::Deleted,
                (Some(a), Some(b)) if a == b => continue,
                (Some(a), Some(b)) if is_link(a.mode) != is_link(b.mode) => Kind::TypeChange,
                (Some(_), Some(_)) => Kind::Modified,
                (None, None) => continue,
            };
            out.insert(path.clone(), kind);
        }
        Ok(out)
    }

    /// Paths whose work tree file differs from the index, plus unmerged paths.
    pub fn unstaged_changes(&self, index: &Index) -> Result<BTreeMap<Vec<u8>, Kind>> {
        let root = self.root()?;
        let file_mode = self.bool_setting("core.filemode", true);
        let index_time = std::fs::metadata(self.git().index_path())
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let mut out = BTreeMap::new();
        for e in &index.entries {
            if e.stage > 0 {
                out.insert(e.path.clone(), Kind::Unmerged);
            } else if let Some(kind) = self.check_entry(root, e, file_mode, index_time)? {
                out.insert(e.path.clone(), kind);
            }
        }
        Ok(out)
    }

    fn check_entry(
        &self,
        root: &Path,
        e: &IndexEntry,
        file_mode: bool,
        index_time: Option<u64>,
    ) -> Result<Option<Kind>> {
        if e.assume_valid || e.skip_worktree {
            return Ok(None);
        }
        let path = fs_path(root, &e.path);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(err) if is_missing(&err) => return Ok(Some(Kind::Deleted)),
            Err(err) => return Err(io_at(&path)(err)),
        };
        if e.mode == GITLINK {
            return Ok(if meta.is_dir() {
                None
            } else {
                Some(Kind::TypeChange)
            });
        }
        let Some(mode) = worktree_mode(&meta) else {
            return Ok(Some(Kind::Deleted));
        };
        if is_link(mode) != (e.mode == SYMLINK) {
            return Ok(Some(Kind::TypeChange));
        }
        if e.intent_to_add {
            return Ok(Some(Kind::Added));
        }
        let mode_differs =
            file_mode && !is_link(mode) && (mode == FileMode::Executable) != (e.mode == EXECUTABLE);
        let racy = index_time.is_none_or(|t| u64::from(e.mtime.seconds) >= t);
        if !mode_differs && !racy && stat_matches(&meta, e) {
            return Ok(None);
        }
        let (_, id) = hash_worktree_file(self.algorithm(), &path)?;
        Ok((id != e.id || mode_differs).then_some(Kind::Modified))
    }

    /// Files and folders in the work tree that the index does not track and no ignore rule hides. A folder with
    /// nothing tracked in it is reported once, as `dir/`.
    pub fn untracked(&self, index: &Index) -> Result<Vec<Vec<u8>>> {
        let root = self.root()?;
        let ignore = self.ignore()?;
        let mut files: HashSet<&[u8]> = HashSet::new();
        let mut dirs: HashSet<Vec<u8>> = HashSet::new();
        let mut gitlinks: HashSet<&[u8]> = HashSet::new();
        for e in &index.entries {
            files.insert(&e.path);
            if e.mode == GITLINK {
                gitlinks.insert(&e.path);
            }
            let mut end = 0;
            while let Some(i) = e.path[end..].iter().position(|b| *b == b'/') {
                end += i;
                dirs.insert(e.path[..end].to_vec());
                end += 1;
            }
        }
        let walk = Walk {
            root,
            ignore: &ignore,
            files: &files,
            dirs: &dirs,
            gitlinks: &gitlinks,
        };
        let mut out = Vec::new();
        walk.folder(b"", &mut Vec::new(), &mut out)?;
        out.sort();
        Ok(out)
    }
}

struct Walk<'a> {
    root: &'a Path,
    ignore: &'a Ignore,
    files: &'a HashSet<&'a [u8]>,
    dirs: &'a HashSet<Vec<u8>>,
    gitlinks: &'a HashSet<&'a [u8]>,
}

impl Walk<'_> {
    fn level(&self, dir: &[u8]) -> Result<Option<Level>> {
        let file = fs_path(self.root, dir).join(".gitignore");
        match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_file() => {
                let bytes = std::fs::read(&file).map_err(io_at(&file))?;
                let mut base = dir.to_vec();
                if !base.is_empty() {
                    base.push(b'/');
                }
                Ok(Some(Level::new(&base, parse_rules(&bytes))))
            }
            _ => Ok(None),
        }
    }

    fn entries(&self, dir: &[u8]) -> Result<Vec<(Vec<u8>, std::fs::FileType)>> {
        let path = fs_path(self.root, dir);
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&path).map_err(io_at(&path))? {
            let entry = entry.map_err(io_at(&path))?;
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
            let kind = entry.file_type().map_err(io_at(&path))?;
            out.push((name, kind));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    fn child(dir: &[u8], name: &[u8]) -> Vec<u8> {
        let mut p = dir.to_vec();
        if !p.is_empty() {
            p.push(b'/');
        }
        p.extend_from_slice(name);
        p
    }

    /// Reports untracked paths under a folder that holds tracked files.
    fn folder(&self, dir: &[u8], levels: &mut Vec<Level>, out: &mut Vec<Vec<u8>>) -> Result<()> {
        let pushed = self.level(dir)?.map(|l| levels.push(l)).is_some();
        for (name, kind) in self.entries(dir)? {
            if name == b".git" {
                continue;
            }
            let path = Self::child(dir, &name);
            if kind.is_dir() {
                if self.gitlinks.contains(path.as_slice()) || self.ignored(levels, &path, true) {
                    continue;
                }
                if self.dirs.contains(&path) {
                    self.folder(&path, levels, out)?;
                } else if self.holds_untracked(&path, levels)? {
                    let mut shown = path;
                    shown.push(b'/');
                    out.push(shown);
                }
            } else if !self.files.contains(path.as_slice()) && !self.ignored(levels, &path, false) {
                out.push(path);
            }
        }
        if pushed {
            levels.pop();
        }
        Ok(())
    }

    /// Whether an untracked folder has anything in it that is not ignored; a nested repository counts.
    fn holds_untracked(&self, dir: &[u8], levels: &mut Vec<Level>) -> Result<bool> {
        let pushed = self.level(dir)?.map(|l| levels.push(l)).is_some();
        let mut found = false;
        for (name, kind) in self.entries(dir)? {
            let path = Self::child(dir, &name);
            if name == b".git" {
                found = true;
            } else if kind.is_dir() {
                found =
                    !self.ignored(levels, &path, true) && self.holds_untracked(&path, levels)?;
            } else {
                found = !self.ignored(levels, &path, false);
            }
            if found {
                break;
            }
        }
        if pushed {
            levels.pop();
        }
        Ok(found)
    }

    fn ignored(&self, levels: &[Level], path: &[u8], is_dir: bool) -> bool {
        self.ignore.is_ignored(levels, path, is_dir)
    }
}

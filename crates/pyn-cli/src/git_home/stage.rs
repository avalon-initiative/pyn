//! Staging on a git home: `add`, `restore --staged` and `commit` through `.git/index`, the object store and the
//! current branch's ref. The index is the only file `add` and `restore --staged` write.

use std::collections::{BTreeMap, HashSet};
use std::fs::Metadata;
use std::path::Path;

use anyhow::{Context, Result, bail};
use pyn_git::{Index, IndexEntry, Timestamp};
use pyn_gitwork::{
    Entry, Kind, Level, MagicSet, Pathspec, Tracker, fs_path, parse_rules, worktree_mode,
};
use pyn_object::{FileMode, Kind as ObjectKind, ObjectId, Signature, TreeEntry, encode_tree};

use super::{GitHome, Refused, path_string};

pub(super) const GITLINK: u32 = 0o160000;
const LOCAL_ONLY: &str = ".pyn/local_only";
/// States that a commit made by pyn would silently ignore, so it refuses them.
pub(super) const IN_PROGRESS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

pub struct AddOptions {
    pub paths: Vec<String>,
    pub update: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub force: bool,
    pub intent_to_add: bool,
}

pub(super) fn raw_mode(mode: FileMode) -> u32 {
    match mode {
        FileMode::Regular => 0o100644,
        FileMode::Executable => 0o100755,
        FileMode::Symlink => 0o120000,
        FileMode::Gitlink => GITLINK,
        FileMode::Tree => 0o040000,
    }
}

pub(super) fn leaf_mode(raw: u32) -> FileMode {
    match raw {
        GITLINK => FileMode::Gitlink,
        0o120000 => FileMode::Symlink,
        m if m & 0o111 != 0 => FileMode::Executable,
        _ => FileMode::Regular,
    }
}

/// The index as read, with its raw bytes so a write can tell whether anything else changed it meanwhile.
pub(super) struct OpenIndex {
    pub(super) raw: Option<Vec<u8>>,
    pub(super) index: Index,
}

pub(super) fn read_raw(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

impl GitHome {
    pub(super) fn open_index(&self) -> Result<OpenIndex> {
        let raw = read_raw(&self.repo.git().index_path())?;
        let index = match &raw {
            Some(bytes) => Index::parse(bytes, self.repo.algorithm())?,
            None => Index::new(2),
        };
        Ok(OpenIndex { raw, index })
    }

    pub(super) fn save_index(&self, opened: &OpenIndex, index: &Index) -> Result<()> {
        if read_raw(&self.repo.git().index_path())? != opened.raw {
            bail!("the index changed while pyn was working; run the command again");
        }
        Ok(self.repo.git().write_index(index)?)
    }
}

/// The pathspecs a command was given, parsed as git does.
pub(super) struct Specs {
    pub(super) spec: Pathspec,
}

pub(super) fn resolve_specs(git: &GitHome, cwd: &Path, args: &[String]) -> Result<Specs> {
    Ok(Specs {
        spec: git.pathspec(cwd, args, false)?,
    })
}

impl GitHome {
    /// Parses `args` as pathspecs for a command run in `cwd`; with `prefer_cwd`, none (or only excludes) means the
    /// working folder rather than the whole repository.
    pub(super) fn pathspec(
        &self,
        cwd: &Path,
        args: &[String],
        prefer_cwd: bool,
    ) -> Result<Pathspec> {
        self.pathspec_without(cwd, args, prefer_cwd, MagicSet::default())
    }

    /// Like [`Self::pathspec`], for a command that refuses the `unsupported` magic.
    pub(super) fn pathspec_without(
        &self,
        cwd: &Path,
        args: &[String],
        prefer_cwd: bool,
        unsupported: MagicSet,
    ) -> Result<Pathspec> {
        crate::pathspec::parse(
            &self.ws,
            cwd,
            args,
            (crate::pathspec::options_from_env()?, unsupported),
            prefer_cwd,
            super::plumbing::usage,
        )
    }
}

/// Whether git reads a loose argument as a pathspec without it naming a file: a wildcard or `:` magic.
pub(super) fn looks_like_pathspec(arg: &str) -> bool {
    let mut escaped = false;
    for b in arg.bytes() {
        match b {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'*' | b'?' | b'[' => return true,
            _ => {}
        }
    }
    let b = arg.as_bytes();
    b.first() == Some(&b':') && b.get(1).is_some_and(|c| b"(!/^".contains(c))
}

pub(super) fn under(path: &[u8], prefix: &[u8]) -> bool {
    prefix.is_empty()
        || path == prefix
        || (path.starts_with(prefix) && path.get(prefix.len()) == Some(&b'/'))
}

/// Fails, as `git add` does, for a pathspec that selects nothing known or in the work tree.
fn require_seen<'a>(
    git: &GitHome,
    spec: &Pathspec,
    seen: impl Iterator<Item = &'a [u8]> + Clone,
) -> Result<()> {
    for n in 0..spec.len() {
        let text = spec.item_path(n);
        if spec.item_is_exclude(n)
            || text.is_empty()
            || seen.clone().any(|p| spec.item_match(n, p).is_some())
        {
            continue;
        }
        if spec.item_is_pattern(n)
            || std::fs::symlink_metadata(fs_path(git.repo.root()?, text)).is_err()
        {
            bail!(
                "pathspec '{}' did not match any files",
                String::from_utf8_lossy(spec.original(n))
            );
        }
    }
    Ok(())
}

pub(super) struct Ignores {
    pub(super) ignore: pyn_gitwork::Ignore,
}

impl Ignores {
    pub(super) fn level(git: &GitHome, dir: &[u8]) -> Result<Option<Level>> {
        let file = fs_path(git.repo.root()?, dir).join(".gitignore");
        match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_file() => {
                let bytes =
                    std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
                let mut base = dir.to_vec();
                if !base.is_empty() {
                    base.push(b'/');
                }
                Ok(Some(Level::new(&base, parse_rules(&bytes))))
            }
            _ => Ok(None),
        }
    }

    /// Whether the path, or a folder above it, is hidden by ignore rules.
    pub(super) fn hides(&self, git: &GitHome, path: &[u8], is_dir: bool) -> Result<bool> {
        let mut levels = Vec::new();
        let mut dir_end = 0;
        let mut dir: &[u8] = b"";
        loop {
            levels.extend(Self::level(git, dir)?);
            let rest = &path[dir_end..];
            let Some(slash) = rest.iter().position(|b| *b == b'/') else {
                break;
            };
            dir_end += slash;
            let next = &path[..dir_end];
            if self.ignore.is_ignored(&levels, next, true) {
                return Ok(true);
            }
            dir = next;
            dir_end += 1;
        }
        Ok(self.ignore.is_ignored(&levels, path, is_dir))
    }
}

pub(super) fn child(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = dir.to_vec();
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

pub(super) fn dir_entries(path: &Path) -> Result<Vec<(Vec<u8>, std::fs::FileType)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
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
        out.push((name, entry.file_type()?));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// What a walk over the work tree found that the index does not track.
#[derive(Default)]
struct Found {
    files: Vec<Vec<u8>>,
    nested: Vec<Vec<u8>>,
}

struct Walk<'a> {
    git: &'a GitHome,
    ignores: &'a Ignores,
    spec: &'a Pathspec,
    picks: std::cell::RefCell<Tracker<'a>>,
    tracked: &'a HashSet<&'a [u8]>,
    dirs: &'a HashSet<Vec<u8>>,
    gitlinks: &'a HashSet<&'a [u8]>,
    force: bool,
}

impl Walk<'_> {
    /// Tracked folders are always opened; an untracked one only when the pathspecs let the walk in.
    fn enters(&self, dir: &[u8]) -> bool {
        if self.dirs.contains(dir) {
            self.spec.may_contain(dir)
        } else {
            self.spec.walks_into(dir)
        }
    }

    fn folder(&self, dir: &[u8], levels: &mut Vec<Level>, found: &mut Found) -> Result<()> {
        let pushed = Ignores::level(self.git, dir)?
            .map(|l| levels.push(l))
            .is_some();
        let root = self.git.repo.root()?;
        for (name, kind) in dir_entries(&fs_path(root, dir))? {
            if name == b".git" {
                continue;
            }
            let path = child(dir, &name);
            if kind.is_dir() {
                if path == LOCAL_ONLY.as_bytes()
                    || self.gitlinks.contains(path.as_slice())
                    || !self.enters(&path)
                    || (!self.force && self.ignores.ignore.is_ignored(levels, &path, true))
                {
                    continue;
                }
                if fs_path(root, &path).join(".git").exists() {
                    if self.spec.matches(&path) {
                        found.nested.push(path);
                    }
                    continue;
                }
                self.folder(&path, levels, found)?;
            } else if self.spec.matches(&path)
                && !self.tracked.contains(path.as_slice())
                && (self.force || !self.ignores.ignore.is_ignored(levels, &path, false))
                && self.picks.borrow_mut().picks(&path)
            {
                found.files.push(path);
            }
        }
        if pushed {
            levels.pop();
        }
        Ok(())
    }
}

enum Op {
    Put,
    Remove,
}

struct Plan {
    ops: BTreeMap<Vec<u8>, Op>,
    ignored: Vec<Vec<u8>>,
    nested: Vec<Vec<u8>>,
}

fn plan(git: &GitHome, index: &Index, specs: &Specs, update: bool, force: bool) -> Result<Plan> {
    let mut ops = BTreeMap::new();
    for (path, kind) in git.repo.unstaged_changes_for_staging(index)? {
        if !specs.spec.matches(&path) {
            continue;
        }
        match kind {
            Kind::Unmerged => {
                let present = std::fs::symlink_metadata(fs_path(git.repo.root()?, &path)).is_ok();
                ops.insert(path, if present { Op::Put } else { Op::Remove })
            }
            Kind::Deleted => ops.insert(path, Op::Remove),
            _ => ops.insert(path, Op::Put),
        };
    }
    let mut out = Plan {
        ops,
        ignored: Vec::new(),
        nested: Vec::new(),
    };
    if update {
        return Ok(out);
    }
    let tracked: HashSet<&[u8]> = index.entries.iter().map(|e| e.path.as_slice()).collect();
    let mut dirs: HashSet<Vec<u8>> = HashSet::new();
    for path in &tracked {
        let mut end = 0;
        while let Some(i) = path[end..].iter().position(|b| *b == b'/') {
            end += i;
            dirs.insert(path[..end].to_vec());
            end += 1;
        }
    }
    let gitlinks: HashSet<&[u8]> = index
        .entries
        .iter()
        .filter(|e| e.mode == GITLINK)
        .map(|e| e.path.as_slice())
        .collect();
    let ignores = Ignores {
        ignore: git.repo.ignore()?,
    };
    let root = git.repo.root()?;
    if !force {
        for n in 0..specs.spec.len() {
            let literal = specs.spec.item_path(n);
            if specs.spec.item_is_exclude(n) || specs.spec.item_literal(n).len() != literal.len() {
                continue;
            }
            let bytes = literal.strip_suffix(b"/").unwrap_or(literal);
            let Ok(meta) = std::fs::symlink_metadata(fs_path(root, bytes)) else {
                continue;
            };
            if !bytes.is_empty()
                && !tracked.iter().any(|p| under(p, bytes))
                && ignores.hides(git, bytes, meta.is_dir())?
                && !out.ignored.iter().any(|p| p == bytes)
            {
                out.ignored.push(bytes.to_vec());
            }
        }
    }
    let walk = Walk {
        git,
        ignores: &ignores,
        spec: &specs.spec,
        picks: std::cell::RefCell::new(specs.spec.tracker()),
        tracked: &tracked,
        dirs: &dirs,
        gitlinks: &gitlinks,
        force,
    };
    let mut found = Found::default();
    walk.folder(b"", &mut Vec::new(), &mut found)?;
    for path in found.files {
        out.ops.insert(path, Op::Put);
    }
    out.nested = found.nested;
    Ok(out)
}

#[cfg(unix)]
pub(super) fn link_target(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::fs::read_link(path)?.into_os_string().into_vec())
}

#[cfg(not(unix))]
pub(super) fn link_target(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read_link(path)?
        .to_string_lossy()
        .into_owned()
        .into_bytes())
}

#[cfg(unix)]
pub(super) fn stat_fields(entry: &mut IndexEntry, meta: &Metadata) {
    use std::os::unix::fs::MetadataExt;
    let time = |s: i64, n: i64| Timestamp {
        seconds: s as u32,
        nanos: n as u32,
    };
    entry.ctime = time(meta.ctime(), meta.ctime_nsec());
    entry.mtime = time(meta.mtime(), meta.mtime_nsec());
    entry.dev = meta.dev() as u32;
    entry.ino = meta.ino() as u32;
    entry.uid = meta.uid();
    entry.gid = meta.gid();
    entry.size = meta.len() as u32;
}

#[cfg(not(unix))]
pub(super) fn stat_fields(entry: &mut IndexEntry, meta: &Metadata) {
    entry.size = meta.len() as u32;
    let _ = Timestamp::default();
}

/// Writes the file's blob and returns the index entry for it; `existing` keeps the mode where `core.filemode` is off.
pub(super) fn stage_file(
    git: &GitHome,
    path: &[u8],
    existing: Option<&IndexEntry>,
) -> Result<IndexEntry> {
    let file = fs_path(git.repo.root()?, path);
    let meta =
        std::fs::symlink_metadata(&file).with_context(|| format!("reading {}", file.display()))?;
    if meta.is_dir() && existing.is_some_and(|e| e.mode == GITLINK) {
        let sub = pyn_gitwork::Repo::open(&file, None, None)?;
        let id = sub
            .head_commit()?
            .context("submodule does not have a commit checked out")?;
        let mut entry = IndexEntry::new(path, GITLINK, id);
        stat_fields(&mut entry, &meta);
        return Ok(entry);
    }
    let mut mode = worktree_mode(&meta).with_context(|| {
        format!(
            "'{}' is not a regular file or a symbolic link",
            path_string(path)
        )
    })?;
    let bytes = if mode == FileMode::Symlink {
        link_target(&file)?
    } else {
        let clean = git
            .repo
            .read_clean(path, &file, existing.map(|e| e.id), true)?;
        for warning in &clean.warnings {
            eprintln!("warning: {warning}");
        }
        clean.data.into_owned()
    };
    if mode != FileMode::Symlink && !git.repo.bool_setting("core.filemode", true) {
        mode = match existing.map(|e| leaf_mode(e.mode)) {
            Some(m @ (FileMode::Regular | FileMode::Executable)) => m,
            _ => FileMode::Regular,
        };
    }
    let id = git.repo.objects().write(ObjectKind::Blob, &bytes)?;
    let mut entry = IndexEntry::new(path, raw_mode(mode), id);
    stat_fields(&mut entry, &meta);
    Ok(entry)
}

pub(super) type Entries = BTreeMap<(Vec<u8>, u8), IndexEntry>;

pub(super) fn entries_of(index: &Index) -> Entries {
    index
        .entries
        .iter()
        .map(|e| ((e.path.clone(), e.stage), e.clone()))
        .collect()
}

pub(super) fn index_of(version: u32, entries: Entries) -> Index {
    Index {
        version,
        entries: entries.into_values().collect(),
    }
}

/// Puts `entry` in place of whatever file or folder the index had at its path or above it.
pub(super) fn put_entry(entries: &mut Entries, entry: IndexEntry) {
    let path = entry.path.clone();
    for (i, b) in path.iter().enumerate() {
        if *b == b'/' {
            for stage in 0..=3 {
                entries.remove(&(path[..i].to_vec(), stage));
            }
        }
    }
    let mut folder = path.clone();
    folder.push(b'/');
    let below: Vec<_> = entries
        .range((folder.clone(), 0)..)
        .take_while(|((p, _), _)| p.starts_with(&folder))
        .map(|(k, _)| k.clone())
        .collect();
    for key in below {
        entries.remove(&key);
    }
    for stage in 1..=3 {
        entries.remove(&(path.clone(), stage));
    }
    entries.insert((path, 0), entry);
}

pub(super) fn remove_entry(entries: &mut Entries, path: &[u8]) {
    for stage in 0..=3 {
        entries.remove(&(path.to_vec(), stage));
    }
}

/// Stages every change `plan` found into `entries`; prints each one with `verbose` or `dry_run`.
fn apply(
    git: &GitHome,
    entries: &mut Entries,
    ops: &BTreeMap<Vec<u8>, Op>,
    (dry_run, verbose, intent): (bool, bool, bool),
) -> Result<()> {
    for (path, op) in ops {
        let name = path_string(path);
        match op {
            Op::Remove => {
                if dry_run || verbose {
                    println!("remove '{name}'");
                }
                if !dry_run {
                    remove_entry(entries, path);
                }
            }
            Op::Put => {
                if dry_run || verbose {
                    println!("add '{name}'");
                }
                let tracked = (0..=3).any(|stage| entries.contains_key(&(path.clone(), stage)));
                if !dry_run && !(intent && tracked) {
                    let existing = entries.get(&(path.clone(), 0)).or_else(|| {
                        (1..=3)
                            .filter_map(|stage| entries.get(&(path.clone(), stage)))
                            .find(|entry| entry.mode == GITLINK)
                    });
                    let mut entry = stage_file(git, path, existing)?;
                    if intent {
                        entry.id = git.repo.objects().write(ObjectKind::Blob, b"")?;
                        entry.intent_to_add = true;
                    }
                    put_entry(entries, entry);
                }
            }
        }
    }
    Ok(())
}

/// Files in the work tree that the index does not track and no ignore rule hides.
pub(super) fn untracked_files(git: &GitHome, index: &Index) -> Result<Vec<Vec<u8>>> {
    let specs = Specs {
        spec: Pathspec::default(),
    };
    let tracked: HashSet<&[u8]> = index.entries.iter().map(|e| e.path.as_slice()).collect();
    let found = plan(git, index, &specs, false, false)?;
    Ok(found
        .ops
        .into_keys()
        .filter(|p| !tracked.contains(p.as_slice()))
        .collect())
}

/// `pyn add`: returns whether some named path was refused (the exit status is then 1, as in git).
pub fn add(git: &GitHome, cwd: &Path, options: &AddOptions) -> Result<bool> {
    let opened = git.open_index()?;
    let specs = Specs {
        spec: git.pathspec_without(
            cwd,
            &options.paths,
            false,
            MagicSet {
                attr: true,
                ..MagicSet::default()
            },
        )?,
    };
    let plan = plan(git, &opened.index, &specs, options.update, options.force)?;
    require_seen(
        git,
        &specs.spec,
        opened
            .index
            .entries
            .iter()
            .map(|e| e.path.as_slice())
            .chain(plan.ops.keys().map(Vec::as_slice))
            .chain(plan.nested.iter().map(Vec::as_slice)),
    )?;
    for path in &plan.nested {
        eprintln!(
            "warning: '{}/' is a nested git repository; pyn does not add submodules",
            path_string(path)
        );
    }
    let mut entries = entries_of(&opened.index);
    apply(
        git,
        &mut entries,
        &plan.ops,
        (options.dry_run, options.verbose, options.intent_to_add),
    )?;
    if !options.dry_run && !plan.ops.is_empty() {
        git.save_index(&opened, &index_of(opened.index.version, entries))?;
    }
    if plan.ignored.is_empty() {
        return Ok(false);
    }
    eprintln!("The following paths are ignored by one of your .gitignore files:");
    for path in &plan.ignored {
        eprintln!("{}", path_string(path));
    }
    eprintln!("hint: Use -f if you really want to add them.");
    Ok(true)
}

/// `pyn restore --staged`: puts the index entries under `paths` back to what `source` (default `HEAD`) has.
pub fn restore_staged(
    git: &GitHome,
    cwd: &Path,
    paths: &[String],
    source: Option<&str>,
) -> Result<()> {
    let opened = git.open_index()?;
    let specs = resolve_specs(git, cwd, paths)?;
    let commit = match source {
        Some(rev) => Some(git.repo.resolve(rev)?),
        None => git.repo.head_commit()?,
    };
    let tree = match commit {
        Some(id) => Some(git.repo.commit(&id)?.tree),
        None => None,
    };
    let committed = git.repo.flatten(tree.as_ref(), &specs.spec)?;
    let mut known: Vec<&[u8]> = opened
        .index
        .entries
        .iter()
        .map(|e| e.path.as_slice())
        .collect();
    known.extend(committed.keys().map(Vec::as_slice));
    if let Some(arg) = specs.spec.first_unmatched(known.iter().copied()) {
        return Err(Refused(format!(
            "pathspec '{}' did not match any file(s) known to git",
            String::from_utf8_lossy(arg)
        ))
        .into());
    }
    let mut entries = entries_of(&opened.index);
    let current: Vec<Vec<u8>> = entries
        .keys()
        .filter(|(p, _)| specs.spec.matches(p))
        .map(|(p, _)| p.clone())
        .collect();
    for path in current {
        remove_entry(&mut entries, &path);
    }
    for (path, entry) in committed {
        entries.insert(
            (path.clone(), 0),
            IndexEntry::new(path, raw_mode(entry.mode), entry.id),
        );
    }
    git.save_index(&opened, &index_of(opened.index.version, entries))
}

fn write_dir(
    git: &GitHome,
    items: &[(&[u8], FileMode, ObjectId)],
    depth: usize,
) -> Result<ObjectId> {
    let mut entries: Vec<TreeEntry> = Vec::new();
    let mut i = 0;
    while i < items.len() {
        let rest = &items[i].0[depth..];
        match rest.iter().position(|b| *b == b'/') {
            Some(slash) => {
                let name = &rest[..slash];
                let member = |p: &[u8]| {
                    p.get(depth..depth + slash) == Some(name) && p.get(depth + slash) == Some(&b'/')
                };
                let mut j = i;
                while j < items.len() && member(items[j].0) {
                    j += 1;
                }
                let id = write_dir(git, &items[i..j], depth + slash + 1)?;
                entries.push(TreeEntry {
                    mode: FileMode::Tree,
                    name: name.to_vec(),
                    id,
                });
                i = j;
            }
            None => {
                entries.push(TreeEntry {
                    mode: items[i].1,
                    name: rest.to_vec(),
                    id: items[i].2,
                });
                i += 1;
            }
        }
    }
    let body = encode_tree(git.algorithm(), &entries)?;
    Ok(git.repo.objects().write(ObjectKind::Tree, &body)?)
}

/// Writes the trees for the index's resolved entries and returns the root tree's id.
pub(super) fn write_tree(git: &GitHome, index: &Index) -> Result<ObjectId> {
    let mut items: Vec<(&[u8], FileMode, ObjectId)> = index
        .entries
        .iter()
        .filter(|e| e.stage == 0 && !e.intent_to_add)
        .map(|e| (e.path.as_slice(), leaf_mode(e.mode), e.id))
        .collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    write_dir(git, &items, 0)
}

/// Writes the trees for a map of paths to entries and returns the root tree.
pub(super) fn write_leaves(git: &GitHome, leaves: &BTreeMap<Vec<u8>, Entry>) -> Result<ObjectId> {
    let items: Vec<(&[u8], FileMode, ObjectId)> = leaves
        .iter()
        .map(|(p, e)| (p.as_slice(), e.mode, e.id))
        .collect();
    write_dir(git, &items, 0)
}

pub(super) fn local_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

/// A date as git reads it from `GIT_AUTHOR_DATE`: `<seconds> <+hhmm>`, with or without a leading `@`, or ISO 8601.
pub(super) fn parse_date(text: &str) -> Result<(i64, i32)> {
    let t = text.trim();
    let raw = t.strip_prefix('@').unwrap_or(t);
    let (secs, zone) = raw.split_once(' ').unwrap_or((raw, ""));
    if let Ok(seconds) = secs.parse::<i64>() {
        let zone = zone.trim();
        if zone.is_empty() {
            return Ok((seconds, local_offset_minutes()));
        }
        if zone.len() == 5
            && let (Some(sign), Ok(h), Ok(m)) = (
                match zone.as_bytes()[0] {
                    b'+' => Some(1),
                    b'-' => Some(-1),
                    _ => None,
                },
                zone[1..3].parse::<i32>(),
                zone[3..5].parse::<i32>(),
            )
        {
            return Ok((seconds, sign * (h * 60 + m)));
        }
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(t)
        .or_else(|_| chrono::DateTime::parse_from_rfc2822(t))
        .or_else(|_| chrono::DateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S %z"))
        .or_else(|_| chrono::DateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%z"));
    if let Ok(d) = parsed {
        return Ok((d.timestamp(), d.offset().local_minus_utc() / 60));
    }
    local_date(t).ok_or_else(|| anyhow::anyhow!("invalid date format: {text}"))
}

/// A date without a zone, in the local one; a bare day takes the current time of day, as git does.
fn local_date(text: &str) -> Option<(i64, i32)> {
    let offset = local_offset_minutes();
    let naive = ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"]
        .iter()
        .find_map(|f| chrono::NaiveDateTime::parse_from_str(text, f).ok())
        .or_else(|| {
            let day = ["%Y-%m-%d", "%Y/%m/%d"]
                .iter()
                .find_map(|f| chrono::NaiveDate::parse_from_str(text, f).ok())?;
            let now = chrono::Utc::now().naive_utc() + chrono::Duration::minutes(offset.into());
            Some(day.and_time(now.time()))
        })?;
    Some((naive.and_utc().timestamp() - i64::from(offset) * 60, offset))
}

pub(super) fn clean_ident(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '\n'))
        .collect::<String>()
        .trim()
        .to_string()
}

pub(super) fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The author or committer: `GIT_<ROLE>_*`, then `<role>.name` and `<role>.email`, then `user.*`.
pub(super) fn identity(git: &GitHome, role: &str) -> Result<Signature> {
    let upper = role.to_ascii_uppercase();
    let config = git.repo.config();
    let pick = |var: &str, key: &str| {
        env(&format!("GIT_{upper}_{var}"))
            .or_else(|| config.get(&format!("{role}.{key}")).map(str::to_string))
            .or_else(|| config.get(&format!("user.{key}")).map(str::to_string))
    };
    let name = pick("NAME", "name")
        .map(|n| clean_ident(&n))
        .unwrap_or_default();
    let email = pick("EMAIL", "email")
        .or_else(|| env("EMAIL"))
        .map(|e| clean_ident(&e))
        .unwrap_or_default();
    if name.is_empty() || email.is_empty() {
        bail!(
            "unable to determine who you are; set user.name and user.email in the git configuration"
        );
    }
    let (time, offset_minutes) = match env(&format!("GIT_{upper}_DATE")) {
        Some(d) => parse_date(&d)?,
        None => (chrono::Utc::now().timestamp(), local_offset_minutes()),
    };
    Ok(Signature {
        name: name.into_bytes(),
        email: email.into_bytes(),
        time,
        offset_minutes,
    })
}

/// Git's `whitespace` cleanup for a message given with `-m`: trailing blanks off every line, no leading or
/// trailing empty lines, runs of empty lines collapsed, and a final newline.
pub(crate) fn clean_message(messages: &[String]) -> Vec<u8> {
    clean_bytes(messages.join("\n\n").as_bytes())
}

/// The same cleanup for raw bytes (a message file or a commit's own message).
pub(super) fn clean_bytes(text: &[u8]) -> Vec<u8> {
    let mut lines: Vec<&[u8]> = Vec::new();
    for line in text.split(|b| *b == b'\n') {
        let end = line
            .iter()
            .rposition(|b| !b.is_ascii_whitespace() && *b != 0x0b)
            .map_or(0, |i| i + 1);
        let line = &line[..end];
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut out = lines.join(&b'\n');
    out.push(b'\n');
    out
}

/// Stages every tracked file that changed in the work tree into `entries`; true if there was any.
pub(super) fn stage_tracked(git: &GitHome, index: &Index, entries: &mut Entries) -> Result<bool> {
    let specs = Specs {
        spec: Pathspec::default(),
    };
    let plan = plan(git, index, &specs, true, false)?;
    apply(git, entries, &plan.ops, (false, false, false))?;
    Ok(!plan.ops.is_empty())
}

/// Stops, naming the state, while a merge, cherry-pick, revert or rebase is in progress.
pub(super) fn refuse_in_progress(git: &GitHome) -> Result<()> {
    refuse_states(git, &IN_PROGRESS)
}

pub(super) fn refuse_states(git: &GitHome, states: &[&str]) -> Result<()> {
    let dir = git.repo.git().git_dir();
    if let Some(name) = states.iter().find(|n| dir.join(n).exists()) {
        if *name == "MERGE_HEAD" {
            bail!(
                "a merge is in progress; resolve the conflicts and finish it with `pyn add` and `pyn commit`, \
                 or give it up with `pyn merge --abort`"
            );
        }
        if *name == "rebase-merge" {
            bail!(
                "a rebase is in progress ({name}); conclude it with `pyn rebase --continue`, \
                 `--skip` or `--abort`"
            );
        }
        bail!(
            "a merge, cherry-pick, revert or rebase is in progress ({name}); \
             pyn cannot continue it, so finish or abort it with git first"
        );
    }
    Ok(())
}

/// Warns once that git hooks exist and were not run.
pub(super) fn warn_about_hooks_for(git: &GitHome, names: &[&str]) {
    let hooks = git.hooks_dir();
    if names.iter().any(|h| hooks.join(h).is_file()) {
        eprintln!(
            "warning: pyn does not run git hooks; the ones in {} were skipped",
            hooks.display()
        );
    }
}

/// The commits a merge in progress is merging in (`MERGE_HEAD`), empty when there is none.
pub(super) fn merge_heads(git: &GitHome) -> Result<Vec<ObjectId>> {
    let path = git.repo.git().git_dir().join("MERGE_HEAD");
    let Some(bytes) = read_raw(&path)? else {
        return Ok(Vec::new());
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Ok(ObjectId::from_hex(git.repo.algorithm(), l.trim())?))
        .collect()
}

/// Removes the files that mark a merge in progress.
pub(super) fn clear_merge_state(git: &GitHome) {
    let dir = git.repo.git().git_dir();
    for name in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// `MERGE_MSG` with its comment lines dropped, as `git commit` uses it; `--no-edit` keeps them.
pub(super) fn merge_message(git: &GitHome, keep_comments: bool) -> Result<Option<Vec<u8>>> {
    let Some(bytes) = read_raw(&git.repo.git().git_dir().join("MERGE_MSG"))? else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(&bytes);
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| keep_comments || !l.starts_with('#'))
        .collect();
    Ok(Some(clean_message(&[kept.join("\n")])))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_get_git_whitespace_cleanup() {
        let clean = |m: &[&str]| {
            String::from_utf8(clean_message(
                &m.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            ))
            .unwrap()
        };
        assert_eq!(
            clean(&["  \n\nsubject   \n\n\n\nbody \n\n"]),
            "subject\n\nbody\n"
        );
        assert_eq!(clean(&["one", "two"]), "one\n\ntwo\n");
        assert_eq!(clean(&["indented\n    line"]), "indented\n    line\n");
    }

    #[test]
    fn dates_are_read_as_git_reads_them() {
        assert_eq!(
            parse_date("1700000000 +0200").unwrap(),
            (1_700_000_000, 120)
        );
        assert_eq!(
            parse_date("@1700000000 -0530").unwrap(),
            (1_700_000_000, -330)
        );
        assert_eq!(
            parse_date("2023-11-14T22:13:20+02:00").unwrap(),
            (1_699_992_800, 120)
        );
        assert!(parse_date("yesterday").is_err());
    }

    #[test]
    fn a_file_replaces_a_folder_and_a_folder_replaces_a_file_in_the_index() {
        let id = ObjectId::from_bytes(pyn_object::Algorithm::Sha1, &[1; 20]).unwrap();
        let entry = |p: &str| IndexEntry::new(p.as_bytes(), 0o100644, id);
        let mut entries = Entries::new();
        for p in ["a/b", "a/c", "z"] {
            entries.insert((p.as_bytes().to_vec(), 0), entry(p));
        }
        put_entry(&mut entries, entry("a"));
        let paths: Vec<&[u8]> = entries.keys().map(|(p, _)| p.as_slice()).collect();
        assert_eq!(paths, [b"a".as_slice(), b"z"]);
        put_entry(&mut entries, entry("z/inner"));
        let paths: Vec<&[u8]> = entries.keys().map(|(p, _)| p.as_slice()).collect();
        assert_eq!(paths, [b"a".as_slice(), b"z/inner"]);
    }

    #[test]
    fn names_are_under_a_folder_only_at_a_slash() {
        assert!(under(b"src/a", b"src") && under(b"src", b"src") && under(b"x", b""));
        assert!(!under(b"srcx/a", b"src"));
    }
}

//! Staging on a git home: `add`, `restore --staged` and `commit` through `.git/index`, the object store and the
//! current branch's ref. The index is the only file `add` and `restore --staged` write.

use std::collections::{BTreeMap, HashSet};
use std::fs::Metadata;
use std::path::Path;

use anyhow::{Context, Result, bail};
use pyn_git::{Index, IndexEntry, Ref, Timestamp};
use pyn_gitwork::{Head, Kind, Level, Pathspec, fs_path, parse_rules, worktree_mode};
use pyn_object::{
    Commit, FileMode, Kind as ObjectKind, ObjectId, Signature, TreeEntry, encode_tree, hash_object,
};

use super::{GitHome, path_string};

const GITLINK: u32 = 0o160000;
const LOCAL_ONLY: &str = ".pyn/local_only";
/// States that a commit made by pyn would silently ignore, so it refuses them.
const IN_PROGRESS: [&str; 6] = [
    "MERGE_HEAD",
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];
const HOOKS: [&str; 4] = [
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
];

pub struct AddOptions {
    pub paths: Vec<String>,
    pub update: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub force: bool,
}

fn raw_mode(mode: FileMode) -> u32 {
    match mode {
        FileMode::Regular => 0o100644,
        FileMode::Executable => 0o100755,
        FileMode::Symlink => 0o120000,
        FileMode::Gitlink => GITLINK,
        FileMode::Tree => 0o040000,
    }
}

fn leaf_mode(raw: u32) -> FileMode {
    match raw {
        GITLINK => FileMode::Gitlink,
        0o120000 => FileMode::Symlink,
        m if m & 0o111 != 0 => FileMode::Executable,
        _ => FileMode::Regular,
    }
}

/// The index as read, with its raw bytes so a write can tell whether anything else changed it meanwhile.
struct OpenIndex {
    raw: Option<Vec<u8>>,
    index: Index,
}

fn read_raw(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

impl GitHome {
    fn open_index(&self) -> Result<OpenIndex> {
        let raw = read_raw(&self.repo.git().index_path())?;
        let index = match &raw {
            Some(bytes) => Index::parse(bytes, self.repo.algorithm())?,
            None => Index::new(2),
        };
        Ok(OpenIndex { raw, index })
    }

    fn save_index(&self, opened: &OpenIndex, index: &Index) -> Result<()> {
        if read_raw(&self.repo.git().index_path())? != opened.raw {
            bail!("the index changed while pyn was working; run the command again");
        }
        Ok(self.repo.git().write_index(index)?)
    }
}

/// The repository paths the arguments name, with `""` (everything) when one of them is the root.
struct Specs {
    spec: Pathspec,
    named: Vec<(String, String)>,
}

fn resolve_specs(git: &GitHome, cwd: &Path, args: &[String]) -> Result<Specs> {
    let named = args
        .iter()
        .map(|a| Ok((a.clone(), git.ws.repo_prefix(cwd, a)?)))
        .collect::<Result<Vec<_>>>()?;
    let spec = if named.iter().any(|(_, p)| p.is_empty()) {
        Pathspec::default()
    } else {
        Pathspec::new(named.iter().map(|(_, p)| p.clone().into_bytes()))
    };
    Ok(Specs { spec, named })
}

fn under(path: &[u8], prefix: &[u8]) -> bool {
    prefix.is_empty()
        || path == prefix
        || (path.starts_with(prefix) && path.get(prefix.len()) == Some(&b'/'))
}

/// Fails for a name that matches nothing in the index, `from` or the work tree, as git does.
fn require_matches(
    git: &GitHome,
    named: &[(String, String)],
    known: &[&[u8]],
    what: &str,
) -> Result<()> {
    for (arg, prefix) in named {
        let on_disk =
            std::fs::symlink_metadata(fs_path(git.repo.root()?, prefix.as_bytes())).is_ok();
        if !on_disk && !known.iter().any(|p| under(p, prefix.as_bytes())) {
            bail!("pathspec '{arg}' did not match any {what}");
        }
    }
    Ok(())
}

struct Ignores {
    ignore: pyn_gitwork::Ignore,
}

impl Ignores {
    fn level(git: &GitHome, dir: &[u8]) -> Result<Option<Level>> {
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
    fn hides(&self, git: &GitHome, path: &[u8], is_dir: bool) -> Result<bool> {
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

fn child(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = dir.to_vec();
    if !p.is_empty() {
        p.push(b'/');
    }
    p.extend_from_slice(name);
    p
}

fn dir_entries(path: &Path) -> Result<Vec<(Vec<u8>, std::fs::FileType)>> {
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
    tracked: &'a HashSet<&'a [u8]>,
    gitlinks: &'a HashSet<&'a [u8]>,
    force: bool,
}

impl Walk<'_> {
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
                    || !self.spec.may_contain(&path)
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
    for (path, kind) in git.repo.unstaged_changes(index)? {
        if !specs.spec.matches(&path) {
            continue;
        }
        match kind {
            Kind::Unmerged => bail!(
                "'{}' is unmerged; pyn cannot resolve merge conflicts yet",
                path_string(&path)
            ),
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
        for (_, prefix) in &specs.named {
            let bytes = prefix.as_bytes();
            let Ok(meta) = std::fs::symlink_metadata(fs_path(root, bytes)) else {
                continue;
            };
            if !prefix.is_empty()
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
        tracked: &tracked,
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
fn link_target(path: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::fs::read_link(path)?.into_os_string().into_vec())
}

#[cfg(not(unix))]
fn link_target(path: &Path) -> Result<Vec<u8>> {
    Ok(std::fs::read_link(path)?
        .to_string_lossy()
        .into_owned()
        .into_bytes())
}

#[cfg(unix)]
fn stat_fields(entry: &mut IndexEntry, meta: &Metadata) {
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
fn stat_fields(entry: &mut IndexEntry, meta: &Metadata) {
    entry.size = meta.len() as u32;
    let _ = Timestamp::default();
}

/// Writes the file's blob and returns the index entry for it; `existing` keeps the mode where `core.filemode` is off.
fn stage_file(git: &GitHome, path: &[u8], existing: Option<&IndexEntry>) -> Result<IndexEntry> {
    let file = fs_path(git.repo.root()?, path);
    let meta =
        std::fs::symlink_metadata(&file).with_context(|| format!("reading {}", file.display()))?;
    let mut mode = worktree_mode(&meta).with_context(|| {
        format!(
            "'{}' is not a regular file or a symbolic link",
            path_string(path)
        )
    })?;
    let bytes = if mode == FileMode::Symlink {
        link_target(&file)?
    } else {
        std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?
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

type Entries = BTreeMap<(Vec<u8>, u8), IndexEntry>;

fn entries_of(index: &Index) -> Entries {
    index
        .entries
        .iter()
        .map(|e| ((e.path.clone(), e.stage), e.clone()))
        .collect()
}

fn index_of(version: u32, entries: Entries) -> Index {
    Index {
        version,
        entries: entries.into_values().collect(),
    }
}

/// Puts `entry` in place of whatever file or folder the index had at its path or above it.
fn put_entry(entries: &mut Entries, entry: IndexEntry) {
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

fn remove_entry(entries: &mut Entries, path: &[u8]) {
    for stage in 0..=3 {
        entries.remove(&(path.to_vec(), stage));
    }
}

/// Stages every change `plan` found into `entries`; prints each one with `verbose` or `dry_run`.
fn apply(
    git: &GitHome,
    entries: &mut Entries,
    ops: &BTreeMap<Vec<u8>, Op>,
    dry_run: bool,
    verbose: bool,
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
                if !dry_run {
                    let entry = stage_file(git, path, entries.get(&(path.clone(), 0)))?;
                    put_entry(entries, entry);
                }
            }
        }
    }
    Ok(())
}

/// `pyn add`: returns whether some named path was refused (the exit status is then 1, as in git).
pub fn add(git: &GitHome, cwd: &Path, options: &AddOptions) -> Result<bool> {
    let opened = git.open_index()?;
    let specs = resolve_specs(git, cwd, &options.paths)?;
    let known: Vec<&[u8]> = opened
        .index
        .entries
        .iter()
        .map(|e| e.path.as_slice())
        .collect();
    require_matches(git, &specs.named, &known, "files")?;
    let plan = plan(git, &opened.index, &specs, options.update, options.force)?;
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
        options.dry_run,
        options.verbose,
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
    for (arg, prefix) in &specs.named {
        if !known.iter().any(|p| under(p, prefix.as_bytes())) {
            bail!("pathspec '{arg}' did not match any file(s) known to git");
        }
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
    let body = encode_tree(&entries)?;
    Ok(git.repo.objects().write(ObjectKind::Tree, &body)?)
}

/// Writes the trees for the index's resolved entries and returns the root tree's id.
fn write_tree(git: &GitHome, index: &Index) -> Result<ObjectId> {
    let mut items: Vec<(&[u8], FileMode, ObjectId)> = index
        .entries
        .iter()
        .filter(|e| e.stage == 0 && !e.intent_to_add)
        .map(|e| (e.path.as_slice(), leaf_mode(e.mode), e.id))
        .collect();
    items.sort_by(|a, b| a.0.cmp(b.0));
    write_dir(git, &items, 0)
}

fn local_offset_minutes() -> i32 {
    chrono::Local::now().offset().local_minus_utc() / 60
}

/// A date as git reads it from `GIT_AUTHOR_DATE`: `<seconds> <+hhmm>`, with or without a leading `@`, or ISO 8601.
fn parse_date(text: &str) -> Result<(i64, i32)> {
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
        .or_else(|_| chrono::DateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S %z"))
        .or_else(|_| chrono::DateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%z"));
    match parsed {
        Ok(d) => Ok((d.timestamp(), d.offset().local_minus_utc() / 60)),
        Err(_) => bail!("invalid date format: {text}"),
    }
}

fn clean_ident(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | '\n'))
        .collect::<String>()
        .trim()
        .to_string()
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// The author or committer: `GIT_<ROLE>_*`, then `<role>.name` and `<role>.email`, then `user.*`.
fn identity(git: &GitHome, role: &str) -> Result<Signature> {
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
fn clean_message(messages: &[String]) -> Vec<u8> {
    let text = messages.join("\n\n");
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out.into_bytes()
}

fn refuse_unsafe_state(git: &GitHome) -> Result<()> {
    let dir = git.repo.git().git_dir();
    if let Some(name) = IN_PROGRESS.iter().find(|n| dir.join(n).exists()) {
        bail!(
            "a merge, cherry-pick, revert or rebase is in progress ({name}); \
             pyn cannot continue it, so finish or abort it with git first"
        );
    }
    if git.repo.bool_setting("commit.gpgsign", false) {
        bail!("commit.gpgsign is set and pyn cannot sign commits yet; unset it to commit with pyn");
    }
    Ok(())
}

fn warn_about_hooks(git: &GitHome) {
    let common = git.repo.git().common_dir();
    let hooks = match git.repo.config().get("core.hookspath") {
        Some(p) => common.join(p),
        None => common.join("hooks"),
    };
    if HOOKS.iter().any(|h| hooks.join(h).is_file()) {
        eprintln!(
            "warning: pyn does not run git hooks; the ones in {} were skipped",
            hooks.display()
        );
    }
}

/// `pyn commit` on a branch: one commit with the index as its tree, then the branch moves to it.
pub fn commit(git: &GitHome, messages: &[String], all: bool) -> Result<()> {
    refuse_unsafe_state(git)?;
    let head = git.repo.head()?;
    let (branch_ref, parent) = match (&head, git.repo.git().refs().head()?) {
        (Head::Branch { id, .. }, Ref::Symbolic(target)) => (target, *id),
        _ => bail!(
            "HEAD is detached; pyn commits only on a branch, so create or switch to one with git first"
        ),
    };
    let author = identity(git, "author")?;
    let committer = identity(git, "committer")?;
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        bail!("the index has unmerged paths; pyn cannot resolve merge conflicts yet");
    }
    let mut entries = entries_of(&opened.index);
    let mut staged_now = false;
    if all {
        let specs = Specs {
            spec: Pathspec::default(),
            named: Vec::new(),
        };
        let plan = plan(git, &opened.index, &specs, true, false)?;
        staged_now = !plan.ops.is_empty();
        apply(git, &mut entries, &plan.ops, false, false)?;
    }
    let index = index_of(opened.index.version, entries);
    let tree = write_tree(git, &index)?;
    let parent_tree = match parent {
        Some(p) => git.repo.commit(&p)?.tree,
        None => hash_object(git.repo.algorithm(), ObjectKind::Tree, b"")?,
    };
    if tree == parent_tree {
        eprintln!("nothing to commit; stage changes with `pyn add`, or use `pyn commit -a`");
        std::process::exit(1);
    }
    warn_about_hooks(git);
    let message = clean_message(messages);
    let record = Commit {
        tree,
        parents: parent.into_iter().collect(),
        author,
        committer,
        extra_headers: Vec::new(),
        message: message.clone(),
    };
    let id = git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode())?;
    if staged_now {
        git.save_index(&opened, &index)?;
    }
    if git.repo.head()? != head {
        bail!("HEAD moved while committing; run the command again");
    }
    git.repo.git().refs().update(&branch_ref, &id)?;
    let Head::Branch { name, .. } = &head else {
        unreachable!("checked above");
    };
    let root = if parent.is_none() {
        " (root-commit)"
    } else {
        ""
    };
    let subject = String::from_utf8_lossy(&message);
    let changed = git
        .repo
        .diff_trees(
            parent.map(|_| &parent_tree),
            Some(&tree),
            &Pathspec::default(),
        )?
        .len();
    println!(
        "[{name}{root} {}] {}",
        &id.to_hex()[..7],
        subject.lines().next().unwrap_or_default()
    );
    println!("{changed} file(s) changed");
    Ok(())
}

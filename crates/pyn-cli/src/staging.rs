//! The staging index of a pyn server workspace: `add`, `restore --staged` and `commit` of the staged changes.
//!
//! The index is `.pyn/local_only/state/index.toml` and the staged content is kept beside it in
//! `.pyn/local_only/staged/<blob id>`, so edits made after `add` stay unstaged. Nothing here is sent to the server
//! until `commit`, which sends the whole staged set as one changeset. A staged deletion has no content.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pyn_proto as api;
use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::local_commit::{self, Staged};
use crate::local_store::LocalStore;
use crate::workspace::{Workspace, hash_of, set_read_only};
use crate::{branch, localfile, sync};

const INDEX_VERSION: u32 = 1;

/// A staged file: its content (by blob id), its mode, and the revision the workspace was at when it was staged.
/// A deletion has an empty `hash` and the mode and revision of the file removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub hash: String,
    pub file_mode: api::FileMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

impl Entry {
    /// Whether the file on disk, with this mode and content, differs from what is staged.
    pub fn differs(&self, file_mode: api::FileMode, bytes: &[u8]) -> bool {
        self.deleted
            || self.hash != hash_of(bytes)
            || localfile::mode_differs(self.file_mode, file_mode)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Default)]
pub struct Index {
    pub entries: BTreeMap<String, Entry>,
}

fn index_path(ws: &Workspace) -> PathBuf {
    ws.dir.join("local_only/state/index.toml")
}

fn staged_dir(ws: &Workspace) -> PathBuf {
    ws.dir.join("local_only/staged")
}

/// Where the staged content with this blob id is kept.
pub fn content_path(ws: &Workspace, hash: &str) -> PathBuf {
    staged_dir(ws).join(hash)
}

impl Index {
    pub fn load(ws: &Workspace) -> Result<Self> {
        match std::fs::read_to_string(index_path(ws)) {
            Ok(text) => {
                let file: IndexFile = toml::from_str(&text).context("reading the staging index")?;
                if file.version > INDEX_VERSION {
                    bail!(
                        "the staging index is version {}, newer than this pyn understands",
                        file.version
                    );
                }
                Ok(Self {
                    entries: file.entries,
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Writes the index (or removes its file when nothing is staged) and drops staged content nothing refers to.
    pub fn save(&self, ws: &Workspace) -> Result<()> {
        let path = index_path(ws);
        if self.entries.is_empty() {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let text = toml::to_string_pretty(&IndexFile {
                version: INDEX_VERSION,
                entries: self.entries.clone(),
            })?;
            std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        }
        self.prune(ws)
    }

    fn prune(&self, ws: &Workspace) -> Result<()> {
        let keep: BTreeSet<&str> = self.entries.values().map(|e| e.hash.as_str()).collect();
        let Ok(dir) = std::fs::read_dir(staged_dir(ws)) else {
            return Ok(());
        };
        for file in dir.flatten() {
            if !keep.contains(file.file_name().to_string_lossy().as_ref()) {
                let _ = std::fs::remove_file(file.path());
            }
        }
        Ok(())
    }

    /// Forgets the staged entries for `paths`, as after a commit of exactly those paths.
    pub fn forget(ws: &Workspace, paths: &[String]) -> Result<()> {
        let mut index = Self::load(ws)?;
        let before = index.entries.len();
        index.entries.retain(|p, _| !paths.contains(p));
        if index.entries.len() != before {
            index.save(ws)?;
        }
        Ok(())
    }
}

pub(crate) fn store(ws: &Workspace, hash: &str, bytes: &[u8]) -> Result<()> {
    let dir = staged_dir(ws);
    std::fs::create_dir_all(&dir)?;
    let target = dir.join(hash);
    if !target.exists() {
        let tmp = dir.join(format!("{hash}.tmp"));
        std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &target)?;
    }
    Ok(())
}

fn under(path: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || path == prefix
        || (path.starts_with(prefix) && path[prefix.len()..].starts_with('/'))
}

/// The paths named on the command line, as repository paths; the empty path is everything.
struct Specs {
    named: Vec<(String, String)>,
}

impl Specs {
    fn resolve(ws: &Workspace, cwd: &Path, args: &[String]) -> Result<Self> {
        let named = args
            .iter()
            .map(|a| Ok((a.clone(), ws.repo_prefix(cwd, a)?)))
            .collect::<Result<_>>()?;
        Ok(Self { named })
    }

    fn matches(&self, path: &str) -> bool {
        self.named.is_empty() || self.named.iter().any(|(_, p)| under(path, p))
    }

    /// Fails for a name that is neither on disk nor known to the workspace.
    fn require_known(&self, ws: &Workspace, known: &BTreeSet<&str>, what: &str) -> Result<()> {
        for (arg, prefix) in &self.named {
            let on_disk = prefix.is_empty()
                || std::fs::symlink_metadata(ws.abs(prefix)).is_ok()
                || known.iter().any(|p| under(p, prefix));
            if !on_disk {
                bail!("pathspec '{arg}' did not match any {what}");
            }
        }
        Ok(())
    }
}

pub struct AddArgs {
    pub paths: Vec<String>,
    pub update: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub force: bool,
}

enum Op {
    Put {
        file_mode: api::FileMode,
        bytes: Vec<u8>,
    },
    /// The file is gone from disk: stage its removal.
    Delete { file_mode: api::FileMode },
    /// The file is back to what the last commit has, so nothing is staged for it.
    Unstage,
}

struct Plan {
    ops: BTreeMap<String, Op>,
    ignored: Vec<String>,
}

fn plan(ws: &Workspace, index: &Index, specs: &Specs, args: &AddArgs) -> Result<Plan> {
    let state = ws.load_state()?;
    let conflicted = crate::merging::conflicted(ws);
    let mut ops = BTreeMap::new();
    let mut ignored = Vec::new();
    for (path, known) in &state {
        if !specs.matches(path) {
            continue;
        }
        let file = ws.abs(path);
        if !localfile::exists(&file) {
            if conflicted.contains(path) && !crate::merging::is_local(ws) {
                eprintln!(
                    "warning: {path} was deleted; a merge conflict cannot be resolved by deleting the file, so it is not staged"
                );
            } else if !index.entries.get(path).is_some_and(|s| s.deleted) {
                ops.insert(
                    path.clone(),
                    Op::Delete {
                        file_mode: known.file_mode,
                    },
                );
            }
            continue;
        }
        let (file_mode, bytes) = localfile::read(&file)?;
        if !known.changed(file_mode, &bytes) && !conflicted.contains(path) {
            if index.entries.contains_key(path) {
                ops.insert(path.clone(), Op::Unstage);
            }
            continue;
        }
        let staged_same = index
            .entries
            .get(path)
            .is_some_and(|s| !s.differs(file_mode, &bytes));
        if !staged_same {
            ops.insert(path.clone(), Op::Put { file_mode, bytes });
        }
    }
    if args.update {
        return Ok(Plan { ops, ignored });
    }
    let mut candidates: BTreeSet<String> = ws.scan_with(!args.force)?.into_iter().collect();
    for (_, prefix) in &specs.named {
        if !prefix.is_empty()
            && localfile::exists(&ws.abs(prefix))
            && !state.contains_key(prefix)
            && !args.force
            && ws.is_ignored(prefix)?
        {
            ignored.push(prefix.clone());
        }
    }
    candidates.retain(|p| !state.contains_key(p) && specs.matches(p) && !ignored.contains(p));
    for path in candidates {
        let (file_mode, bytes) = localfile::read(&ws.abs(&path))?;
        let staged_same = index
            .entries
            .get(&path)
            .is_some_and(|s| !s.differs(file_mode, &bytes));
        if !staged_same {
            ops.insert(path, Op::Put { file_mode, bytes });
        }
    }
    Ok(Plan { ops, ignored })
}

/// Fails, staging nothing, when a path to stage is exclusive and the signed-in user does not hold its lock.
fn require_locks(backend: &dyn Backend, paths: &[&String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let me = backend.whoami()?;
    let files: BTreeMap<String, api::FileEntry> = backend
        .list_files()?
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
    let refused: Vec<String> = paths
        .iter()
        .filter_map(|p| {
            let entry = files.get(*p)?;
            let held = entry.lock.as_ref().is_some_and(|l| l.owner == me);
            (entry.mode == api::Mode::Exclusive && !held).then(|| {
                let by = entry
                    .lock
                    .as_ref()
                    .map_or(String::new(), |l| format!(" (locked by {})", l.owner));
                format!(
                    "{p} is exclusive and you do not hold its lock{by}; run `pyn lock {p}` first"
                )
            })
        })
        .collect();
    if !refused.is_empty() {
        bail!("{}", refused.join("\n"));
    }
    Ok(())
}

/// `pyn add`: stages the changed files under the paths; true when some named path was refused (exit status 1).
pub fn add(backend: &dyn Backend, ws: &Workspace, cwd: &Path, args: &AddArgs) -> Result<bool> {
    let mut index = Index::load(ws)?;
    let specs = Specs::resolve(ws, cwd, &args.paths)?;
    let state = ws.load_state()?;
    let known: BTreeSet<&str> = state
        .keys()
        .map(String::as_str)
        .chain(index.entries.keys().map(String::as_str))
        .collect();
    specs.require_known(ws, &known, "files")?;
    let plan = plan(ws, &index, &specs, args)?;
    let conflicted = crate::merging::conflicted(ws);
    let puts: Vec<&String> = plan
        .ops
        .iter()
        .filter(|(p, op)| {
            matches!(op, Op::Put { .. } | Op::Delete { .. }) && !conflicted.contains(*p)
        })
        .map(|(p, _)| p)
        .collect();
    require_locks(backend, &puts)?;
    for (path, op) in plan.ops {
        match op {
            Op::Put { file_mode, bytes } => {
                if args.dry_run || args.verbose {
                    println!("add '{path}'");
                }
                if args.dry_run {
                    continue;
                }
                if conflicted.contains(&path) && crate::merging::has_markers(&ws.abs(&path)) {
                    eprintln!("warning: {path} still has conflict markers");
                }
                let hash = hash_of(&bytes);
                store(ws, &hash, &bytes)?;
                index.entries.insert(
                    path.clone(),
                    Entry {
                        hash,
                        file_mode,
                        base_revision: state.get(&path).map(|k| k.revision),
                        deleted: false,
                    },
                );
            }
            Op::Delete { file_mode } => {
                if args.dry_run || args.verbose {
                    println!("remove '{path}'");
                }
                if args.dry_run {
                    continue;
                }
                index.entries.insert(
                    path.clone(),
                    Entry {
                        hash: String::new(),
                        file_mode,
                        base_revision: state.get(&path).map(|k| k.revision),
                        deleted: true,
                    },
                );
            }
            Op::Unstage => {
                if args.dry_run || args.verbose {
                    println!("unstage '{path}'");
                }
                if !args.dry_run {
                    index.entries.remove(&path);
                }
            }
        }
    }
    if !args.dry_run {
        index.save(ws)?;
    }
    if plan.ignored.is_empty() {
        return Ok(false);
    }
    eprintln!("The following paths are ignored by .pyn/ignore:");
    for path in &plan.ignored {
        eprintln!("{path}");
    }
    eprintln!("hint: Use -f if you really want to add them.");
    Ok(true)
}

/// `pyn restore --staged`: takes the paths out of the staged changes; the files on disk are left as they are.
pub fn unstage(ws: &Workspace, cwd: &Path, paths: &[String]) -> Result<()> {
    let mut index = Index::load(ws)?;
    let specs = Specs::resolve(ws, cwd, paths)?;
    let state = ws.load_state()?;
    let known: BTreeSet<&str> = state
        .keys()
        .map(String::as_str)
        .chain(index.entries.keys().map(String::as_str))
        .collect();
    for (arg, prefix) in &specs.named {
        if !known.iter().any(|p| under(p, prefix)) {
            bail!("pathspec '{arg}' did not match any file(s) known to pyn");
        }
    }
    index.entries.retain(|p, _| !specs.matches(p));
    index.save(ws)
}

/// `pyn commit` of the staged changes (after staging every changed tracked file with `all`).
pub fn commit(backend: &dyn Backend, ws: &Workspace, message: &str, all: bool) -> Result<()> {
    if all {
        let args = AddArgs {
            paths: Vec::new(),
            update: true,
            dry_run: false,
            verbose: false,
            force: false,
        };
        add(backend, ws, ws.root.as_path(), &args)?;
    }
    let index = Index::load(ws)?;
    if index.entries.is_empty() {
        eprintln!("nothing to commit; stage changes with `pyn add`, or use `pyn commit -a`");
        std::process::exit(1);
    }
    if let Some(store) = LocalStore::open(ws) {
        return commit_local(backend, ws, &store, &index, message);
    }
    let mut entries = Vec::new();
    let mut deletions = Vec::new();
    let mut written = Vec::new();
    let mut removed = Vec::new();
    for (path, staged) in &index.entries {
        if staged.deleted {
            deletions.push(api::ChangesetDeletion {
                path: path.clone(),
                base_revision: staged.base_revision,
            });
            removed.push(path);
            continue;
        }
        let file = content_path(ws, &staged.hash);
        let sent = if staged.file_mode == api::FileMode::Symlink {
            backend.upload_bytes(std::fs::read(&file)?)?
        } else {
            backend.upload(&file)?
        };
        if sent != staged.hash {
            bail!(
                "the server stored {path} as {sent}, not the staged {}",
                staged.hash
            );
        }
        entries.push(api::ChangesetEntry {
            path: path.clone(),
            content: sent,
            file_mode: Some(staged.file_mode),
            base_revision: staged.base_revision,
        });
        written.push((path, staged));
    }
    let request = api::ChangesetRequest {
        entries,
        deletions,
        message: message.to_string(),
        branch: None,
    };
    let response = backend
        .commit_changeset(&request)
        .map_err(|e| crate::behind_hint(e, true))?;
    let (made, gone) = response.revisions.split_at(written.len());
    for (rev, (path, staged)) in made.iter().zip(&written) {
        sync::after_commit(backend, ws, path, rev.id, &staged.hash, staged.file_mode)?;
        println!("{} is now at revision {}", rev.path, rev.id);
    }
    let mut state = ws.load_state()?;
    for (rev, path) in gone.iter().zip(&removed) {
        state.remove(path.as_str());
        println!("{} was deleted at revision {}", rev.path, rev.id);
    }
    if !removed.is_empty() {
        ws.save_state(&state)?;
    }
    Index::default().save(ws)
}

/// `pyn commit` with `local_store` on: records the staged changes in the local store and publishes nothing.
fn commit_local(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    index: &Index,
    message: &str,
) -> Result<()> {
    let paths: Vec<&String> = index.entries.keys().collect();
    require_locks(backend, &paths)?;
    let state = ws.load_state()?;
    let mut changes = Vec::new();
    for (path, staged) in &index.entries {
        let content = if staged.deleted {
            None
        } else {
            let bytes = std::fs::read(content_path(ws, &staged.hash))
                .with_context(|| format!("reading the staged content of {path}"))?;
            Some((staged.file_mode, bytes))
        };
        changes.push(Staged {
            path: path.clone(),
            content,
        });
    }
    let branch = branch::name(Some(ws));
    if store.tip(&branch)?.is_none() && backend.head_commit()?.is_some() {
        bail!(
            "the local store has no history of branch '{branch}' yet; run `pyn update` to bring it up to date"
        );
    }
    let who = local_commit::signature(&backend.whoami()?);
    let Some(made) = store.commit_staged(&branch, &changes, message, &who)? else {
        eprintln!("nothing to commit; the staged changes match the local branch");
        std::process::exit(1);
    };
    let mut state = state;
    for (path, staged) in &index.entries {
        if staged.deleted {
            state.remove(path.as_str());
        }
    }
    ws.save_state(&state)?;
    for (path, staged) in index.entries.iter().filter(|(_, s)| !s.deleted) {
        let revision = state.get(path).map_or(0, |k| k.revision);
        sync::after_commit(backend, ws, path, revision, &staged.hash, staged.file_mode)?;
    }
    let me = backend.whoami()?;
    for entry in backend.list_files()? {
        let staged = index.entries.get(&entry.path).is_some_and(|s| !s.deleted);
        if staged && !sync::wants_read_only(&entry, &me) {
            set_read_only(&ws.dest(&entry.path)?, false)?;
        }
    }
    Index::default().save(ws)?;
    let first = message.lines().next().unwrap_or_default();
    let root = if made.root { " (root-commit)" } else { "" };
    println!(
        "[{branch}{root} {}] {first}",
        crate::snapshot::short(&made.id.to_hex())
    );
    println!("{} file(s) changed", made.changed);
    println!("this commit is local and not yet published to the server");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_under_a_folder_only_at_a_slash() {
        assert!(under("src/a", "src") && under("src", "src") && under("x", ""));
        assert!(!under("srcx/a", "src"));
    }

    #[test]
    fn a_staged_entry_differs_when_content_or_mode_does() {
        let entry = Entry {
            hash: hash_of(b"one"),
            file_mode: api::FileMode::Regular,
            base_revision: Some(1),
            deleted: false,
        };
        assert!(!entry.differs(api::FileMode::Regular, b"one"));
        assert!(entry.differs(api::FileMode::Regular, b"two"));
        assert_eq!(entry.differs(api::FileMode::Executable, b"one"), cfg!(unix));
    }
}

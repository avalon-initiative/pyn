//! Keeping a workspace and the server in step: cloning, reporting differences and fetching newer revisions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use pyn_proto as api;

use crate::backend::Backend;
use crate::localfile;
use crate::snapshot::short;
use crate::staging;
use crate::table;
use crate::workspace::{FileState, Settings, Workspace, set_read_only};

pub fn mode_name(mode: api::Mode) -> &'static str {
    match mode {
        api::Mode::Shared => "shared",
        api::Mode::Exclusive => "exclusive",
    }
}

/// Exclusive files stay read-only until their owner holds the lock.
fn wants_read_only(entry: &api::FileEntry, me: &str) -> bool {
    entry.mode == api::Mode::Exclusive && entry.lock.as_ref().is_none_or(|l| l.owner != me)
}

fn fetch(
    backend: &dyn Backend,
    ws: &Workspace,
    entry: &api::FileEntry,
    revision: u64,
    me: &str,
) -> Result<FileState> {
    let target = ws.abs(&entry.path);
    set_read_only(&target, false)?;
    let (_, hash) = backend.download(&entry.path, Some(revision), &target)?;
    localfile::apply(&target, entry.file_mode)?;
    set_read_only(&target, wants_read_only(entry, me))?;
    Ok(FileState {
        revision,
        hash,
        legacy: false,
        mode: mode_name(entry.mode).to_string(),
        file_mode: entry.file_mode,
    })
}

/// The server's files with the revision each had as of `commit` (the head if `None`); a path the commit does not
/// have, such as one created later, has no revision.
pub fn entries_at(backend: &dyn Backend, commit: Option<&str>) -> Result<Vec<api::FileEntry>> {
    let mut entries = backend.list_files()?;
    let Some(commit) = commit else {
        return Ok(entries);
    };
    let (_, snap) = backend.snapshot(Some(commit))?;
    let snap: BTreeMap<&str, &api::SnapshotEntry> =
        snap.iter().map(|e| (e.path.as_str(), e)).collect();
    for entry in &mut entries {
        match snap.get(entry.path.as_str()) {
            Some(s) => {
                entry.revision = Some(s.revision);
                entry.mode = s.mode;
                entry.file_mode = s.file_mode;
            }
            None => entry.revision = None,
        }
    }
    Ok(entries)
}

/// Creates a workspace in `root` and downloads every file as of `commit` (the head if `None`).
pub fn clone_into(
    backend: &dyn Backend,
    root: &Path,
    server: &str,
    repo: &str,
    commit: Option<&str>,
) -> Result<()> {
    if root.exists() && std::fs::read_dir(root)?.next().is_some() {
        bail!("{} already exists and is not empty", root.display());
    }
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    let root = root.canonicalize()?;
    let ws = Workspace::create(&root)?;
    Settings {
        server: Some(server.to_string()),
        user: None,
        repo: Some(repo.to_string()),
    }
    .save(&ws.config_path())?;

    let me = backend.whoami()?;
    let mut state = BTreeMap::new();
    for entry in entries_at(backend, commit)? {
        let Some(revision) = entry.revision else {
            continue;
        };
        state.insert(
            entry.path.clone(),
            fetch(backend, &ws, &entry, revision, &me)?,
        );
    }
    ws.save_state(&state)?;
    let at = commit.map_or(String::new(), |c| format!(" at {}", short(c)));
    println!(
        "cloned {} files from {repo}{at} into {}",
        state.len(),
        root.display()
    );
    Ok(())
}

/// "7h 59m", "2d 3h" or "<1m" until `expires_at`; "expired" once past.
fn time_left(expires_at: chrono::DateTime<chrono::Utc>) -> String {
    let mins = (expires_at - chrono::Utc::now()).num_minutes();
    match mins {
        m if m < 0 => "expired".into(),
        0 => "<1m".into(),
        m if m < 60 => format!("{m}m"),
        m if m < 24 * 60 => format!("{}h {}m", m / 60, m % 60),
        m => format!("{}d {}h", m / (24 * 60), m / 60 % 24),
    }
}

/// Lists what differs between the workspace and the server as a table; clean, unlocked files are left out.
pub fn status(backend: &dyn Backend, ws: &Workspace, commit: Option<&str>) -> Result<()> {
    let entries: BTreeMap<String, api::FileEntry> = entries_at(backend, commit)?
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
    let state = ws.load_state()?;
    let index = staging::Index::load(ws)?;
    let local: BTreeSet<String> = ws.scan()?.into_iter().collect();
    let me = backend.whoami()?;
    let paths: BTreeSet<&String> = entries
        .keys()
        .chain(state.keys())
        .chain(local.iter())
        .chain(index.entries.keys())
        .collect();

    let (mut staged, mut modified, mut behind, mut ahead, mut new, mut untracked) =
        (0, 0, 0, 0, 0, 0);
    let (mut mine, mut theirs) = (0, 0);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for path in paths {
        let (known, entry) = (state.get(path), entries.get(path));
        let on_disk = localfile::exists(&ws.abs(path));
        let mut notes: Vec<String> = Vec::new();
        let staged_entry = index.entries.get(path);
        let stage =
            staged_entry.map_or("-", |_| if known.is_some() { "modified" } else { "added" });
        staged += usize::from(staged_entry.is_some());
        let dirty = |file_mode, bytes: &[u8]| match (staged_entry, known) {
            (Some(s), _) => s.differs(file_mode, bytes),
            (None, Some(k)) => k.changed(file_mode, bytes),
            (None, None) => true,
        };
        match (known, on_disk) {
            (Some(k), true) => {
                let (file_mode, bytes) = localfile::read(&ws.abs(path))?;
                if dirty(file_mode, &bytes) {
                    notes.push("modified".into());
                    modified += 1;
                }
                let theirs = entry.and_then(|e| e.revision);
                match (commit, theirs) {
                    (None, Some(head)) if head > k.revision => {
                        notes.push(format!("behind (head r{head})"));
                        behind += 1;
                    }
                    (Some(c), Some(rev)) if rev > k.revision => {
                        notes.push(format!("behind ({} has r{rev})", short(c)));
                        behind += 1;
                    }
                    (Some(c), Some(rev)) if rev < k.revision => {
                        notes.push(format!("ahead ({} has r{rev})", short(c)));
                        ahead += 1;
                    }
                    (Some(c), None) if entry.is_some() => {
                        notes.push(format!("ahead (not in {})", short(c)));
                        ahead += 1;
                    }
                    _ => {}
                }
            }
            (Some(_), false) => {
                notes.push("deleted locally".into());
                modified += 1;
            }
            (None, true) if staged_entry.is_some() => {
                let (file_mode, bytes) = localfile::read(&ws.abs(path))?;
                if dirty(file_mode, &bytes) {
                    notes.push("modified".into());
                    modified += 1;
                }
            }
            (None, true) if local.contains(path) => {
                let on_server = entry.is_some_and(|e| e.revision.is_some());
                notes.push(if on_server {
                    "untracked (on the server: run `pyn update`)".into()
                } else {
                    "untracked".into()
                });
                untracked += 1;
            }
            (None, false) if entry.is_some() => {
                if entry.is_some_and(|e| e.revision.is_some()) {
                    notes.push("new".into());
                    new += 1;
                } else if entry.is_some_and(|e| e.lock.is_some()) {
                    notes.push("locked, not checked in yet".into());
                }
            }
            _ => {}
        }
        let lock = entry.and_then(|e| e.lock.as_ref()).map(|lock| {
            let by_me = lock.owner == me;
            if by_me {
                mine += 1
            } else {
                theirs += 1
            }
            format!(
                "{} \u{b7} {}",
                if by_me { "you" } else { &lock.owner },
                time_left(lock.expires_at)
            )
        });
        if notes.is_empty() && lock.is_none() && staged_entry.is_none() {
            continue;
        }
        let mode = entry.map_or_else(
            || known.map_or("-", |k| k.mode.as_str()),
            |e| mode_name(e.mode),
        );
        rows.push(vec![
            mode.to_string(),
            known.map_or("-".to_string(), |k| format!("r{}", k.revision)),
            stage.to_string(),
            if notes.is_empty() {
                "clean".to_string()
            } else {
                notes.join(", ")
            },
            lock.unwrap_or_else(|| "-".into()),
            path.clone(),
        ]);
    }
    if rows.is_empty() {
        println!("everything is up to date");
        return Ok(());
    }
    println!(
        "{}",
        table::render(&["MODE", "REV", "STAGED", "STATE", "LOCK", "PATH"], &rows)
    );
    let ahead = if commit.is_some() {
        format!("{ahead} ahead, ")
    } else {
        String::new()
    };
    println!(
        "{staged} staged, {modified} modified, {behind} behind, {ahead}{new} new, \
         {untracked} untracked, {mine} locked by you, {theirs} locked by others"
    );
    Ok(())
}

/// Fetches files that are new or behind, or with `commit` brings every file to that commit's revision, removing files
/// it does not have. Anything with local changes is left alone and reported.
pub fn update(
    backend: &dyn Backend,
    ws: &Workspace,
    only: &[String],
    commit: Option<&str>,
) -> Result<()> {
    let me = backend.whoami()?;
    let mut state = ws.load_state()?;
    let index = staging::Index::load(ws)?;
    let (mut updated, mut removed, mut skipped) = (0, 0, Vec::new());
    for entry in entries_at(backend, commit)? {
        if !only.is_empty() && !only.contains(&entry.path) {
            continue;
        }
        if index.entries.contains_key(&entry.path) {
            skipped.push(format!(
                "{}: staged; commit it or `pyn restore --staged` it first",
                entry.path
            ));
            continue;
        }
        let Some(head) = entry.revision else {
            if let (Some(c), Some(known)) = (commit, state.get(&entry.path)) {
                let target = ws.abs(&entry.path);
                if localfile::exists(&target) && local_changes(known, &target)? {
                    skipped.push(format!(
                        "{}: local changes; not in {}",
                        entry.path,
                        short(c)
                    ));
                } else {
                    set_read_only(&target, false)?;
                    let _ = std::fs::remove_file(&target);
                    state.remove(&entry.path);
                    removed += 1;
                }
            }
            continue;
        };
        let target = ws.abs(&entry.path);
        let on_disk = localfile::exists(&target);
        match state.get(&entry.path) {
            Some(k) if k.revision == head => {
                set_read_only(&target, wants_read_only(&entry, &me))?;
                state.get_mut(&entry.path).expect("just read").mode =
                    mode_name(entry.mode).to_string();
                continue;
            }
            Some(k) if on_disk && local_changes(k, &target)? => {
                skipped.push(format!(
                    "{}: local changes; r{head} is available",
                    entry.path
                ));
                continue;
            }
            None if on_disk => {
                skipped.push(format!(
                    "{}: a file is already there that pyn does not track",
                    entry.path
                ));
                continue;
            }
            _ => {}
        }
        state.insert(entry.path.clone(), fetch(backend, ws, &entry, head, &me)?);
        updated += 1;
    }
    ws.save_state(&state)?;
    if removed > 0 {
        println!("updated {updated} file(s), removed {removed}");
    } else {
        println!("updated {updated} file(s)");
    }
    for line in skipped {
        println!("skipped {line}");
    }
    Ok(())
}

fn local_changes(known: &FileState, path: &Path) -> Result<bool> {
    let (file_mode, bytes) = localfile::read(path)?;
    Ok(known.changed(file_mode, &bytes))
}

pub fn after_lock(ws: &Workspace, path: &str) -> Result<()> {
    set_read_only(&ws.abs(path), false)
}

pub fn after_unlock(ws: &Workspace, path: &str) -> Result<()> {
    if ws
        .load_state()?
        .get(path)
        .is_some_and(|k| k.mode == "exclusive")
    {
        set_read_only(&ws.abs(path), true)?;
    }
    Ok(())
}

/// Records the new revision, and makes an exclusive file read-only again now that its lock is gone.
pub fn after_commit(
    backend: &dyn Backend,
    ws: &Workspace,
    path: &str,
    revision: u64,
    hash: &str,
    file_mode: api::FileMode,
) -> Result<()> {
    let mut state = ws.load_state()?;
    let mode = match state.get(path) {
        Some(k) => k.mode.clone(),
        None => backend
            .list_files()?
            .into_iter()
            .find(|e| e.path == path)
            .map_or_else(|| "shared".to_string(), |e| mode_name(e.mode).to_string()),
    };
    if mode == "exclusive" {
        set_read_only(&ws.abs(path), true)?;
    }
    state.insert(
        path.to_string(),
        FileState {
            revision,
            hash: hash.to_string(),
            legacy: false,
            mode,
            file_mode,
        },
    );
    ws.save_state(&state)
}

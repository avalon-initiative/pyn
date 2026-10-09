//! Keeping a workspace and the server in step: cloning, reporting differences and fetching newer revisions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use pyn_proto as api;

use crate::client::Api;
use crate::table;
use crate::workspace::{FileState, Settings, Workspace, hash_of, set_read_only};

pub fn mode_name(mode: api::Mode) -> &'static str {
    match mode {
        api::Mode::Shared => "shared",
        api::Mode::Exclusive => "exclusive",
    }
}

fn whoami(api: &Api) -> Result<String> {
    let me: api::Account = api.send(api.get("/v1/me"))?.json()?;
    Ok(me.user)
}

/// Exclusive files stay read-only until their owner holds the lock.
fn wants_read_only(entry: &api::FileEntry, me: &str) -> bool {
    entry.mode == api::Mode::Exclusive && entry.lock.as_ref().is_none_or(|l| l.owner != me)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    set_read_only(path, false)?;
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn fetch(
    api: &Api,
    ws: &Workspace,
    entry: &api::FileEntry,
    revision: u64,
    me: &str,
) -> Result<FileState> {
    let bytes = api.content(&entry.path, Some(revision))?;
    let target = ws.abs(&entry.path);
    write_file(&target, &bytes)?;
    set_read_only(&target, wants_read_only(entry, me))?;
    Ok(FileState {
        revision,
        hash: hash_of(&bytes),
        mode: mode_name(entry.mode).to_string(),
    })
}

/// Creates a workspace in `root` and downloads every file at its head revision.
pub fn clone_into(api: &Api, root: &Path, server: &str, repo: &str) -> Result<()> {
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

    let me = whoami(api)?;
    let mut state = BTreeMap::new();
    for entry in api.list_files()? {
        let Some(revision) = entry.revision else {
            continue;
        };
        state.insert(entry.path.clone(), fetch(api, &ws, &entry, revision, &me)?);
    }
    ws.save_state(&state)?;
    println!(
        "cloned {} files from {repo} into {}",
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
pub fn status(api: &Api, ws: &Workspace) -> Result<()> {
    let entries: BTreeMap<String, api::FileEntry> = api
        .list_files()?
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();
    let state = ws.load_state()?;
    let local: BTreeSet<String> = ws.scan()?.into_iter().collect();
    let me = whoami(api)?;
    let paths: BTreeSet<&String> = entries
        .keys()
        .chain(state.keys())
        .chain(local.iter())
        .collect();

    let (mut modified, mut behind, mut new, mut untracked) = (0, 0, 0, 0);
    let (mut mine, mut theirs) = (0, 0);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for path in paths {
        let (known, entry) = (state.get(path), entries.get(path));
        let on_disk = ws.abs(path).is_file();
        let mut notes: Vec<String> = Vec::new();
        match (known, on_disk) {
            (Some(k), true) => {
                if hash_of(&std::fs::read(ws.abs(path))?) != k.hash {
                    notes.push("modified".into());
                    modified += 1;
                }
                if let Some(head) = entry.and_then(|e| e.revision).filter(|h| *h > k.revision) {
                    notes.push(format!("behind (head r{head})"));
                    behind += 1;
                }
            }
            (Some(_), false) => {
                notes.push("deleted locally".into());
                modified += 1;
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
                } else {
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
        if notes.is_empty() && lock.is_none() {
            continue;
        }
        let mode = entry.map_or_else(
            || known.map_or("-", |k| k.mode.as_str()),
            |e| mode_name(e.mode),
        );
        rows.push(vec![
            mode.to_string(),
            known.map_or("-".to_string(), |k| format!("r{}", k.revision)),
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
        table::render(&["MODE", "REV", "STATE", "LOCK", "PATH"], &rows)
    );
    println!(
        "{modified} modified, {behind} behind, {new} new, {untracked} untracked, \
         {mine} locked by you, {theirs} locked by others"
    );
    Ok(())
}

/// Fetches files that are new or behind. Anything with local changes is left alone and reported.
pub fn update(api: &Api, ws: &Workspace, only: &[String]) -> Result<()> {
    let me = whoami(api)?;
    let mut state = ws.load_state()?;
    let (mut updated, mut skipped) = (0, Vec::new());
    for entry in api.list_files()? {
        if !only.is_empty() && !only.contains(&entry.path) {
            continue;
        }
        let Some(head) = entry.revision else { continue };
        let target = ws.abs(&entry.path);
        let on_disk = target.is_file();
        match state.get(&entry.path) {
            Some(k) if k.revision == head => {
                set_read_only(&target, wants_read_only(&entry, &me))?;
                state.get_mut(&entry.path).expect("just read").mode =
                    mode_name(entry.mode).to_string();
                continue;
            }
            Some(k) if on_disk && hash_of(&std::fs::read(&target)?) != k.hash => {
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
        state.insert(entry.path.clone(), fetch(api, ws, &entry, head, &me)?);
        updated += 1;
    }
    ws.save_state(&state)?;
    println!("updated {updated} file(s)");
    for line in skipped {
        println!("skipped {line}");
    }
    Ok(())
}

pub fn after_checkout(ws: &Workspace, path: &str) -> Result<()> {
    set_read_only(&ws.abs(path), false)
}

pub fn after_release(ws: &Workspace, path: &str) -> Result<()> {
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
pub fn after_checkin(
    api: &Api,
    ws: &Workspace,
    path: &str,
    revision: u64,
    bytes: &[u8],
) -> Result<()> {
    let mut state = ws.load_state()?;
    let mode = match state.get(path) {
        Some(k) => k.mode.clone(),
        None => api
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
            hash: hash_of(bytes),
            mode,
        },
    );
    ws.save_state(&state)
}

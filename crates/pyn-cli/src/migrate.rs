//! `pyn migrate`: converts a workspace made before local commits into one that keeps them (decision #300).

use std::collections::BTreeMap;

use anyhow::{Result, bail};

use crate::backend::Backend;
use crate::staging::Index;
use crate::workspace::Workspace;
use crate::{local_store, localfile, merging, sync};

/// What would be lost or left inconsistent by switching on local commits, as one line per problem.
fn blockers(backend: &dyn Backend, ws: &Workspace) -> Result<Vec<String>> {
    let mut found = Vec::new();
    if merging::in_progress(ws) {
        found.push("a merge is in progress; conclude it or run `pyn merge --abort`".to_string());
    }
    let staged = Index::load(ws)?.entries.len();
    if staged > 0 {
        found.push(format!(
            "{staged} staged change(s); commit them or unstage them with `pyn restore --staged`"
        ));
    }
    let state = ws.load_state()?;
    let changed: Vec<&String> = state
        .iter()
        .filter(|(path, known)| match localfile::read(&ws.abs(path)) {
            Ok((mode, bytes)) => known.changed(mode, &bytes),
            Err(_) => true,
        })
        .map(|(path, _)| path)
        .collect();
    if !changed.is_empty() {
        found.push(format!(
            "{} file(s) changed in the working tree (first: {}); commit them or restore them",
            changed.len(),
            changed[0]
        ));
    }
    let server: BTreeMap<String, u64> = sync::entries_at(backend, None)?
        .into_iter()
        .filter_map(|e| e.revision.map(|r| (e.path, r)))
        .collect();
    let behind = server
        .iter()
        .filter(|(p, r)| state.get(*p).is_none_or(|k| k.revision != **r))
        .count()
        + state.keys().filter(|p| !server.contains_key(*p)).count();
    if behind > 0 {
        found.push(format!(
            "the workspace is not up to date with the server ({behind} path(s) differ); run `pyn update`"
        ));
    }
    Ok(found)
}

pub fn run(backend: &dyn Backend, ws: &Workspace) -> Result<()> {
    let api = backend.pyn_server("migrate")?;
    let mut settings = ws.settings();
    if settings.local_store_on() && local_store::LocalStore::open(ws).is_some() {
        println!("This workspace already keeps local commits; nothing to migrate.");
        return Ok(());
    }
    api.require_local_commits()?;
    let problems = blockers(backend, ws)?;
    if !problems.is_empty() {
        bail!(
            "cannot migrate yet:\n  - {}\nnothing was changed",
            problems.join("\n  - ")
        );
    }
    let before = settings.clone();
    settings.local_store = Some("true".into());
    settings.save(&ws.config_path())?;
    if let Err(e) = local_store::refresh(backend, ws) {
        before.save(&ws.config_path())?;
        return Err(e.context("migrating failed; the workspace is unchanged"));
    }
    println!(
        "Migrated: `pyn commit` now records local commits and `pyn push` publishes them; `pyn fetch` and `pyn pull` bring server commits."
    );
    Ok(())
}

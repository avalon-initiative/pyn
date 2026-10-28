//! `branch`, `switch` and `checkout <branch>` in a workspace with the local store: branches are the store's own refs, as
//! in git, and a switch needs the server only for files whose blobs the store lacks.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use pyn_gitwork::Pathspec;
use pyn_object::ObjectId;
use pyn_proto as api;

use crate::backend::Backend;
use crate::branch::{self, BranchArgs, Step};
use crate::client::Api;
use crate::local_branches::DETACHED;
use crate::local_store::LocalStore;
use crate::snapshot::short;
use crate::staging::Index;
use crate::sync::{self, mode_name};
use crate::workspace::{FileState, Workspace, set_read_only};
use crate::{local_sync, local_tags, localfile, merging};

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Local,
    Remote,
    All,
}

/// The commit a start point names: a branch, `origin/<branch>`, a tag, a hash, `HEAD` and steps back from them.
pub(crate) fn resolve(store: &LocalStore, ws: &Workspace, spec: &str) -> Result<ObjectId> {
    local_tags::resolve_target(store, &branch::name(Some(ws)), spec)
        .map_err(|_| anyhow::anyhow!("not a valid object name: '{spec}'"))
}

fn subject(store: &LocalStore, id: &ObjectId) -> Result<String> {
    let message = String::from_utf8_lossy(&store.repo().commit(id)?.message).into_owned();
    Ok(message.lines().next().unwrap_or_default().to_string())
}

pub(crate) fn branch_command(store: &LocalStore, ws: &Workspace, args: BranchArgs) -> Result<()> {
    let BranchArgs {
        names,
        delete,
        force_delete,
        rename,
        list,
        verbose,
        show_current,
        all,
        remotes,
        force_rename,
        copy,
        force_copy,
        force,
        set_upstream_to,
        unset_upstream,
        track: _,
    } = args;
    if copy || force_copy || force {
        crate::usage_error("`pyn branch` -c, -C and -f need a git repository");
    }
    if (all || remotes)
        && (delete
            || force_delete
            || rename
            || show_current
            || !names.is_empty()
            || set_upstream_to.is_some())
    {
        bail!("`pyn branch -a` and `-r` only list");
    }
    let here = branch::name(Some(ws));
    if show_current {
        if here != DETACHED {
            println!("{here}");
        }
        return Ok(());
    }
    if set_upstream_to.is_some() || unset_upstream {
        return upstream(store, &here, &names, set_upstream_to.as_deref());
    }
    let rename = rename || force_rename;
    let modes = [delete || force_delete, rename, list].map(usize::from);
    if modes.iter().sum::<usize>() > 1 {
        bail!("choose one of --delete, --move and --list");
    }
    if delete || force_delete {
        if names.is_empty() {
            bail!("name the branches to delete");
        }
        return delete_branches(store, &here, &names, force_delete);
    }
    if rename {
        let (old, new) = match names.as_slice() {
            [new] => (here.clone(), new.clone()),
            [old, new] => (old.clone(), new.clone()),
            _ => bail!("`pyn branch -m` takes `[<old>] <new>`"),
        };
        if old == DETACHED {
            bail!("cannot rename the detached HEAD; create a branch with `pyn switch -c <name>`");
        }
        if force_rename && new != old {
            if store.tip(&old)?.is_none() || new == DETACHED {
                bail!("no branch named '{old}', or '{new}' is not a valid branch name");
            }
            if new == here {
                bail!("cannot force-rename over the branch this workspace is on");
            }
            if store.tip(&new)?.is_some() {
                store.delete_branch(&new)?;
            }
        }
        store.rename_branch(&old, &new)?;
        if old == here {
            branch::set_current(ws, &new)?;
            store
                .repo()
                .refs()
                .set_symbolic("HEAD", &format!("refs/heads/{new}"))?;
        }
        return Ok(());
    }
    match names.as_slice() {
        [] => list_branches(store, &here, verbose > 0, scope(all, remotes)),
        _ if list => bail!("`pyn branch --list` takes no branch names"),
        [new] => create(store, ws, new, None),
        [new, start] => create(store, ws, new, Some(start)),
        _ => crate::usage_error("`pyn branch` takes `<name> [<start>]`"),
    }
}

fn scope(all: bool, remotes: bool) -> Scope {
    match (all, remotes) {
        (true, _) => Scope::All,
        (_, true) => Scope::Remote,
        _ => Scope::Local,
    }
}

fn list_branches(store: &LocalStore, here: &str, verbose: bool, scope: Scope) -> Result<()> {
    let mut rows: Vec<(char, String, ObjectId, Option<usize>)> = Vec::new();
    if scope != Scope::Remote {
        if let Some(tip) = store.tip(DETACHED)?.filter(|_| here == DETACHED) {
            rows.push((
                '*',
                format!("(HEAD detached at {})", short(&tip.to_hex())),
                tip,
                None,
            ));
        }
        for (name, tip) in store.heads()? {
            let ahead = match store.upstream(&name)? {
                Some(_) => Some(store.unpublished(&name)?.ahead),
                None => None,
            };
            let mark = if name == here { '*' } else { ' ' };
            rows.push((mark, name, tip, ahead));
        }
    }
    if scope != Scope::Local {
        let prefix = if scope == Scope::All {
            "remotes/origin"
        } else {
            "origin"
        };
        for (name, tip) in store.remote_heads()? {
            rows.push((' ', format!("{prefix}/{name}"), tip, None));
        }
    }
    let width = rows
        .iter()
        .map(|(_, n, _, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    for (mark, shown, tip, ahead) in rows {
        if !verbose {
            println!("{mark} {shown}");
            continue;
        }
        let note = match ahead {
            Some(n) if n > 0 => format!(" (local, {n} not yet published)"),
            _ => String::new(),
        };
        println!(
            "{mark} {shown:<width$}  {}  {}{note}",
            short(&tip.to_hex()),
            subject(store, &tip)?
        );
    }
    Ok(())
}

fn create(store: &LocalStore, ws: &Workspace, new: &str, start: Option<&str>) -> Result<()> {
    let (at, tracks) = start_point(store, ws, start)?;
    store.create_branch(new, &at)?;
    if let Some(up) = tracks {
        store.set_upstream(new, Some(&up))?;
        eprintln!("branch '{new}' set up to track 'origin/{up}'.");
    }
    Ok(())
}

/// Where a new branch starts, and the remote branch it then follows when the start is a remote-tracking one.
fn start_point(
    store: &LocalStore,
    ws: &Workspace,
    start: Option<&str>,
) -> Result<(ObjectId, Option<String>)> {
    let here = branch::name(Some(ws));
    let Some(spec) = start else {
        let Some(tip) = store.tip(&here)? else {
            bail!("not a valid object name: '{here}'");
        };
        return Ok((tip, None));
    };
    let tracks = spec
        .strip_prefix("origin/")
        .filter(|n| {
            matches!(store.remote_tip(n), Ok(Some(_))) && store.tip(spec).ok().flatten().is_none()
        })
        .map(str::to_owned);
    Ok((resolve(store, ws, spec)?, tracks))
}

fn delete_branches(store: &LocalStore, here: &str, names: &[String], force: bool) -> Result<()> {
    for n in names {
        if n == here {
            bail!(
                "cannot delete branch '{n}': this workspace is on it; switch to another branch first"
            );
        }
        let Some(tip) = store.tip(n)?.filter(|_| n != DETACHED) else {
            bail!("branch '{n}' not found");
        };
        if !force && !merged(store, n, &tip, here)? {
            bail!(
                "the branch '{n}' is not fully merged.\n\
                 hint: If you are sure you want to delete it, run `pyn branch -D {n}`"
            );
        }
        store.delete_branch(n)?;
        println!("Deleted branch {n} (was {}).", short(&tip.to_hex()));
    }
    Ok(())
}

/// Whether `tip` is part of the upstream's history or of the branch the workspace is on, as git checks.
fn merged(store: &LocalStore, name: &str, tip: &ObjectId, here: &str) -> Result<bool> {
    for base in [store.published(name)?, store.tip(here)?]
        .into_iter()
        .flatten()
    {
        if store.ancestors(Some(base))?.contains(tip) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn upstream(store: &LocalStore, here: &str, names: &[String], to: Option<&str>) -> Result<()> {
    let name = match names {
        [] => here,
        [one] => one.as_str(),
        _ => bail!("too many arguments to set new upstream"),
    };
    if store.tip(name)?.is_none() {
        bail!("branch '{name}' does not exist");
    }
    match to {
        Some(up) => {
            let remote = up.strip_prefix("origin/").unwrap_or(up);
            if store.remote_tip(remote)?.is_none() {
                bail!("the requested upstream branch '{up}' does not exist");
            }
            store.set_upstream(name, Some(remote))?;
            println!("branch '{name}' set up to track 'origin/{remote}'.");
        }
        None => {
            if store.upstream(name)?.is_none() {
                bail!("branch '{name}' has no upstream information");
            }
            store.set_upstream(name, None)?;
        }
    }
    Ok(())
}

/// How a switch names its target.
pub(crate) enum Target {
    /// An existing local branch.
    Local(ObjectId),
    /// A remote-tracking branch that a new local branch will follow.
    Remote(ObjectId),
    /// A commit to leave HEAD on, with no branch.
    Detached(ObjectId),
}

/// The local branch `name`, else the one remote-tracking branch of that name (fetched first if the store lacks it).
pub(crate) fn find_target(
    backend: &dyn Backend,
    api: &Api,
    ws: &Workspace,
    store: &LocalStore,
    name: &str,
) -> Result<Option<Target>> {
    if name == DETACHED {
        return Ok(None);
    }
    if let Some(tip) = store.tip(name)? {
        return Ok(Some(Target::Local(tip)));
    }
    if store.remote_matches(name)?.is_empty() && branch::find(api, name).ok().flatten().is_some() {
        let current = branch::name(Some(ws));
        store.fetch(backend, &[name.to_string()], &current)?;
    }
    Ok(store.remote_tip(name)?.map(Target::Remote))
}

/// The files of `tip` as snapshot entries: revisions and modes come from the server for what it knows; unpublished
/// content has revision `0` and the mode the policy file at `tip` gives it.
fn entries_at(
    backend: &dyn Backend,
    store: &LocalStore,
    tip: &ObjectId,
) -> Result<Vec<api::SnapshotEntry>> {
    let remotes: Vec<ObjectId> = store
        .remote_heads()?
        .into_iter()
        .map(|(_, id)| id)
        .collect();
    let known = store.reach(remotes)?;
    let (mut at, mut base) = (Some(*tip), None);
    while let Some(id) = at {
        if known.contains(&id) {
            base = Some(id);
            break;
        }
        at = store.repo().commit(&id)?.parents.first().copied();
    }
    let server: BTreeMap<String, api::SnapshotEntry> = match base {
        Some(b) => backend
            .snapshot(Some(&b.to_hex()))
            .map(|(_, entries)| entries)
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.path.clone(), e))
            .collect(),
        None => BTreeMap::new(),
    };
    let tree = store.tree_of(Some(*tip))?;
    let files = store.repo().flatten(tree.as_ref(), &Pathspec::default())?;
    let policy = match files.get(crate::policy::POLICY_PATH.as_bytes()) {
        Some(e) => store
            .repo()
            .blob(&e.id)
            .ok()
            .and_then(|b| crate::policy::Policy::parse(&String::from_utf8_lossy(&b))),
        None => Some(crate::policy::Policy::none()),
    };
    let mut out = Vec::new();
    for (path, entry) in files {
        let path = String::from_utf8_lossy(&path).into_owned();
        let content = entry.id.to_hex();
        let known = server.get(&path);
        let (revision, mode) = match (known, &policy) {
            (Some(s), _) if s.content == content => (s.revision, s.mode),
            (_, Some(p)) => (0, p.mode_for(&path)),
            (Some(s), None) => (0, s.mode),
            (None, None) => (0, api::Mode::Shared),
        };
        out.push(api::SnapshotEntry {
            path,
            mode,
            revision,
            content,
            file_mode: local_sync::api_mode(entry.mode)?,
        });
    }
    Ok(out)
}

pub(crate) fn go(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    target: (&str, &Target),
    (force, created): (bool, bool),
) -> Result<()> {
    let (name, found) = target;
    merging::refuse_during_merge(ws, "switch")?;
    let already = branch::name(Some(ws)) == name && matches!(found, Target::Local(_));
    let (Target::Local(tip) | Target::Remote(tip) | Target::Detached(tip)) = found;
    let left = if branch::name(Some(ws)) == DETACHED && !matches!(found, Target::Detached(_)) {
        store.tip(DETACHED)?
    } else {
        None
    };
    let snap = entries_at(backend, store, tip)?;
    let mut state = ws.load_state()?;
    let mut index = Index::load(ws)?;
    let (steps, conflicts) = branch::plan(ws, &state, &index, &snap)?;
    if !conflicts.is_empty() && !force {
        let mut message = String::from("local changes would be overwritten by the switch:\n");
        for (path, why) in &conflicts {
            message.push_str(&format!("\t{path} ({why})\n"));
        }
        message.push_str(
            "commit them, put them back with `pyn checkout -- <path>`, or discard them with --force; nothing was changed",
        );
        bail!(message);
    }
    if let Target::Remote(_) = found {
        store.create_branch(name, tip)?;
        store.set_upstream(name, Some(name))?;
        eprintln!("branch '{name}' set up to track 'origin/{name}'.");
    }
    let locks: Vec<api::Lock> = backend
        .pyn_server("switch")
        .and_then(|api| api.send(api.get(&api.repo_route("/locks")?))?.json())
        .unwrap_or_default();
    let me = backend.whoami().unwrap_or_default();
    branch::carry_staged(&mut index, &steps);
    let applied = apply(backend, ws, &mut state, steps, &locks, &me);
    ws.save_state(&state)?;
    applied?;
    index.save(ws)?;
    if let Target::Detached(_) = found {
        store.set_detached(tip)?;
    }
    branch::set_current(ws, name)?;
    store
        .repo()
        .refs()
        .set_symbolic("HEAD", &format!("refs/heads/{name}"))?;
    if let Some(old) = left {
        warn_left_behind(store, &old)?;
        store.clear_detached()?;
    }
    if let Target::Detached(_) = found {
        eprintln!(
            "HEAD is now at {} {}\nYou are in 'detached HEAD' state; commits made here belong to no branch. \
             Keep them with `pyn switch -c <new-branch>`.",
            short(&tip.to_hex()),
            subject(store, tip)?
        );
    } else if already {
        eprintln!("Already on '{name}'");
    } else if created || matches!(found, Target::Remote(_)) {
        eprintln!("Switched to a new branch '{name}'");
    } else {
        eprintln!("Switched to branch '{name}'");
    }
    Ok(())
}

/// Says so when the commits of a detached HEAD being left are on no branch.
fn warn_left_behind(store: &LocalStore, old: &ObjectId) -> Result<()> {
    let tips = store
        .heads()?
        .into_iter()
        .chain(store.remote_heads()?)
        .map(|(_, id)| id)
        .collect();
    let kept = store.reach(tips)?;
    let behind = store.ancestors(Some(*old))?.difference(&kept).count();
    if behind > 0 {
        eprintln!(
            "Warning: you are leaving {behind} commit(s) behind, not connected to any of your branches.\n\
             To keep them, create a branch: pyn branch <new-branch> {}",
            short(&old.to_hex())
        );
    }
    Ok(())
}

fn apply(
    backend: &dyn Backend,
    ws: &Workspace,
    state: &mut BTreeMap<String, FileState>,
    steps: BTreeMap<String, Step>,
    locks: &[api::Lock],
    me: &str,
) -> Result<()> {
    for (path, step) in steps {
        match step {
            Step::Adopt(w) => {
                state.insert(path, branch::state_of(&w));
            }
            Step::Remove => {
                let target = ws.dest(&path)?;
                set_read_only(&target, false)?;
                let _ = std::fs::remove_file(&target);
                branch::remove_empty_parents(ws, &path);
                state.remove(&path);
            }
            Step::Fetch(w) => {
                let bytes = backend.blob(&w.content, None)?;
                let target = ws.dest_with_parents(&path)?;
                set_read_only(&target, false)?;
                ws.write_file(&path, &bytes)?;
                localfile::apply(&target, &path, w.file_mode)?;
                let entry = branch::file_entry(&w, locks);
                set_read_only(&target, sync::wants_read_only(&entry, me))?;
                state.insert(
                    path,
                    FileState {
                        revision: w.revision,
                        hash: w.content.clone(),
                        legacy: false,
                        mode: mode_name(w.mode).to_string(),
                        file_mode: w.file_mode,
                    },
                );
            }
        }
    }
    Ok(())
}

/// `pyn switch --detach` and `pyn checkout <commit>`: HEAD moves to the commit `spec` names, on no branch.
pub(crate) fn detach(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    spec: &str,
    force: bool,
) -> Result<()> {
    let tip = resolve(store, ws, spec)?;
    go(
        backend,
        ws,
        store,
        (DETACHED, &Target::Detached(tip)),
        (force, false),
    )
}

/// `pyn switch` and `pyn checkout <branch>`: `create` makes the branch first, at `start`.
pub(crate) fn create_and_go(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    (new, start): (&str, Option<&str>),
    force: bool,
) -> Result<()> {
    let (at, tracks) = start_point(store, ws, start)?;
    if store.tip(new)?.is_some() {
        bail!("a branch named '{new}' already exists");
    }
    pyn_git::check_ref_name(&format!("refs/heads/{new}"))
        .map_err(|_| anyhow::anyhow!("'{new}' is not a valid branch name"))?;
    let snap = entries_at(backend, store, &at)?;
    let state = ws.load_state()?;
    let index = Index::load(ws)?;
    let (_, conflicts) = branch::plan(ws, &state, &index, &snap)?;
    if !conflicts.is_empty() && !force {
        bail!(
            "local changes would be overwritten by the switch; commit them or discard them with --force; nothing was changed"
        );
    }
    store.create_branch(new, &at)?;
    if let Some(up) = tracks {
        store.set_upstream(new, Some(&up))?;
        eprintln!("branch '{new}' set up to track 'origin/{up}'.");
    }
    let tip = store.tip(new)?.expect("the branch was just created");
    go(
        backend,
        ws,
        store,
        (new, &Target::Local(tip)),
        (force, true),
    )?;
    Ok(())
}

//! A `pyn pull` that stopped on conflicts in a workspace with local commits (docs/concepts/commits.md): the merge or
//! rebase in progress, and how it is concluded or given up. It shares the record, the markers and the commands of
//! `pyn merge` (docs/concepts/merging.md); the server is not involved until the result is pushed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result, bail};
use pyn_git::Expect;
use pyn_gitwork::{Entry, Pathspec};
use pyn_object::{Commit, Kind, ObjectId, Signature};
use pyn_proto as api;
use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::git_home::clean_message;
use crate::local_commit::signature;
use crate::local_store::{ALGORITHM, Files, LocalStore, build_trees, file_mode, parse_id};
use crate::local_sync::{
    Conflict, Joined, Judge, Rejected, Step, api_mode, dirty_paths, local_ref, my_locks,
};
use crate::merging::{self, MergeState, STATE_VERSION, Touched};
use crate::snapshot::short;
use crate::staging::{self, Index};
use crate::workspace::{FileState, Workspace, hash_of, set_read_only};
use crate::{localfile, sync};

/// What a pull that has stopped can be told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Abort,
    Continue,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Merge,
    Rebase,
}

/// One side of a conflicted path: a blob of the local store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    id: String,
    mode: api::FileMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ours: Option<Side>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theirs: Option<Side>,
    #[serde(default)]
    exclusive: bool,
}

/// The part of the merge record that only a pull knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOp {
    op: Op,
    /// The local tip when the pull started.
    orig: String,
    /// The fetched tip.
    theirs: String,
    /// The merged tree with a placeholder for each conflicted path: what the working tree holds apart from them.
    result_tree: String,
    #[serde(default)]
    sides: BTreeMap<String, Sides>,
    /// Rebase: the tip of the replayed commits so far, the commit being replayed and those still to come.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<String>,
    #[serde(default)]
    todo: Vec<String>,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    dropped: usize,
}

impl LocalOp {
    pub fn name(&self) -> &'static str {
        match self.op {
            Op::Merge => "pull",
            Op::Rebase => "rebase",
        }
    }

    pub fn how_to_end(&self) -> String {
        match self.op {
            Op::Merge => "conclude it with `pyn commit` or give it up with `pyn pull --abort`".into(),
            Op::Rebase => {
                "conclude this step with `pyn commit` (or `pyn pull --continue`), drop the commit with `pyn pull --skip`, \
                 or give the rebase up with `pyn pull --abort`"
                    .into()
            }
        }
    }

    pub fn header(&self) -> String {
        match self.op {
            Op::Merge => "  (use \"pyn pull --abort\" to abort the merge)\n".into(),
            Op::Rebase => {
                let at = self.total - self.todo.len();
                format!(
                    "You are rebasing your local commits (commit {at} of {}, {} left).\n  \
                     (use \"pyn pull --skip\" to drop this commit, \"pyn pull --abort\" to give up the rebase)\n",
                    self.total,
                    self.todo.len()
                )
            }
        }
    }
}

fn id_of(hex: &str) -> Result<ObjectId> {
    parse_id(hex)
}

fn flat(store: &LocalStore, commit: Option<ObjectId>) -> Result<Files> {
    let tree = store.tree_of(commit)?;
    Ok(store
        .repo()
        .flatten(tree.as_ref(), &Pathspec::default())?
        .into_iter()
        .collect())
}

fn flat_tree(store: &LocalStore, tree: &ObjectId) -> Result<Files> {
    Ok(store
        .repo()
        .flatten(Some(tree), &Pathspec::default())?
        .into_iter()
        .collect())
}

fn side(entry: Option<Entry>) -> Result<Option<Side>> {
    entry
        .map(|e| {
            Ok(Side {
                id: e.id.to_hex(),
                mode: api_mode(e.mode)?,
            })
        })
        .transpose()
}

fn name(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn write_trees(store: &LocalStore, files: &Files) -> Result<ObjectId> {
    let (tree, trees) = build_trees(files)?;
    for (_, body) in trees {
        store.repo().objects().write(Kind::Tree, &body)?;
    }
    Ok(tree)
}

/// Brings the working tree from `from` to `to`, with `working` bytes where given; only `force` paths may overwrite
/// uncommitted changes and `editable` ones stay writable. Returns the paths written.
fn materialize(
    store: &LocalStore,
    remote: &dyn Backend,
    ws: &Workspace,
    (from, to): (&Files, &Files),
    working: &BTreeMap<Vec<u8>, Vec<u8>>,
    (force, editable): (&BTreeSet<String>, &BTreeSet<String>),
) -> Result<BTreeSet<String>> {
    let mut keys: BTreeSet<Vec<u8>> = from
        .keys()
        .chain(to.keys())
        .filter(|k| from.get(*k) != to.get(*k))
        .cloned()
        .collect();
    keys.extend(working.keys().cloned());
    keys.extend(
        force
            .iter()
            .map(|n| n.clone().into_bytes())
            .filter(|k| from.contains_key(k) || to.contains_key(k)),
    );
    let names: Vec<String> = keys.iter().map(|k| name(k)).collect();
    ws.check_paths(&names)?;
    let mut state = ws.load_state()?;
    let dirty = dirty_paths(ws)?;
    let blocked: Vec<&String> = names
        .iter()
        .zip(&keys)
        .filter(|(n, _)| !force.contains(*n))
        .filter(|(n, k)| {
            dirty.contains(*n)
                || (!state.contains_key(*n) && to.contains_key(*k) && localfile::exists(&ws.abs(n)))
        })
        .map(|(n, _)| n)
        .collect();
    if !blocked.is_empty() {
        return Err(Rejected(format!(
            "error: your local changes to the following files would be overwritten:\n\t{}\n\
             commit them, or put them back with `pyn checkout -- <path>`, and run the command again; nothing was changed",
            blocked
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join("\n\t")
        ))
        .into());
    }
    let mut written = BTreeSet::new();
    for (name, key) in names.iter().zip(&keys) {
        let abs = ws.abs(name);
        let was_read_only = localfile::exists(&abs)
            && std::fs::metadata(&abs).is_ok_and(|m| m.permissions().readonly());
        match to.get(key) {
            Some(entry) => {
                let bytes = match working.get(key) {
                    Some(b) => b.clone(),
                    None => store.blob(remote, &entry.id.to_hex())?,
                };
                set_read_only(&abs, false)?;
                ws.write_file(name, &bytes)?;
                let mode = api_mode(entry.mode)?;
                localfile::apply(&abs, name, mode)?;
                if was_read_only && !editable.contains(name) {
                    set_read_only(&abs, true)?;
                }
                let hash = hash_of(&bytes);
                match state.get_mut(name) {
                    Some(known) => {
                        if !working.contains_key(key) {
                            known.hash = hash;
                            known.legacy = false;
                        }
                        known.file_mode = mode;
                    }
                    None => {
                        state.insert(
                            name.clone(),
                            FileState {
                                revision: 0,
                                hash,
                                legacy: false,
                                mode: "shared".into(),
                                file_mode: mode,
                            },
                        );
                    }
                }
            }
            None => {
                set_read_only(&abs, false)?;
                let _ = std::fs::remove_file(&abs);
                state.remove(name);
            }
        }
        written.insert(name.clone());
    }
    ws.save_state(&state)?;
    Ok(written)
}

/// What a stopped merge needs to be recorded: the commits it joins and the message of the merge commit.
pub struct Stop {
    pub orig: ObjectId,
    pub theirs: ObjectId,
    pub message: String,
}

fn conflict_lines(conflicts: &BTreeMap<Vec<u8>, Conflict>, kept: &[String]) -> (String, bool) {
    let mut text = String::new();
    for path in kept {
        text.push_str(&format!("Kept your version of {path}: you hold its lock\n"));
    }
    let mut exclusive = false;
    for (path, c) in conflicts {
        let path = name(path);
        text.push_str(&if c.exclusive {
            exclusive = true;
            format!("CONFLICT (exclusive): {path} is exclusive and changed on both sides\n")
        } else if c.ours.is_none() || c.theirs.is_none() {
            format!("CONFLICT (modify/delete): {path} was removed on one side and changed on the other\n")
        } else if c.working.is_some() {
            format!("CONFLICT (content): Merge conflict in {path}\n")
        } else {
            format!("CONFLICT (binary or file mode): {path} cannot be merged; the working tree has your side\n")
        });
    }
    (text, exclusive)
}

const RESOLVE: &str = "fix conflicts, stage each with `pyn add <path>` (a file you deleted is staged the same way), then run `pyn commit`";
const SIDES: &str = "hint: To keep the version of a file on one side, use `pyn checkout --ours|--theirs -- <path>`.";
const LOCKS: &str = "hint: An exclusive file needs its lock to be concluded: choose a side, then `pyn lock <path>`.";

fn sides_of(conflicts: &BTreeMap<Vec<u8>, Conflict>) -> Result<BTreeMap<String, Sides>> {
    conflicts
        .iter()
        .map(|(p, c)| {
            Ok((
                name(p),
                Sides {
                    ours: side(c.ours)?,
                    theirs: side(c.theirs)?,
                    exclusive: c.exclusive,
                },
            ))
        })
        .collect()
}

/// Writes a stopped merge or replay into the working tree and records it; the caller reports it.
#[allow(clippy::too_many_arguments)]
fn record(
    store: &LocalStore,
    backend: &dyn Backend,
    ws: &Workspace,
    branch: &str,
    disk: &Files,
    files: &Files,
    conflicts: &BTreeMap<Vec<u8>, Conflict>,
    op: LocalOp,
    message: String,
) -> Result<()> {
    let working: BTreeMap<Vec<u8>, Vec<u8>> = conflicts
        .iter()
        .filter_map(|(p, c)| c.working.clone().map(|w| (p.clone(), w)))
        .collect();
    let editable: BTreeSet<String> = conflicts.keys().map(|p| name(p)).collect();
    let written = materialize(
        store,
        backend,
        ws,
        (disk, files),
        &working,
        (&BTreeSet::new(), &editable),
    )?;
    let mut paths = BTreeMap::new();
    for path in written.iter().chain(editable.iter()) {
        let c = conflicts.get(path.as_bytes());
        paths.insert(
            path.clone(),
            Touched {
                mode: if c.is_some_and(|c| c.exclusive) {
                    "exclusive".into()
                } else {
                    "shared".into()
                },
                conflict: c.map(|c| c.label.to_string()),
                ours: None,
                theirs: None,
                line_head: None,
            },
        );
    }
    Index::default().save(ws)?;
    merging::save(
        ws,
        &MergeState {
            version: STATE_VERSION,
            branch: branch.to_string(),
            source: format!("origin/{branch}"),
            source_commit: op.theirs.clone(),
            target_commit: op.orig.clone(),
            message,
            no_ff: false,
            paths,
            local: Some(op),
        },
    )
}

/// Leaves a merge of the local branch and the fetched commits in progress, with markers in the conflicted files.
pub fn stop_merge(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    branch: &str,
    stop: Stop,
    joined: Joined,
) -> Result<()> {
    let result_tree = write_trees(store, &joined.files)?;
    let op = LocalOp {
        op: Op::Merge,
        orig: stop.orig.to_hex(),
        theirs: stop.theirs.to_hex(),
        result_tree: result_tree.to_hex(),
        sides: sides_of(&joined.conflicts)?,
        tip: None,
        current: None,
        todo: Vec::new(),
        total: 0,
        dropped: 0,
    };
    let disk = flat(store, Some(stop.orig))?;
    record(
        store,
        backend,
        ws,
        branch,
        &disk,
        &joined.files,
        &joined.conflicts,
        op,
        stop.message,
    )?;
    let (mut text, exclusive) = conflict_lines(&joined.conflicts, &joined.kept);
    text.push_str(&format!("Automatic merge failed; {RESOLVE}.\n{SIDES}\n"));
    if exclusive {
        text.push_str(&format!("{LOCKS}\n"));
    }
    text.push_str("hint: To put everything back as it was, run `pyn pull --abort`.");
    Err(Rejected(text).into())
}

/// The local commits being replayed onto the fetched tip.
struct Run {
    orig: ObjectId,
    onto: ObjectId,
    tip: ObjectId,
    todo: VecDeque<ObjectId>,
    total: usize,
    dropped: usize,
}

enum Replayed {
    Done(Run),
    Stopped {
        run: Run,
        current: ObjectId,
        files: Files,
        conflicts: BTreeMap<Vec<u8>, Conflict>,
        kept: Vec<String>,
    },
}

fn subject(message: &[u8]) -> String {
    String::from_utf8_lossy(message)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Replays the commits of `run.todo` over its tip, stopping at the first that has a conflict.
fn replay(
    store: &LocalStore,
    backend: &dyn Backend,
    who: &Signature,
    mut run: Run,
) -> Result<Replayed> {
    let repo = store.repo();
    let mut judge = Judge::new(backend, run.onto);
    let mut files = flat(store, Some(run.tip))?;
    let mut parent_tree = repo.commit(&run.tip)?.tree;
    while let Some(id) = run.todo.pop_front() {
        let commit = repo.commit(&id)?;
        let before = store.tree_of(commit.parents.first().copied())?;
        let theirs_label = format!("{} ({})", short(&id.to_hex()), subject(&commit.message));
        let (mut conflicts, mut kept) = (BTreeMap::new(), Vec::new());
        for change in repo.diff_trees(before.as_ref(), Some(&commit.tree), &Pathspec::default())? {
            let ours = files.get(&change.path).copied();
            let sides = (change.old, ours, change.new);
            match store.join_path(
                backend,
                &mut judge,
                &change.path,
                sides,
                true,
                ("HEAD", &theirs_label),
            )? {
                Step::Take(entry) => set_entry(&mut files, &change.path, entry),
                Step::Kept(entry) => {
                    set_entry(&mut files, &change.path, entry);
                    kept.push(name(&change.path));
                }
                Step::Conflict(c) => {
                    set_entry(&mut files, &change.path, c.placeholder);
                    conflicts.insert(change.path.clone(), c);
                }
            }
        }
        if !conflicts.is_empty() {
            return Ok(Replayed::Stopped {
                run,
                current: id,
                files,
                conflicts,
                kept,
            });
        }
        for path in kept {
            println!("Kept your version of {path}: you hold its lock");
        }
        let tree = write_trees(store, &files)?;
        if tree == parent_tree {
            run.dropped += 1;
            continue;
        }
        let record = Commit {
            tree,
            parents: vec![run.tip],
            author: commit.author,
            committer: who.clone(),
            extra_headers: Vec::new(),
            message: commit.message,
        };
        run.tip = repo
            .objects()
            .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
        parent_tree = tree;
    }
    Ok(Replayed::Done(run))
}

fn set_entry(files: &mut Files, path: &[u8], entry: Option<Entry>) {
    match entry {
        Some(entry) => files.insert(path.to_vec(), entry),
        None => files.remove(path),
    };
}

/// Replays the local commits onto the fetched tip; a conflict leaves the rebase in progress.
pub fn begin_rebase(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    branch: &str,
    outgoing: crate::local_sync::Outgoing,
    onto: ObjectId,
    who: &Signature,
) -> Result<()> {
    for id in &outgoing.commits {
        if store.repo().commit(id)?.parents.len() > 1 {
            return Err(Rejected(format!(
                "local commit {} is a merge commit, which cannot be rebased; use `pyn pull` to merge\nnothing was changed",
                short(&id.to_hex())
            ))
            .into());
        }
    }
    let orig = *outgoing.commits.last().expect("outgoing has commits");
    let run = Run {
        orig,
        onto,
        tip: onto,
        total: outgoing.commits.len(),
        todo: outgoing.commits.into(),
        dropped: 0,
    };
    let disk = flat(store, Some(orig))?;
    proceed(
        backend,
        ws,
        store,
        branch,
        who,
        run,
        &disk,
        &BTreeSet::new(),
    )
}

/// Carries a rebase on from `run` with `disk` in the working tree: to its end, or to the next conflict.
#[allow(clippy::too_many_arguments)]
fn proceed(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    branch: &str,
    who: &Signature,
    run: Run,
    disk: &Files,
    force: &BTreeSet<String>,
) -> Result<()> {
    match replay(store, backend, who, run)? {
        Replayed::Done(run) => {
            let end = flat(store, Some(run.tip))?;
            materialize(
                store,
                backend,
                ws,
                (disk, &end),
                &BTreeMap::new(),
                (force, &BTreeSet::new()),
            )?;
            store
                .repo()
                .refs()
                .update_checked(&local_ref(branch), Expect::Id(run.orig), &run.tip)
                .context("the local branch moved while pulling; run the command again")?;
            merging::clear(ws)?;
            Index::default().save(ws)?;
            println!(
                "Rebased {} local commit(s) onto {}{}",
                run.total - run.dropped,
                short(&run.onto.to_hex()),
                if run.dropped > 0 {
                    format!(" ({} became empty and was dropped)", run.dropped)
                } else {
                    String::new()
                }
            );
            sync::update(backend, ws, &[], None)
        }
        Replayed::Stopped {
            run,
            current,
            files,
            conflicts,
            kept,
        } => {
            let commit = store.repo().commit(&current)?;
            let result_tree = write_trees(store, &files)?;
            let op = LocalOp {
                op: Op::Rebase,
                orig: run.orig.to_hex(),
                theirs: run.onto.to_hex(),
                result_tree: result_tree.to_hex(),
                sides: sides_of(&conflicts)?,
                tip: Some(run.tip.to_hex()),
                current: Some(current.to_hex()),
                todo: run.todo.iter().map(ObjectId::to_hex).collect(),
                total: run.total,
                dropped: run.dropped,
            };
            let message = String::from_utf8_lossy(&commit.message).into_owned();
            let at = run.total - run.todo.len();
            let (mut text, exclusive) = conflict_lines(&conflicts, &kept);
            text.push_str(&format!(
                "error: could not apply {} ({}), commit {at} of {}\n{RESOLVE} (or `pyn pull --continue`).\n{SIDES}\n",
                short(&current.to_hex()),
                subject(&commit.message),
                run.total
            ));
            if exclusive {
                text.push_str(&format!("{LOCKS}\n"));
            }
            text.push_str(
                "hint: To drop this commit, run `pyn pull --skip`; to put everything back as it was, `pyn pull --abort`.",
            );
            record(
                store, backend, ws, branch, disk, &files, &conflicts, op, message,
            )?;
            Err(Rejected(text).into())
        }
    }
}

fn open(ws: &Workspace) -> Result<(LocalStore, MergeState, LocalOp)> {
    let Some(mut state) = merging::load(ws)? else {
        bail!("there is no pull in progress");
    };
    let Some(op) = state.local.take() else {
        bail!(
            "the merge in progress was not made by `pyn pull`; use `pyn merge --abort` or `pyn commit`"
        );
    };
    let store = LocalStore::open(ws).context("the local store is missing")?;
    Ok((store, state, op))
}

/// Records the staged resolutions as what the working tree holds.
fn adopt(ws: &Workspace, index: &Index) -> Result<()> {
    let mut state = ws.load_state()?;
    for (path, staged) in &index.entries {
        if staged.deleted {
            state.remove(path);
        } else if let Some(known) = state.get_mut(path) {
            known.hash = staged.hash.clone();
            known.legacy = false;
            known.file_mode = staged.file_mode;
        }
    }
    ws.save_state(&state)
}

/// Concludes the pull in progress with the staged resolutions: the merge commit, or the replayed commit and the rest.
pub fn finish(backend: &dyn Backend, ws: &Workspace, message: Option<String>) -> Result<()> {
    let (store, state, op) = open(ws)?;
    let index = Index::load(ws)?;
    merging::check_resolved(&state, &index)?;
    let exclusive: Vec<&String> = op
        .sides
        .iter()
        .filter(|(p, s)| s.exclusive && state.paths.contains_key(*p))
        .map(|(p, _)| p)
        .collect();
    if !exclusive.is_empty() {
        let mine = my_locks(backend)?;
        let missing: Vec<String> = exclusive
            .iter()
            .filter(|p| !mine.contains(**p))
            .map(|p| format!("\t`pyn lock {p}`"))
            .collect();
        if !missing.is_empty() {
            bail!(
                "an exclusive file that both sides changed needs your lock to be concluded; take it with:\n{}\nthen commit again",
                missing.join("\n")
            );
        }
    }
    let mut files = flat_tree(&store, &id_of(&op.result_tree)?)?;
    for (path, staged) in &index.entries {
        if staged.deleted {
            files.remove(path.as_bytes());
            continue;
        }
        let bytes = std::fs::read(staging::content_path(ws, &staged.hash))
            .with_context(|| format!("reading the staged content of {path}"))?;
        let id = store.repo().objects().write(Kind::Blob, &bytes)?;
        files.insert(
            path.clone().into_bytes(),
            Entry {
                mode: file_mode(staged.file_mode),
                id,
            },
        );
    }
    let tree = write_trees(&store, &files)?;
    let who = signature(&backend.whoami()?);
    let (orig, theirs) = (id_of(&op.orig)?, id_of(&op.theirs)?);
    let repo = store.repo();
    match op.op {
        Op::Merge => {
            let record = Commit {
                tree,
                parents: vec![orig, theirs],
                author: who.clone(),
                committer: who,
                extra_headers: Vec::new(),
                message: clean_message(&[message.unwrap_or(state.message)]),
            };
            let tip = repo
                .objects()
                .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
            repo.refs()
                .update_checked(&local_ref(&state.branch), Expect::Id(orig), &tip)
                .context("the local branch moved while pulling; run the command again")?;
            adopt(ws, &index)?;
            Index::default().save(ws)?;
            merging::clear(ws)?;
            println!("Merge made by the 'ort' strategy.");
            sync::update(backend, ws, &[], None)
        }
        Op::Rebase => {
            let current = id_of(
                op.current
                    .as_deref()
                    .context("the rebase has no current commit")?,
            )?;
            let picked = repo.commit(&current)?;
            let tip = id_of(op.tip.as_deref().context("the rebase has no tip")?)?;
            let mut run = Run {
                orig,
                onto: theirs,
                tip,
                todo: op.todo.iter().map(|t| id_of(t)).collect::<Result<_>>()?,
                total: op.total,
                dropped: op.dropped,
            };
            if tree == repo.commit(&tip)?.tree {
                run.dropped += 1;
            } else {
                let record = Commit {
                    tree,
                    parents: vec![tip],
                    author: picked.author,
                    committer: who.clone(),
                    extra_headers: Vec::new(),
                    message: message.map_or(picked.message, |m| clean_message(&[m])),
                };
                run.tip = repo
                    .objects()
                    .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
            }
            adopt(ws, &index)?;
            proceed(
                backend,
                ws,
                &store,
                &state.branch,
                &who,
                run,
                &files,
                &BTreeSet::new(),
            )
        }
    }
}

/// Drops the commit a rebase stopped on and goes on with the next.
pub fn skip(backend: &dyn Backend, ws: &Workspace) -> Result<()> {
    let (store, state, op) = open(ws)?;
    if op.op != Op::Rebase {
        bail!("`pyn pull --skip` drops a commit of a rebase; there is none in progress");
    }
    let (orig, onto) = (id_of(&op.orig)?, id_of(&op.theirs)?);
    let tip = id_of(op.tip.as_deref().context("the rebase has no tip")?)?;
    let disk = flat_tree(&store, &id_of(&op.result_tree)?)?;
    let back = flat(&store, Some(tip))?;
    let touched: BTreeSet<String> = state.paths.keys().cloned().collect();
    materialize(
        &store,
        backend,
        ws,
        (&disk, &back),
        &BTreeMap::new(),
        (&touched, &BTreeSet::new()),
    )?;
    let mut index = Index::load(ws)?;
    index.entries.retain(|p, _| !touched.contains(p));
    index.save(ws)?;
    let run = Run {
        orig,
        onto,
        tip,
        todo: op.todo.iter().map(|t| id_of(t)).collect::<Result<_>>()?,
        total: op.total,
        dropped: op.dropped + 1,
    };
    let who = signature(&backend.whoami()?);
    proceed(
        backend,
        ws,
        &store,
        &state.branch,
        &who,
        run,
        &back,
        &BTreeSet::new(),
    )
}

/// Gives the pull up: the working tree is what it was, and nothing in the local store or on the server changed.
pub fn abort(backend: &dyn Backend, ws: &Workspace) -> Result<()> {
    let (store, state, op) = open(ws)?;
    let disk = flat_tree(&store, &id_of(&op.result_tree)?)?;
    let back = flat(&store, Some(id_of(&op.orig)?))?;
    let touched: BTreeSet<String> = state.paths.keys().cloned().collect();
    materialize(
        &store,
        backend,
        ws,
        (&disk, &back),
        &BTreeMap::new(),
        (&touched, &BTreeSet::new()),
    )?;
    let mut index = Index::load(ws)?;
    index.entries.retain(|p, _| !touched.contains(p));
    index.save(ws)?;
    merging::clear(ws)?;
    println!(
        "The {} was given up; your branch is back at {}",
        op.name(),
        short(&op.orig)
    );
    Ok(())
}

/// Takes one side of a conflicted path into the working tree, as `pyn checkout --ours|--theirs -- <path>`; in a
/// rebase `--ours` is the fetched side and `--theirs` the commit being replayed.
pub fn take_side(
    backend: &dyn Backend,
    ws: &Workspace,
    theirs: bool,
    paths: &[String],
) -> Result<()> {
    let (store, state, op) = open(ws)?;
    for path in paths {
        if !state.paths.get(path).is_some_and(|t| t.conflict.is_some()) {
            bail!("{path} is not unmerged");
        }
        let sides = op
            .sides
            .get(path)
            .context("the pull recorded no sides of this path")?;
        let Some(side) = (if theirs { &sides.theirs } else { &sides.ours }) else {
            bail!("{path} does not exist on that side of the merge");
        };
        let bytes = store.blob(backend, &side.id)?;
        let abs = ws.abs(path);
        set_read_only(&abs, false)?;
        ws.write_file(path, &bytes)?;
        localfile::apply(&abs, path, side.mode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips_through_toml() {
        let mut sides = BTreeMap::new();
        sides.insert(
            "Content/m.umap".to_string(),
            Sides {
                ours: Some(Side {
                    id: "ab".repeat(32),
                    mode: api::FileMode::Regular,
                }),
                theirs: None,
                exclusive: true,
            },
        );
        let op = LocalOp {
            op: Op::Rebase,
            orig: "cd".repeat(32),
            theirs: "ef".repeat(32),
            result_tree: "01".repeat(32),
            sides,
            tip: Some("23".repeat(32)),
            current: Some("45".repeat(32)),
            todo: vec!["67".repeat(32)],
            total: 3,
            dropped: 1,
        };
        let state = MergeState {
            version: STATE_VERSION,
            branch: "main".into(),
            source: "origin/main".into(),
            source_commit: op.theirs.clone(),
            target_commit: op.orig.clone(),
            message: "m".into(),
            no_ff: false,
            paths: BTreeMap::new(),
            local: Some(op.clone()),
        };
        let back: MergeState = toml::from_str(&toml::to_string_pretty(&state).unwrap()).unwrap();
        assert_eq!(back.local, Some(op));
    }

    #[test]
    fn the_header_counts_the_commits_of_a_rebase() {
        let op = LocalOp {
            op: Op::Rebase,
            orig: String::new(),
            theirs: String::new(),
            result_tree: String::new(),
            sides: BTreeMap::new(),
            tip: None,
            current: None,
            todo: vec!["a".into()],
            total: 3,
            dropped: 0,
        };
        assert!(op.header().contains("commit 2 of 3, 1 left"));
    }
}

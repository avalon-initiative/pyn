//! `fetch`, `pull` and `push` of a workspace with `local_store` on (docs/concepts/commits.md): commits and trees travel
//! git-encoded, every id is verified by hashing, and a push is accepted or refused as a whole.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pyn_git::Expect;
use pyn_gitwork::{Entry, Pathspec};
use pyn_merge::{Outcome, merge_text};
use pyn_object::{Commit, FileMode, Kind, ObjectId, hash_object, parse_tree};
use pyn_proto as api;

use crate::backend::Backend;
use crate::branch;
use crate::client::ApiError;
use crate::local_commit::signature;
use crate::local_store::{ALGORITHM, Files, LocalStore, build_trees, parse_id};
use crate::snapshot::short;
use crate::staging::Index;
use crate::stranded;
use crate::workspace::{Workspace, hash_of, set_read_only};
use crate::{localfile, merging, sync};

/// A refusal reported the way git reports one: the message, then exit status 1.
#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

pub struct Fetched {
    pub previous: Option<ObjectId>,
    pub head: Option<ObjectId>,
}

/// Local commits the server lacks, oldest first, and the commit they build on.
pub struct Outgoing {
    pub commits: Vec<ObjectId>,
    pub base: Option<ObjectId>,
}

fn tracking_ref(branch: &str) -> String {
    format!("refs/remotes/origin/{branch}")
}

fn local_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

fn decode(data: &str, what: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(data)
        .with_context(|| format!("{what} from the server is not valid base64"))
}

/// The body of an object the server sent, only if it hashes to the id it came with.
fn verified(kind: Kind, id: &str, data: &str) -> Result<(ObjectId, Vec<u8>)> {
    let wanted = parse_id(id)?;
    let body = decode(data, &format!("{} {id}", kind.name()))?;
    if hash_object(ALGORITHM, kind, &body)? != wanted {
        bail!(
            "{} {id} does not match its content; the server's answer is refused and nothing was stored",
            kind.name()
        );
    }
    Ok((wanted, body))
}

fn api_mode(mode: FileMode) -> Result<api::FileMode> {
    Ok(match mode {
        FileMode::Regular => api::FileMode::Regular,
        FileMode::Executable => api::FileMode::Executable,
        FileMode::Symlink => api::FileMode::Symlink,
        _ => bail!("a submodule entry cannot be handled"),
    })
}

fn encode(id: &ObjectId, body: &[u8]) -> api::PushObject {
    api::PushObject {
        id: id.to_hex(),
        data: STANDARD.encode(body),
    }
}

impl LocalStore {
    /// Every commit reachable from `from`, itself included.
    pub(crate) fn ancestors(&self, from: Option<ObjectId>) -> Result<HashSet<ObjectId>> {
        self.reach(from.into_iter().collect())
    }

    fn reach(&self, from: Vec<ObjectId>) -> Result<HashSet<ObjectId>> {
        let mut known = HashSet::new();
        let mut queue = from;
        while let Some(id) = queue.pop() {
            if known.insert(id) {
                queue.extend(self.repo().commit(&id)?.parents);
            }
        }
        Ok(known)
    }

    fn tree_of(&self, commit: Option<ObjectId>) -> Result<Option<ObjectId>> {
        Ok(match commit {
            Some(id) => Some(self.repo().commit(&id)?.tree),
            None => None,
        })
    }

    /// The first-parent chain of local commits the server lacks, `None` if none; a merge may only merge in commits
    /// the server has.
    pub fn outgoing(&self, branch: &str) -> Result<Option<Outgoing>> {
        let (Some(tip), published) = (self.tip(branch)?, self.published(branch)?) else {
            return Ok(None);
        };
        let known = self.ancestors(published)?;
        let (mut commits, mut at, mut base) = (Vec::new(), Some(tip), None);
        while let Some(id) = at {
            if known.contains(&id) {
                base = Some(id);
                break;
            }
            let parents = self.repo().commit(&id)?.parents;
            if let Some(local) = parents.iter().skip(1).find(|p| !known.contains(p)) {
                bail!(
                    "merge commit {} merges {}, which the server does not have; push that history first",
                    short(&id.to_hex()),
                    short(&local.to_hex())
                );
            }
            commits.push(id);
            at = parents.first().copied();
        }
        commits.reverse();
        Ok((!commits.is_empty()).then_some(Outgoing { commits, base }))
    }

    /// Stores the commits and trees of a page, all verified first; a commit needs its tree and parents already stored.
    fn store_page(&self, commits: &[api::FetchCommit]) -> Result<()> {
        let mut checked = Vec::new();
        for c in commits {
            let (id, body) = verified(Kind::Commit, &c.id, &c.data)?;
            let trees = c
                .trees
                .iter()
                .map(|t| verified(Kind::Tree, &t.id, &t.data))
                .collect::<Result<Vec<_>>>()?;
            checked.push((id, body, trees));
        }
        let objects = self.repo().objects();
        for (id, body, trees) in checked {
            for (_, bytes) in trees {
                objects.write(Kind::Tree, &bytes)?;
            }
            let record = Commit::parse_lenient(ALGORITHM, &body)
                .with_context(|| format!("commit {id} from the server cannot be read"))?
                .object;
            if !self.has(&record.tree)? {
                bail!("commit {id} came without its tree {}", record.tree);
            }
            if let Some(missing) = record
                .parents
                .iter()
                .find(|p| !self.has(p).unwrap_or(false))
            {
                bail!("commit {id} came without its parent {missing}");
            }
            objects.write(Kind::Commit, &body)?;
        }
        Ok(())
    }

    /// Brings the server's commits and trees for `branch` into the store and moves the remote-tracking ref.
    pub fn fetch(&self, remote: &dyn Backend, branch: &str) -> Result<Fetched> {
        let previous = self.published(branch)?;
        let haves: Vec<String> = previous.iter().map(ObjectId::to_hex).collect();
        let (mut cursor, mut head) = (None, None);
        loop {
            let page = remote.fetch_page(&api::FetchRequest {
                branches: vec![branch.to_string()],
                haves: haves.clone(),
                cursor: cursor.take(),
                limit: None,
            })?;
            if let Some(r) = page.refs.iter().find(|r| r.branch == branch) {
                head = r.head.clone();
            }
            self.store_page(&page.commits)?;
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let head = head.map(|h| parse_id(&h)).transpose()?;
        if let Some(head) = &head {
            if !self.has(head)? {
                bail!("the server's head {head} was not among the commits it sent");
            }
            let refs = self.repo().refs();
            refs.update(&tracking_ref(branch), head)?;
            if refs.resolve(&local_ref(branch))?.is_none() {
                refs.update(&local_ref(branch), head)?;
                refs.set_symbolic("HEAD", &local_ref(branch))?;
            }
        }
        Ok(Fetched { previous, head })
    }

    /// The trees of `id` that `parent` does not hold at the same place, with their bodies.
    fn new_trees(
        &self,
        id: &ObjectId,
        parent: Option<ObjectId>,
        out: &mut Vec<(ObjectId, Vec<u8>)>,
        seen: &mut HashSet<ObjectId>,
    ) -> Result<()> {
        if Some(*id) == parent || !seen.insert(*id) {
            return Ok(());
        }
        let body = self.repo().objects().read(id)?.data;
        let before: HashMap<Vec<u8>, ObjectId> = match parent {
            Some(p) => parse_tree(ALGORITHM, &self.repo().objects().read(&p)?.data)?
                .into_iter()
                .filter(|e| e.mode.is_tree())
                .map(|e| (e.name, e.id))
                .collect(),
            None => HashMap::new(),
        };
        for entry in parse_tree(ALGORITHM, &body)?
            .into_iter()
            .filter(|e| e.mode.is_tree())
        {
            self.new_trees(&entry.id, before.get(&entry.name).copied(), out, seen)?;
        }
        out.push((*id, body));
        Ok(())
    }

    /// What a push of `outgoing` sends: the blobs to upload first, then the commits and trees.
    #[allow(clippy::type_complexity)]
    fn plan(
        &self,
        outgoing: &Outgoing,
    ) -> Result<(
        Vec<ObjectId>,
        Vec<(ObjectId, Vec<u8>)>,
        Vec<(ObjectId, Vec<u8>)>,
    )> {
        let (mut blobs, mut commits, mut trees) = (Vec::new(), Vec::new(), Vec::new());
        let (mut seen_blobs, mut seen_trees) = (HashSet::new(), HashSet::new());
        let mut parent = outgoing.base;
        for id in &outgoing.commits {
            let commit = self.repo().commit(id)?;
            let before = self.tree_of(parent)?;
            let merged_in = match commit.parents.get(1) {
                Some(other) => {
                    let tree = self.tree_of(Some(*other))?;
                    self.repo().flatten(tree.as_ref(), &Pathspec::default())?
                }
                None => Default::default(),
            };
            for change in
                self.repo()
                    .diff_trees(before.as_ref(), Some(&commit.tree), &Pathspec::default())?
            {
                let path = String::from_utf8_lossy(&change.path).into_owned();
                let Some(entry) = change.new else {
                    continue;
                };
                if entry.mode == FileMode::Gitlink {
                    bail!("{path} is a submodule entry, which pyn does not support");
                }
                if merged_in.get(&change.path) == Some(&entry) {
                    continue;
                }
                if seen_blobs.insert(entry.id) {
                    blobs.push(entry.id);
                }
            }
            self.new_trees(&commit.tree, before, &mut trees, &mut seen_trees)?;
            commits.push((*id, self.repo().objects().read(id)?.data));
            parent = Some(*id);
        }
        Ok((blobs, commits, trees))
    }

    /// The lowest common ancestors of `a` and `b`: common commits that no other common commit descends from.
    fn merge_bases(&self, a: ObjectId, b: ObjectId) -> Result<Vec<ObjectId>> {
        let (from_a, from_b) = (self.ancestors(Some(a))?, self.ancestors(Some(b))?);
        let common: Vec<ObjectId> = from_a.intersection(&from_b).copied().collect();
        let mut parents = Vec::new();
        for id in &common {
            parents.extend(self.repo().commit(id)?.parents);
        }
        let above = self.reach(parents)?;
        let mut bases: Vec<ObjectId> = common.into_iter().filter(|c| !above.contains(c)).collect();
        bases.sort_by_key(ObjectId::to_hex);
        Ok(bases)
    }

    /// The merged files, and the exclusive paths kept under the person's lock; any other clash refuses with
    /// nothing written.
    fn three_way(
        &self,
        remote: &dyn Backend,
        base: Option<ObjectId>,
        local: ObjectId,
        theirs: ObjectId,
    ) -> Result<(Files, Vec<String>)> {
        let flat = |c: Option<ObjectId>| -> Result<Files> {
            let tree = self.tree_of(c)?;
            Ok(self
                .repo()
                .flatten(tree.as_ref(), &Pathspec::default())?
                .into_iter()
                .collect())
        };
        let (b, o, t) = (flat(base)?, flat(Some(local))?, flat(Some(theirs))?);
        let mut files = o.clone();
        let (mut modes, mut locks) = (None, None);
        let (mut clashes, mut diverged, mut kept) = (Vec::new(), Vec::new(), Vec::new());
        let paths: std::collections::BTreeSet<&Vec<u8>> =
            b.keys().chain(o.keys()).chain(t.keys()).collect();
        for path in paths {
            let (bv, ov, tv) = (
                b.get(path).copied(),
                o.get(path).copied(),
                t.get(path).copied(),
            );
            if ov == tv || tv == bv {
                continue;
            }
            if ov == bv {
                match tv {
                    Some(entry) => files.insert(path.clone(), entry),
                    None => files.remove(path),
                };
                continue;
            }
            let name = String::from_utf8_lossy(path).into_owned();
            let (Some(_), Some(_)) = (ov, tv) else {
                clashes.push(format!(
                    "{name} (changed on one side, removed on the other)"
                ));
                continue;
            };
            let modes = match &mut modes {
                Some(m) => m,
                None => modes.insert(shared_paths(remote, &theirs)?),
            };
            if modes.get(&name) != Some(&true) {
                let locks = match &mut locks {
                    Some(l) => l,
                    None => locks.insert(my_locks(remote)?),
                };
                if locks.contains(&name) {
                    kept.push(name);
                } else {
                    diverged.push(name);
                }
                continue;
            }
            match self.merge_entry(remote, bv, ov, tv)? {
                Some(entry) => {
                    files.insert(path.clone(), entry);
                }
                None => clashes.push(name),
            }
        }
        let names: Vec<&Vec<u8>> = files.keys().collect();
        for pair in names.windows(2) {
            if pair[1].starts_with(pair[0]) && pair[1].get(pair[0].len()) == Some(&b'/') {
                clashes.push(format!(
                    "{} (a file on one side and a folder on the other)",
                    String::from_utf8_lossy(pair[0])
                ));
            }
        }
        if clashes.is_empty() && diverged.is_empty() {
            return Ok((files, kept));
        }
        let mut message = String::from(
            "error: the pull would merge changes that conflict; nothing was changed\n",
        );
        if !clashes.is_empty() {
            message.push_str(&format!(
                "both sides changed the same lines of, or removed:\n\t{}\n",
                clashes.join("\n\t")
            ));
        }
        if !diverged.is_empty() {
            message.push_str(&format!(
                "exclusive files, which cannot be merged, changed on both sides:\n\t{}\n\
                 hint: To keep your version, take the lock with `pyn lock <path>` and pull again. To take the server's, \
                 commit the server's version of the file over yours, then pull again.\n",
                diverged.join("\n\t")
            ));
        }
        message.push_str(
            "hint: `pyn pull --rebase` replays your commits instead and stops on the same files.",
        );
        Err(Rejected(message).into())
    }

    /// Replays the local commits onto the fetched tip with a three-way merge per file; returns the new tip.
    /// Nothing is written to the branch when a file conflicts.
    fn replay(
        &self,
        remote: &dyn Backend,
        outgoing: &Outgoing,
        onto: ObjectId,
        who: &pyn_object::Signature,
    ) -> Result<(ObjectId, usize)> {
        let repo = self.repo();
        let onto_tree = repo.commit(&onto)?.tree;
        let mut files = repo.flatten(Some(&onto_tree), &Pathspec::default())?;
        let (mut parent, mut parent_tree, mut dropped) = (onto, onto_tree, 0);
        for id in &outgoing.commits {
            let commit = repo.commit(id)?;
            if commit.parents.len() > 1 {
                return Err(Rejected(format!(
                    "local commit {} is a merge commit, which cannot be rebased; use `pyn pull` to merge\nnothing was changed",
                    short(&id.to_hex())
                ))
                .into());
            }
            let before = self.tree_of(commit.parents.first().copied())?;
            let mut conflicts = Vec::new();
            for change in
                repo.diff_trees(before.as_ref(), Some(&commit.tree), &Pathspec::default())?
            {
                let ours = files.get(&change.path).copied();
                if ours == change.old {
                    match change.new {
                        Some(entry) => files.insert(change.path, entry),
                        None => files.remove(&change.path),
                    };
                } else if ours != change.new {
                    match self.merge_entry(remote, change.old, ours, change.new)? {
                        Some(entry) => {
                            files.insert(change.path, entry);
                        }
                        None => conflicts.push(String::from_utf8_lossy(&change.path).into_owned()),
                    }
                }
            }
            if !conflicts.is_empty() {
                return Err(Rejected(format!(
                    "could not apply local commit {} onto the fetched commits: both sides changed {}\n\
                     resolving conflicts is not supported yet; nothing was changed",
                    short(&id.to_hex()),
                    conflicts.join(", ")
                ))
                .into());
            }
            let (tree, trees) = build_trees(&files)?;
            if tree == parent_tree {
                dropped += 1;
                continue;
            }
            for (_, body) in trees {
                repo.objects().write(Kind::Tree, &body)?;
            }
            let record = Commit {
                tree,
                parents: vec![parent],
                author: commit.author,
                committer: who.clone(),
                extra_headers: Vec::new(),
                message: commit.message,
            };
            let body = record.encode(ALGORITHM)?;
            parent = repo.objects().write(Kind::Commit, &body)?;
            parent_tree = tree;
        }
        Ok((parent, dropped))
    }

    /// The merge of a file both sides changed: text without overlapping edits and equal modes; `None` otherwise.
    fn merge_entry(
        &self,
        remote: &dyn Backend,
        base: Option<Entry>,
        ours: Option<Entry>,
        theirs: Option<Entry>,
    ) -> Result<Option<Entry>> {
        let (Some(ours), Some(theirs)) = (ours, theirs) else {
            return Ok(None);
        };
        if ours.mode != theirs.mode || base.is_some_and(|b| b.mode != ours.mode) {
            return Ok(None);
        }
        let read = |entry: Option<Entry>| -> Result<Vec<u8>> {
            match entry {
                Some(e) => self.blob(remote, &e.id.to_hex()),
                None => Ok(Vec::new()),
            }
        };
        let (base, ours_bytes, theirs_bytes) =
            (read(base)?, read(Some(ours))?, read(Some(theirs))?);
        let Outcome::Clean(merged) = merge_text(&base, &ours_bytes, &theirs_bytes) else {
            return Ok(None);
        };
        let id = self.repo().objects().write(Kind::Blob, &merged)?;
        Ok(Some(Entry {
            mode: ours.mode,
            id,
        }))
    }
}

/// Per path of the server's commit, whether it is shared.
fn shared_paths(remote: &dyn Backend, commit: &ObjectId) -> Result<HashMap<String, bool>> {
    Ok(remote
        .snapshot(Some(&commit.to_hex()))?
        .1
        .into_iter()
        .map(|e| (e.path, e.mode == api::Mode::Shared))
        .collect())
}

/// The paths the signed-in person holds a lock on.
fn my_locks(remote: &dyn Backend) -> Result<HashSet<String>> {
    let api = remote.pyn_server("pull")?;
    let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
    let me = remote.whoami()?;
    Ok(locks
        .into_iter()
        .filter(|l| l.owner == me)
        .map(|l| l.path)
        .collect())
}

fn arrow(previous: Option<ObjectId>, head: ObjectId, branch: &str) -> String {
    let (old, new) = (previous.map(|p| p.to_hex()), head.to_hex());
    match old {
        Some(o) => format!(
            "   {}..{}  {branch} -> origin/{branch}",
            short(&o),
            short(&new)
        ),
        None => format!(" * [new branch]      {branch} -> origin/{branch}"),
    }
}

/// `pyn fetch` with `local_store` on: stores what the server has that the store lacks, and touches no file.
pub fn fetch(backend: &dyn Backend, ws: &Workspace) -> Result<Fetched> {
    merging::refuse_during_merge(ws, "fetch")?;
    let branch = branch::name(Some(ws));
    let store = LocalStore::open_or_create(ws, &branch)?;
    let fetched = store.fetch(backend, &branch)?;
    if let Some(head) = fetched.head
        && fetched.previous != Some(head)
    {
        let api = backend.pyn_server("fetch")?;
        println!(
            "From {}/{}",
            api.base,
            api.repo.as_deref().unwrap_or_default()
        );
        println!("{}", arrow(fetched.previous, head, &branch));
    }
    Ok(fetched)
}

/// Paths changed in the workspace and not yet committed.
fn dirty_paths(ws: &Workspace) -> Result<HashSet<String>> {
    let mut dirty = HashSet::new();
    for (path, known) in ws.load_state()? {
        let abs = ws.abs(&path);
        if localfile::exists(&abs) {
            let (mode, bytes) = localfile::read(&abs)?;
            if known.changed(mode, &bytes) {
                dirty.insert(path);
            }
        }
    }
    Ok(dirty)
}

/// Refuses when an uncommitted change is on a path the incoming commits also change.
fn guard_changes(
    store: &LocalStore,
    ws: &Workspace,
    from: Option<ObjectId>,
    to: ObjectId,
) -> Result<()> {
    let dirty = dirty_paths(ws)?;
    let (a, b) = (store.tree_of(from)?, store.tree_of(Some(to))?);
    let hit: Vec<String> = store
        .repo()
        .diff_trees(a.as_ref(), b.as_ref(), &Pathspec::default())?
        .into_iter()
        .map(|c| String::from_utf8_lossy(&c.path).into_owned())
        .filter(|p| dirty.contains(p))
        .collect();
    if hit.is_empty() {
        return Ok(());
    }
    Err(Rejected(format!(
        "error: your local changes to the following files would be overwritten by pull:\n\t{}\n\
         commit them, or put them back with `pyn checkout -- <path>`, before you pull; nothing was changed",
        hit.join("\n\t")
    ))
    .into())
}

/// `pyn pull` with `local_store` on: fetch, then fast-forward, merge, or with `rebase` replay the local commits.
pub fn pull(backend: &dyn Backend, ws: &Workspace, rebase: bool) -> Result<()> {
    if !Index::load(ws)?.entries.is_empty() {
        bail!(
            "you have staged changes; commit them or take them out with `pyn restore --staged` before you pull"
        );
    }
    fetch(backend, ws)?;
    let branch = branch::name(Some(ws));
    let store = LocalStore::open_or_create(ws, &branch)?;
    let (Some(local), Some(theirs)) = (store.tip(&branch)?, store.published(&branch)?) else {
        return sync::update(backend, ws, &[], None);
    };
    let reachable = store.ancestors(Some(local))?;
    if reachable.contains(&theirs) {
        println!("Already up to date.");
        return Ok(());
    }
    let refs = store.repo().refs();
    if store.ancestors(Some(theirs))?.contains(&local) {
        guard_changes(&store, ws, Some(local), theirs)?;
        refs.update_checked(&local_ref(&branch), Expect::Id(local), &theirs)
            .context("the local branch moved while pulling; run the command again")?;
        println!(
            "Updating {}..{}\nFast-forward",
            short(&local.to_hex()),
            short(&theirs.to_hex())
        );
        return sync::update(backend, ws, &[], None);
    }
    let who = signature(&backend.whoami()?);
    if rebase {
        let Some(outgoing) = store.outgoing(&branch)? else {
            bail!("the local branch cannot be related to the fetched commits");
        };
        guard_changes(&store, ws, outgoing.base, theirs)?;
        let (tip, dropped) = store.replay(backend, &outgoing, theirs, &who)?;
        let merged = merged_files(&store, ws, local, theirs, tip)?;
        refs.update_checked(&local_ref(&branch), Expect::Id(local), &tip)
            .context("the local branch moved while pulling; run the command again")?;
        println!(
            "Rebased {} local commit(s) onto {}{}",
            outgoing.commits.len() - dropped,
            short(&theirs.to_hex()),
            if dropped > 0 {
                format!(" ({dropped} became empty and was dropped)")
            } else {
                String::new()
            }
        );
        write_merged(&store, ws, &merged)?;
        return sync::update(backend, ws, &[], None);
    }
    let bases = store.merge_bases(local, theirs)?;
    if bases.len() > 1 {
        return Err(Rejected(
            "the local branch and the fetched commits have several merge bases, which a pull does not merge yet\n\
             hint: `pyn pull --rebase` replays your commits instead; nothing was changed"
                .into(),
        )
        .into());
    }
    let base = bases.first().copied();
    guard_changes(&store, ws, base, theirs)?;
    let (files, kept) = store.three_way(backend, base, local, theirs)?;
    let (tree, trees) = build_trees(&files)?;
    let repo = store.repo();
    for (_, body) in trees {
        repo.objects().write(Kind::Tree, &body)?;
    }
    let api = backend.pyn_server("pull")?;
    let url = format!("{}/{}", api.base, api.repo.as_deref().unwrap_or_default());
    let into = if matches!(branch.as_str(), "main" | "master") {
        String::new()
    } else {
        format!(" into {branch}")
    };
    let record = Commit {
        tree,
        parents: vec![local, theirs],
        author: who.clone(),
        committer: who,
        extra_headers: Vec::new(),
        message: format!("Merge branch '{branch}' of {url}{into}\n").into_bytes(),
    };
    let tip = repo
        .objects()
        .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
    let merged = merged_files(&store, ws, local, theirs, tip)?;
    refs.update_checked(&local_ref(&branch), Expect::Id(local), &tip)
        .context("the local branch moved while pulling; run the command again")?;
    for path in kept {
        println!("Kept your version of {path}: you hold its lock");
    }
    println!("Merge made by the 'ort' strategy.");
    write_merged(&store, ws, &merged)?;
    sync::update(backend, ws, &[], None)
}

/// The files whose content the replay merged: the new tip differs from the local one and from the fetched one.
fn merged_files(
    store: &LocalStore,
    ws: &Workspace,
    local: ObjectId,
    theirs: ObjectId,
    tip: ObjectId,
) -> Result<Vec<(String, Entry, Entry)>> {
    let tree = |c| store.tree_of(Some(c));
    let (local_tree, tip_tree, their_tree) = (tree(local)?, tree(tip)?, tree(theirs)?);
    let theirs_files = store
        .repo()
        .flatten(their_tree.as_ref(), &Pathspec::default())?;
    let dirty = dirty_paths(ws)?;
    let mut out = Vec::new();
    for change in
        store
            .repo()
            .diff_trees(local_tree.as_ref(), tip_tree.as_ref(), &Pathspec::default())?
    {
        let (Some(old), Some(new)) = (change.old, change.new) else {
            continue;
        };
        if theirs_files.get(&change.path) == Some(&new) {
            continue;
        }
        let path = String::from_utf8_lossy(&change.path).into_owned();
        if dirty.contains(&path) {
            bail!(
                "{path} has uncommitted changes and the pull would rewrite it; nothing was changed"
            );
        }
        out.push((path, old, new));
    }
    Ok(out)
}

fn write_merged(
    store: &LocalStore,
    ws: &Workspace,
    merged: &[(String, Entry, Entry)],
) -> Result<()> {
    let mut state = ws.load_state()?;
    for (path, _, new) in merged {
        let bytes = store.repo().blob(&new.id)?;
        let target = ws.abs(path);
        set_read_only(&target, false)?;
        std::fs::write(&target, &bytes).with_context(|| format!("writing {}", target.display()))?;
        let mode = api_mode(new.mode)?;
        localfile::apply(&target, mode)?;
        if let Some(known) = state.get_mut(path) {
            known.hash = hash_of(&bytes);
            known.file_mode = mode;
        }
    }
    ws.save_state(&state)
}

/// What the server's refusal means for the person pushing, in git's words where git has them.
pub fn explain_push(err: anyhow::Error, branch: &str) -> anyhow::Error {
    let Some(e) = err.downcast_ref::<ApiError>() else {
        return err;
    };
    let (headline, hint) = match e.code.as_str() {
        "not_fast_forward" | "stale_base" => (
            format!(" ! [rejected]        {branch} -> {branch} (non-fast-forward)"),
            "Updates were rejected because the server has commits you do not have locally.\n\
             hint: Run `pyn pull` to bring them in and merge them, then push again.",
        ),
        "lock_required" => (
            format!(" ! [rejected]        {branch} -> {branch} (lock required)"),
            "An exclusive path needs your live lock when you push; a lease can lapse while you work.\n\
             hint: Run `pyn lock <path>` for the path named above, then push again.",
        ),
        "lock_held" => (
            format!(" ! [rejected]        {branch} -> {branch} (locked by someone else)"),
            "Another person holds the lock on an exclusive path in your commits.\n\
             hint: Wait for them to release it, or ask them; their version has to come in first.",
        ),
        "object_missing" => (
            format!(" ! [rejected]        {branch} -> {branch} (object missing)"),
            "The server lacks a file the commits need.\nhint: Push again to upload it.",
        ),
        "push_invalid" => (
            format!(" ! [rejected]        {branch} -> {branch} (invalid push)"),
            "The server could not accept the commits as sent.",
        ),
        "push_unsupported" => (
            format!(" ! [rejected]        {branch} -> {branch} (not supported yet)"),
            "The server does not accept this kind of commit yet, for example one that changes nothing or a submodule.",
        ),
        "merge_conflict" => (
            format!(" ! [rejected]        {branch} -> {branch} (merge conflict)"),
            "A merge commit leaves an exclusive file that changed on both sides without a new version of it.\n\
             hint: Take the lock with `pyn lock <path>` for the path named above, run `pyn pull` again to make the merge, and push.",
        ),
        "storage_limit_reached" => (
            format!(" ! [rejected]        {branch} -> {branch} (storage limit)"),
            "The repository has reached its storage limit.",
        ),
        "forbidden" => (
            format!(" ! [rejected]        {branch} -> {branch} (forbidden)"),
            "You need the checkin permission on this repository, and the policy permission to change `.pyn/pyn.toml` on the default branch.",
        ),
        "invalid_rules" => (
            format!(" ! [rejected]        {branch} -> {branch} (invalid policy)"),
            "The policy file in your commits does not validate.\n\
             hint: Fix `.pyn/pyn.toml`, amend or add a commit, and push again.",
        ),
        _ => return err,
    };
    Rejected(format!(
        "{headline}\nerror: failed to push some refs: {}\nhint: {hint}\nnothing was pushed; the server is unchanged",
        e.message
    ))
    .into()
}

/// `pyn push` with `local_store` on: uploads the blobs, then sends the local commits and trees in one request.
pub fn push(backend: &dyn Backend, ws: &Workspace) -> Result<()> {
    merging::refuse_during_merge(ws, "push")?;
    let branch = branch::name(Some(ws));
    let Some(store) = LocalStore::open(ws) else {
        bail!("the local store is empty; run `pyn fetch` first");
    };
    let published = store.published(&branch)?;
    let Some(outgoing) = store.outgoing(&branch)? else {
        println!("Everything up-to-date");
        return Ok(());
    };
    let tip = outgoing
        .commits
        .last()
        .copied()
        .expect("outgoing has commits");
    if let Some(p) = published
        && !store.ancestors(Some(tip))?.contains(&p)
    {
        return Err(explain_push(
            ApiError {
                code: "not_fast_forward".into(),
                status: 409,
                message: "the local branch does not build on the server's latest commit".into(),
            }
            .into(),
            &branch,
        ));
    }
    let (blobs, commits, trees) = store.plan(&outgoing)?;
    for id in &blobs {
        let bytes = store
            .repo()
            .blob(id)
            .with_context(|| format!("blob {id} is not in the local store"))?;
        let sent = backend.upload_bytes(bytes)?;
        if sent != id.to_hex() {
            bail!("the server stored a file as {sent}, not {id}");
        }
    }
    let response = backend
        .push_commits(&api::PushRequest {
            branch: Some(branch.clone()),
            commits: commits.iter().map(|(id, body)| encode(id, body)).collect(),
            trees: trees.iter().map(|(id, body)| encode(id, body)).collect(),
        })
        .map_err(|e| explain_push(e, &branch))?;
    let mut head = parse_id(&response.head)?;
    // A policy commit can make the server add conversion commits on top; `fetch` brings them in.
    let policy_pushed = response.revisions.iter().any(|r| r.path == ".pyn/pyn.toml");
    let merged = outgoing
        .commits
        .iter()
        .any(|c| store.repo().commit(c).is_ok_and(|c| c.parents.len() > 1));
    if head != tip && !policy_pushed && !merged {
        bail!("the server's head {head} is not the pushed commit {tip}");
    }
    head = tip;
    store.repo().refs().update(&tracking_ref(&branch), &head)?;
    let mut last: Vec<&api::Revision> = Vec::new();
    for rev in &response.revisions {
        last.retain(|r| r.path != rev.path);
        last.push(rev);
    }
    let mut gone = Vec::new();
    for rev in last {
        if rev.deleted {
            gone.push(rev.path.as_str());
        } else {
            sync::after_commit(backend, ws, &rev.path, rev.id, &rev.content, rev.file_mode)?;
        }
    }
    if !gone.is_empty() {
        let mut state = ws.load_state()?;
        for path in gone {
            state.remove(path);
        }
        ws.save_state(&state)?;
    }
    let api = backend.pyn_server("push")?;
    println!(
        "To {}/{}",
        api.base,
        api.repo.as_deref().unwrap_or_default()
    );
    println!(
        "   {}..{}  {branch} -> {branch}",
        published.map_or_else(|| "0000000".to_string(), |p| short(&p.to_hex()).to_string()),
        short(&head.to_hex())
    );
    stranded::print(
        "The policy change left shared versions on other branches that can no longer merge:",
        &response.stranded,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api_error(code: &str) -> anyhow::Error {
        ApiError {
            code: code.into(),
            status: 409,
            message: "Content/m.umap is locked".into(),
        }
        .into()
    }

    #[test]
    fn a_non_fast_forward_tells_the_person_to_pull() {
        for code in ["not_fast_forward", "stale_base"] {
            let text = explain_push(api_error(code), "main").to_string();
            assert!(
                text.contains("non-fast-forward") && text.contains("pyn pull"),
                "{text}"
            );
        }
    }

    #[test]
    fn lock_refusals_name_the_path_and_leave_nothing_pushed() {
        let text = explain_push(api_error("lock_required"), "main").to_string();
        assert!(
            text.contains("Content/m.umap") && text.contains("pyn lock"),
            "{text}"
        );
        assert!(text.contains("nothing was pushed"));
        let text = explain_push(api_error("lock_held"), "main").to_string();
        assert!(text.contains("locked by someone else"), "{text}");
    }

    #[test]
    fn other_errors_pass_through() {
        let err = explain_push(api_error("something_else"), "main");
        assert!(err.downcast_ref::<Rejected>().is_none());
    }

    #[test]
    fn a_body_that_does_not_hash_to_its_id_is_refused() {
        let id = hash_object(ALGORITHM, Kind::Blob, b"x").unwrap().to_hex();
        let err = verified(Kind::Blob, &id, &STANDARD.encode(b"y")).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert!(verified(Kind::Blob, &id, &STANDARD.encode(b"x")).is_ok());
    }

    #[test]
    fn modes_map_to_the_api() {
        assert_eq!(
            api_mode(FileMode::Executable).unwrap(),
            api::FileMode::Executable
        );
        assert!(api_mode(FileMode::Gitlink).is_err());
    }
}

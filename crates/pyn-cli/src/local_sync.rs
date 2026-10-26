//! `fetch`, `pull` and `push` of a workspace with `local_store` on (docs/concepts/commits.md): commits and trees travel
//! git-encoded, every id is verified by hashing, and a push is accepted or refused as a whole.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pyn_git::Expect;
use pyn_gitwork::{Entry, Pathspec};
use pyn_merge::{Outcome, merge_text, render_markers};
use pyn_object::{Commit, FileMode, Kind, ObjectId, hash_object, parse_tree};
use pyn_proto as api;

use crate::backend::Backend;
use crate::branch;
use crate::client::ApiError;
use crate::git_home::ServerPush;
use crate::local_commit::signature;
use crate::local_merge::{self, Control};
use crate::local_store::{ALGORITHM, Files, LocalStore, build_trees, parse_id};
use crate::snapshot::{self, short};
use crate::staging::Index;
use crate::stranded;
use crate::transport::Method;
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

/// A push the server refused over a lock: reported like `Rejected`, with the exit status of the other lock refusals.
#[derive(Debug)]
pub struct LockRejected(pub String);

impl std::fmt::Display for LockRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LockRejected {}

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

pub(crate) fn local_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

fn decode(data: &str, what: &str) -> Result<Vec<u8>> {
    STANDARD
        .decode(data)
        .with_context(|| format!("{what} from the server is not valid base64"))
}

/// The body of an object the server sent, only if it hashes to the id it came with.
pub(crate) fn verified(kind: Kind, id: &str, data: &str) -> Result<(ObjectId, Vec<u8>)> {
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

pub(crate) fn api_mode(mode: FileMode) -> Result<api::FileMode> {
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

    pub(crate) fn reach(&self, from: Vec<ObjectId>) -> Result<HashSet<ObjectId>> {
        let mut known = HashSet::new();
        let mut queue = from;
        while let Some(id) = queue.pop() {
            if known.insert(id) {
                queue.extend(self.repo().commit(&id)?.parents);
            }
        }
        Ok(known)
    }

    pub(crate) fn tree_of(&self, commit: Option<ObjectId>) -> Result<Option<ObjectId>> {
        Ok(match commit {
            Some(id) => Some(self.repo().commit(&id)?.tree),
            None => None,
        })
    }

    /// The first-parent chain of local commits the server lacks, `None` if none; a merge may only merge in commits
    /// the server has.
    pub fn outgoing(&self, branch: &str) -> Result<Option<Outgoing>> {
        self.outgoing_to(self.tip(branch)?, self.published(branch)?)
    }

    /// Like `outgoing`, for a tip and the server's tip of the branch it goes to.
    pub fn outgoing_to(
        &self,
        tip: Option<ObjectId>,
        published: Option<ObjectId>,
    ) -> Result<Option<Outgoing>> {
        let Some(tip) = tip else {
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

    /// Brings the server's commits and trees for `branches` (every branch if empty) into the store and moves their
    /// remote-tracking refs; the `current` local branch is created from its server branch if the store has none.
    pub fn fetch(
        &self,
        remote: &dyn Backend,
        branches: &[String],
        current: &str,
    ) -> Result<Vec<(String, Fetched)>> {
        let haves: Vec<String> = self
            .repo()
            .refs()
            .list("refs/remotes/origin/")?
            .iter()
            .map(|(_, id)| id.to_hex())
            .collect();
        let (mut cursor, mut heads) = (None, std::collections::BTreeMap::new());
        loop {
            let page = remote.fetch_page(&api::FetchRequest {
                branches: branches.to_vec(),
                haves: haves.clone(),
                cursor: cursor.take(),
                limit: None,
            })?;
            for r in &page.refs {
                heads.insert(r.branch.clone(), r.head.clone());
            }
            self.store_page(&page.commits)?;
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let refs = self.repo().refs();
        let mut done = Vec::new();
        for (branch, head) in heads {
            let previous = refs.resolve(&tracking_ref(&branch))?;
            let head = head.map(|h| parse_id(&h)).transpose()?;
            if let Some(head) = &head {
                if !self.has(head)? {
                    bail!("the server's head {head} was not among the commits it sent");
                }
                refs.update(&tracking_ref(&branch), head)?;
                if branch == current && refs.resolve(&local_ref(&branch))?.is_none() {
                    refs.update(&local_ref(&branch), head)?;
                    refs.set_symbolic("HEAD", &local_ref(&branch))?;
                    self.set_upstream(&branch, Some(&branch))?;
                }
            }
            done.push((branch, Fetched { previous, head }));
        }
        Ok(done)
    }

    /// The trees of `id` that `parent` does not hold at the same place, with their bodies.
    fn new_trees(
        &self,
        id: &ObjectId,
        parent: Option<ObjectId>,
        out: &mut Vec<(ObjectId, Vec<u8>)>,
        seen: &mut HashSet<ObjectId>,
        depth: usize,
    ) -> Result<()> {
        if Some(*id) == parent || !seen.insert(*id) {
            return Ok(());
        }
        if depth > pyn_gitwork::MAX_DEPTH {
            bail!(
                "a path is nested more than {} folders deep",
                pyn_gitwork::MAX_DEPTH
            );
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
            self.new_trees(
                &entry.id,
                before.get(&entry.name).copied(),
                out,
                seen,
                depth + 1,
            )?;
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
            self.new_trees(&commit.tree, before, &mut trees, &mut seen_trees, 1)?;
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

    /// Merges the files of `local` and `theirs` over `base`: clean results are taken, the exclusive paths kept under
    /// the person's lock are `kept`, and a path that needs a decision is a conflict holding a placeholder.
    pub(crate) fn join_trees(
        &self,
        remote: &dyn Backend,
        base: Option<ObjectId>,
        local: ObjectId,
        theirs: ObjectId,
        labels: (&str, &str),
    ) -> Result<Joined> {
        let flat = |c: Option<ObjectId>| -> Result<Files> {
            let tree = self.tree_of(c)?;
            Ok(self
                .repo()
                .flatten(tree.as_ref(), &Pathspec::default())?
                .into_iter()
                .collect())
        };
        let (b, o, t) = (flat(base)?, flat(Some(local))?, flat(Some(theirs))?);
        let mut joined = Joined {
            files: o.clone(),
            conflicts: BTreeMap::new(),
            kept: Vec::new(),
            tangled: Vec::new(),
        };
        let mut judge = Judge::new(remote, theirs);
        let paths: std::collections::BTreeSet<&Vec<u8>> =
            b.keys().chain(o.keys()).chain(t.keys()).collect();
        for path in paths {
            let sides = (
                b.get(path).copied(),
                o.get(path).copied(),
                t.get(path).copied(),
            );
            match self.join_path(remote, &mut judge, path, sides, false, labels)? {
                Step::Take(entry) => set_entry(&mut joined.files, path, entry),
                Step::Kept(entry) => {
                    set_entry(&mut joined.files, path, entry);
                    joined.kept.push(String::from_utf8_lossy(path).into_owned());
                }
                Step::Conflict(c) => {
                    set_entry(&mut joined.files, path, c.placeholder);
                    joined.conflicts.insert(path.clone(), c);
                }
            }
        }
        joined.tangled = tangled(&joined.files);
        Ok(joined)
    }

    /// Decides one path from its base, ours and theirs entries; `mine_is_theirs` says which side the person's lock
    /// keeps.
    pub(crate) fn join_path(
        &self,
        remote: &dyn Backend,
        judge: &mut Judge<'_>,
        path: &[u8],
        (base, ours, theirs): (Option<Entry>, Option<Entry>, Option<Entry>),
        mine_is_theirs: bool,
        labels: (&str, &str),
    ) -> Result<Step> {
        if ours == theirs || theirs == base {
            return Ok(Step::Take(ours));
        }
        if ours == base {
            return Ok(Step::Take(theirs));
        }
        let conflict = |label, exclusive, working| {
            Step::Conflict(Conflict {
                label,
                exclusive,
                ours,
                theirs,
                placeholder: ours.or(theirs),
                working,
            })
        };
        let (Some(o), Some(t)) = (ours, theirs) else {
            return Ok(conflict("deleted on one side", false, None));
        };
        let name = String::from_utf8_lossy(path).into_owned();
        if !judge.shared(&name)? {
            return Ok(if judge.locked(&name)? {
                Step::Kept(if mine_is_theirs { theirs } else { ours })
            } else {
                conflict("both modified", true, None)
            });
        }
        let label = if base.is_none() {
            "both added"
        } else {
            "both modified"
        };
        Ok(match self.merge_content(remote, base, o, t, labels)? {
            Merged::Clean(entry) => Step::Take(Some(entry)),
            Merged::Markers(bytes) => conflict(label, false, Some(bytes)),
            Merged::Whole => conflict(label, false, None),
        })
    }

    /// The merge of a file both sides changed: text merged line by line, markers where lines overlap, and the whole
    /// file as the conflict for binary content or differing modes.
    fn merge_content(
        &self,
        remote: &dyn Backend,
        base: Option<Entry>,
        ours: Entry,
        theirs: Entry,
        labels: (&str, &str),
    ) -> Result<Merged> {
        if ours.mode != theirs.mode || base.is_some_and(|b| b.mode != ours.mode) {
            return Ok(Merged::Whole);
        }
        let read = |entry: Option<Entry>| -> Result<Vec<u8>> {
            match entry {
                Some(e) => self.blob(remote, &e.id.to_hex()),
                None => Ok(Vec::new()),
            }
        };
        let (base, ours_bytes, theirs_bytes) =
            (read(base)?, read(Some(ours))?, read(Some(theirs))?);
        Ok(match merge_text(&base, &ours_bytes, &theirs_bytes) {
            Outcome::Clean(merged) => {
                let id = self.repo().objects().write(Kind::Blob, &merged)?;
                Merged::Clean(Entry {
                    mode: ours.mode,
                    id,
                })
            }
            Outcome::Conflicted(chunks) => {
                Merged::Markers(render_markers(&chunks, labels.0, "base", labels.1))
            }
            Outcome::BinaryConflict => Merged::Whole,
        })
    }
}

fn set_entry(files: &mut Files, path: &[u8], entry: Option<Entry>) {
    match entry {
        Some(entry) => files.insert(path.to_vec(), entry),
        None => files.remove(path),
    };
}

/// Files that are a file on one side and a folder on the other.
fn tangled(files: &Files) -> Vec<String> {
    let mut out = Vec::new();
    for name in files.keys() {
        for (at, byte) in name.iter().enumerate() {
            if *byte == b'/' && files.contains_key(&name[..at]) {
                out.push(String::from_utf8_lossy(&name[..at]).into_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// A path both sides changed that cannot be decided automatically.
pub(crate) struct Conflict {
    pub label: &'static str,
    pub exclusive: bool,
    pub ours: Option<Entry>,
    pub theirs: Option<Entry>,
    /// The entry the merged tree holds until the person resolves the path.
    pub placeholder: Option<Entry>,
    /// What the working tree gets instead of the placeholder's content: text with conflict markers.
    pub working: Option<Vec<u8>>,
}

pub(crate) enum Step {
    Take(Option<Entry>),
    Kept(Option<Entry>),
    Conflict(Conflict),
}

enum Merged {
    Clean(Entry),
    Markers(Vec<u8>),
    Whole,
}

pub(crate) struct Joined {
    pub files: Files,
    pub conflicts: BTreeMap<Vec<u8>, Conflict>,
    pub kept: Vec<String>,
    pub tangled: Vec<String>,
}

/// What the server says about paths of one commit, asked only when a path needs it.
pub(crate) struct Judge<'a> {
    remote: &'a dyn Backend,
    at: ObjectId,
    modes: Option<HashMap<String, bool>>,
    locks: Option<HashSet<String>>,
    keep_locked: bool,
}

impl<'a> Judge<'a> {
    pub(crate) fn new(remote: &'a dyn Backend, at: ObjectId) -> Self {
        Self {
            remote,
            at,
            modes: None,
            locks: None,
            keep_locked: true,
        }
    }

    /// An exclusive path both sides changed is a conflict even under the person's lock.
    pub(crate) fn never_keep(&mut self) {
        self.keep_locked = false;
    }

    fn shared(&mut self, name: &str) -> Result<bool> {
        if self.modes.is_none() {
            self.modes = Some(shared_paths(self.remote, &self.at)?);
        }
        Ok(self.modes.as_ref().and_then(|m| m.get(name)) == Some(&true))
    }

    fn locked(&mut self, name: &str) -> Result<bool> {
        if !self.keep_locked {
            return Ok(false);
        }
        if self.locks.is_none() {
            self.locks = Some(my_locks(self.remote)?);
        }
        Ok(self.locks.as_ref().is_some_and(|l| l.contains(name)))
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
pub(crate) fn my_locks(remote: &dyn Backend) -> Result<HashSet<String>> {
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
pub fn fetch(backend: &dyn Backend, ws: &Workspace) -> Result<Vec<(String, Fetched)>> {
    merging::refuse_during_merge(ws, "fetch")?;
    let current = branch::name(Some(ws));
    let store = LocalStore::open_or_create(ws, &current)?;
    let fetched = store.fetch(backend, &[], &current)?;
    let mut shown = false;
    for (branch, f) in &fetched {
        let Some(head) = f.head else { continue };
        if f.previous == Some(head) {
            continue;
        }
        if !shown {
            let api = backend.pyn_server("fetch")?;
            println!(
                "From {}/{}",
                api.base,
                api.repo.as_deref().unwrap_or_default()
            );
            shown = true;
        }
        println!("{}", arrow(f.previous, head, branch));
    }
    Ok(fetched)
}

/// Paths changed in the workspace and not yet committed.
pub(crate) fn dirty_paths(ws: &Workspace) -> Result<HashSet<String>> {
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
/// A conflict leaves the merge or rebase in progress, to be resolved and concluded or given up.
pub fn pull(
    backend: &dyn Backend,
    ws: &Workspace,
    rebase: bool,
    control: Option<Control>,
) -> Result<()> {
    if control.is_some() {
        local_merge::require_pull(ws)?;
    }
    match control {
        Some(Control::Abort) => return local_merge::abort(backend, ws),
        Some(Control::Continue) => return local_merge::finish(backend, ws, None),
        Some(Control::Skip) => return local_merge::skip(backend, ws),
        None => merging::refuse_during_merge(ws, "pull")?,
    }
    if !Index::load(ws)?.entries.is_empty() {
        bail!(
            "you have staged changes; commit them or take them out with `pyn restore --staged` before you pull"
        );
    }
    fetch(backend, ws)?;
    let branch = branch::name(Some(ws));
    let store = LocalStore::open_or_create(ws, &branch)?;
    let Some(up) = store.upstream(&branch)? else {
        bail!(
            "There is no tracking information for the current branch.\n\
             hint: Set it with `pyn branch --set-upstream-to=origin/<branch> {branch}`, or publish the branch with `pyn push -u origin {branch}`"
        );
    };
    let (Some(local), Some(theirs)) = (store.tip(&branch)?, store.published(&branch)?) else {
        return sync::update(backend, ws, &[], None);
    };
    let reachable = store.ancestors(Some(local))?;
    if reachable.contains(&theirs) {
        println!("Already up to date.");
        return Ok(());
    }
    let incoming = store
        .repo()
        .flatten(store.tree_of(Some(theirs))?.as_ref(), &Pathspec::default())?;
    pyn_gitwork::check_paths(incoming.keys().map(Vec::as_slice))?;
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
        return local_merge::begin_rebase(backend, ws, &store, &branch, outgoing, theirs, &who);
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
    let source = format!("origin/{up}");
    let joined = store.join_trees(backend, base, local, theirs, ("HEAD", &source))?;
    if !joined.tangled.is_empty() {
        return Err(Rejected(format!(
            "error: the pull would merge a file and a folder of the same name; nothing was changed\n\t{}",
            joined.tangled.join("\n\t")
        ))
        .into());
    }
    let api = backend.pyn_server("pull")?;
    let url = format!("{}/{}", api.base, api.repo.as_deref().unwrap_or_default());
    let into = if matches!(branch.as_str(), "main" | "master") {
        String::new()
    } else {
        format!(" into {branch}")
    };
    let message = format!("Merge branch '{up}' of {url}{into}\n");
    if !joined.conflicts.is_empty() {
        let stop = local_merge::Stop {
            orig: local,
            theirs,
            message,
        };
        return local_merge::stop_merge(backend, ws, &store, &branch, stop, joined);
    }
    let (tree, trees) = build_trees(&joined.files)?;
    let repo = store.repo();
    for (_, body) in trees {
        repo.objects().write(Kind::Tree, &body)?;
    }
    let record = Commit {
        tree,
        parents: vec![local, theirs],
        author: who.clone(),
        committer: who,
        extra_headers: Vec::new(),
        message: message.into_bytes(),
    };
    let tip = repo
        .objects()
        .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
    let merged = merged_files(&store, ws, local, theirs, tip)?;
    refs.update_checked(&local_ref(&branch), Expect::Id(local), &tip)
        .context("the local branch moved while pulling; run the command again")?;
    for path in joined.kept {
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
        let target = ws.dest(path)?;
        set_read_only(&target, false)?;
        ws.write_file(path, &bytes)?;
        let mode = api_mode(new.mode)?;
        localfile::apply(&target, path, mode)?;
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
        "push_too_large" => (
            format!(" ! [rejected]        {branch} -> {branch} (too many file changes)"),
            "The push changes more files than the server accepts at once.\n\
             hint: Push the commits in smaller batches.",
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
            "You need the checkin permission on this repository, and the `edit_policy` permission to change `.pyn/pyn.toml` on the default branch.",
        ),
        "invalid_rules" => (
            format!(" ! [rejected]        {branch} -> {branch} (invalid policy)"),
            "The policy file in your commits does not validate.\n\
             hint: Fix `.pyn/pyn.toml` and commit the fix, or drop the bad commit with `pyn reset HEAD~<n>` (see `pyn reset --help`), then push again.",
        ),
        _ => return err,
    };
    let text = format!(
        "{headline}\nerror: failed to push some refs: {}\nhint: {hint}\nnothing was pushed; the server is unchanged",
        e.message
    );
    if matches!(e.code.as_str(), "lock_required" | "lock_held") {
        LockRejected(text).into()
    } else {
        Rejected(text).into()
    }
}

/// `pyn push` with `local_store` on: uploads the blobs, then sends the local commits and trees in one request.
pub fn push(backend: &dyn Backend, ws: &Workspace, mut req: ServerPush) -> Result<()> {
    if req.delete
        && req.refspecs.is_empty()
        && let Some(only) = req.repository.take_if(|r| r != "origin")
    {
        req.refspecs.push(only);
    }
    merging::refuse_during_merge(ws, "push")?;
    let Some(store) = LocalStore::open(ws) else {
        bail!("the local store is empty; run `pyn fetch` first");
    };
    if let Some(remote) = &req.repository
        && remote != "origin"
    {
        bail!("'{remote}' does not appear to be a remote; the server is `origin`");
    }
    if req.delete || req.refspecs.iter().any(|r| r.starts_with(':')) {
        return delete_remote(backend, &store, &req);
    }
    let current = branch::name(Some(ws));
    let pairs: Vec<(String, String)> = if req.refspecs.is_empty() {
        let Some(remote) = store.upstream(&current)? else {
            if store.tip(&current)?.is_none() {
                bail!("You are not currently on a branch with commits.");
            }
            bail!(
                "The current branch {current} has no upstream branch.\n\
                 To push the current branch and set the remote as upstream, use\n\n    pyn push --set-upstream origin {current}"
            );
        };
        vec![(current.clone(), remote)]
    } else {
        req.refspecs
            .iter()
            .map(|r| {
                if r.starts_with('+') {
                    bail!("a forced push is not available on a pyn server");
                }
                let (src, dst) = r.split_once(':').unwrap_or((r, r));
                let dst = dst.strip_prefix("refs/heads/").unwrap_or(dst);
                let src = src.strip_prefix("refs/heads/").unwrap_or(src);
                Ok((src.to_string(), dst.to_string()))
            })
            .collect::<Result<_>>()?
    };
    for (local, remote) in &pairs {
        push_branch(backend, ws, &store, (local, remote))?;
        if req.set_upstream {
            store.set_upstream(local, Some(remote))?;
            println!("branch '{local}' set up to track 'origin/{remote}'.");
        }
    }
    Ok(())
}

/// `push --delete <branch>` and `push origin :<branch>`: removes branches from the server.
fn delete_remote(backend: &dyn Backend, store: &LocalStore, req: &ServerPush) -> Result<()> {
    let api = backend.pyn_server("push")?;
    if req.refspecs.is_empty() {
        bail!("--delete doesn't make sense without any refs");
    }
    let mut first = true;
    for spec in &req.refspecs {
        let name = spec.strip_prefix(':').unwrap_or(spec);
        let name = name.strip_prefix("refs/heads/").unwrap_or(name);
        if store.remote_tip(name)?.is_none() {
            bail!("error: unable to delete '{name}': remote ref does not exist");
        }
        let route = api.repo_route(&format!("/branches/{name}"))?;
        api.send(
            api.request(Method::DELETE, &route)
                .query(&[("force", true)]),
        )?;
        store
            .repo()
            .refs()
            .delete(&tracking_ref(name), Expect::Any)?;
        if first {
            println!(
                "To {}/{}",
                api.base,
                api.repo.as_deref().unwrap_or_default()
            );
            first = false;
        }
        println!(" - [deleted]         {name}");
    }
    Ok(())
}

/// Sends the commits of the local branch `local` that the server's `remote` lacks.
fn push_branch(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    (local, branch): (&str, &str),
) -> Result<()> {
    let Some(local_tip) = store.tip(local)? else {
        bail!("src refspec {local} does not match any");
    };
    let published = store.remote_tip(branch)?;
    if published.is_none() {
        bail!(
            "cannot publish '{local}' as '{branch}': the server has no branch of that name and a push cannot create one yet (#618)"
        );
    }
    let Some(outgoing) = store.outgoing_to(Some(local_tip), published)? else {
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
            branch,
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
            branch: Some(branch.to_string()),
            commits: commits.iter().map(|(id, body)| encode(id, body)).collect(),
            trees: trees.iter().map(|(id, body)| encode(id, body)).collect(),
        })
        .map_err(|e| explain_push(e, branch))?;
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
    store.repo().refs().update(&tracking_ref(branch), &head)?;
    if local != crate::branch::name(Some(ws)) {
        return print_pushed(backend, (published, head), (local, branch), &response);
    }
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
    print_pushed(backend, (published, head), (local, branch), &response)
}

fn print_pushed(
    backend: &dyn Backend,
    (published, head): (Option<ObjectId>, ObjectId),
    (local, branch): (&str, &str),
    response: &api::PushResponse,
) -> Result<()> {
    let api = backend.pyn_server("push")?;
    println!(
        "To {}/{}",
        api.base,
        api.repo.as_deref().unwrap_or_default()
    );
    println!(
        "   {}..{}  {local} -> {branch}",
        published.map_or_else(
            || "0".repeat(snapshot::ABBREV),
            |p| short(&p.to_hex()).to_string()
        ),
        short(&head.to_hex())
    );
    stranded::print(
        "The policy change left shared versions on other branches that can no longer merge:",
        &response.stranded,
    );
    stranded::print_released(&response.released_locks);
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

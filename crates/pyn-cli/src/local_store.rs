//! The partial local store of a pyn server workspace (opt-in `local_store`): a bare repository in
//! `.pyn/local_only/store/` with every commit and tree of the branch and blobs only for what is checked out.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use pyn_gitwork::{Entry, Pathspec, Repo};
use pyn_object::{
    Algorithm, Commit, FileMode, Kind, ObjectId, Signature, TreeEntry, encode_tree, hash_object,
};
use pyn_proto as api;

use crate::backend::Backend;
use crate::branch;
use crate::localfile;
use crate::workspace::{FileState, Workspace};

/// A pyn server's object ids are SHA-256.
pub(crate) const ALGORITHM: Algorithm = Algorithm::Sha256;
const PAGE: usize = 200;
/// Most pages and most unstored commits one fetch accepts from a server.
const MAX_PAGES: usize = 10_000;
const MAX_COMMITS: usize = 1_000_000;

fn too_much_history(pages: usize, commits: usize) -> anyhow::Error {
    anyhow::anyhow!(
        "the server sent more than {commits} commits or {pages} pages in one fetch; refusing to store them"
    )
}
const REMOTE: &str = "origin";

/// What the store needs from a pyn server, all of it already in the API.
pub trait Remote {
    fn fetch_commits(&self, before: Option<&str>, limit: usize) -> Result<api::CommitPage>;
    fn fetch_commit(&self, id: &str) -> Result<api::Commit>;
    fn fetch_snapshot(&self, commit: &str) -> Result<Vec<api::SnapshotEntry>>;
    fn fetch_object(&self, id: &str) -> Result<Vec<u8>>;
}

impl<T: Backend + ?Sized> Remote for T {
    fn fetch_commits(&self, before: Option<&str>, limit: usize) -> Result<api::CommitPage> {
        self.commit_page(before, limit)
    }

    fn fetch_commit(&self, id: &str) -> Result<api::Commit> {
        self.commit(id)
    }

    fn fetch_snapshot(&self, commit: &str) -> Result<Vec<api::SnapshotEntry>> {
        Ok(self.snapshot(Some(commit))?.1)
    }

    fn fetch_object(&self, id: &str) -> Result<Vec<u8>> {
        self.object(id)
    }
}

pub struct LocalStore {
    repo: Repo,
}

pub(crate) type Files = BTreeMap<Vec<u8>, Entry>;
pub(crate) type Trees = Vec<(ObjectId, Vec<u8>)>;

pub(crate) fn parse_id(hex: &str) -> Result<ObjectId> {
    let id = ObjectId::parse(hex).with_context(|| format!("{hex} is not an object id"))?;
    id.expect(ALGORITHM)?;
    Ok(id)
}

pub(crate) fn file_mode(mode: api::FileMode) -> FileMode {
    match mode {
        api::FileMode::Regular => FileMode::Regular,
        api::FileMode::Executable => FileMode::Executable,
        api::FileMode::Symlink => FileMode::Symlink,
    }
}

/// The tree objects of `files`, children first, and the root's id.
pub(crate) fn build_trees(files: &Files) -> Result<(ObjectId, Trees)> {
    let flat: Vec<(&[u8], Entry)> = files.iter().map(|(p, e)| (p.as_slice(), *e)).collect();
    let mut out = Vec::new();
    let root = build_level(&flat, &mut out, 1)?;
    Ok((root, out))
}

fn build_level(files: &[(&[u8], Entry)], out: &mut Trees, depth: usize) -> Result<ObjectId> {
    if depth > pyn_gitwork::MAX_DEPTH {
        bail!(
            "a path is nested more than {} folders deep",
            pyn_gitwork::MAX_DEPTH
        );
    }
    let mut entries = Vec::new();
    let mut i = 0;
    while i < files.len() {
        let (path, leaf) = files[i];
        let Some(at) = path.iter().position(|b| *b == b'/') else {
            entries.push(TreeEntry {
                mode: leaf.mode,
                name: path.to_vec(),
                id: leaf.id,
            });
            i += 1;
            continue;
        };
        let dir = &path[..at];
        let mut group = Vec::new();
        while i < files.len() && files[i].0.starts_with(dir) && files[i].0.get(at) == Some(&b'/') {
            group.push((&files[i].0[at + 1..], files[i].1));
            i += 1;
        }
        entries.push(TreeEntry {
            mode: FileMode::Tree,
            name: dir.to_vec(),
            id: build_level(&group, out, depth + 1)?,
        });
    }
    let body = encode_tree(ALGORITHM, &entries)?;
    let id = hash_object(ALGORITHM, Kind::Tree, &body)?;
    out.push((id, body));
    Ok(id)
}

fn signature(who: &api::Person, at: i64) -> Signature {
    Signature {
        name: who.name.clone().into_bytes(),
        email: who.email.clone().into_bytes(),
        time: at,
        offset_minutes: who.offset_minutes,
    }
}

/// The git commit the server stores for `c`, with `tree` and `parents` as ids.
fn git_commit(c: &api::Commit) -> Result<Commit> {
    let message = if c.message.ends_with('\n') {
        c.message.clone()
    } else {
        format!("{}\n", c.message)
    };
    let committed = c.created_at.timestamp();
    let authored = c.authored_at.map_or(committed, |t| t.timestamp());
    Ok(Commit {
        tree: parse_id(&c.tree)?,
        parents: c
            .parents
            .iter()
            .map(|p| parse_id(p))
            .collect::<Result<_>>()?,
        author: signature(&c.author, authored),
        committer: signature(&c.committer, committed),
        extra_headers: c
            .signature
            .iter()
            .map(|s| (b"gpgsig".to_vec(), s.clone().into_bytes()))
            .collect(),
        message: message.into_bytes(),
    })
}

impl LocalStore {
    pub fn path(ws: &Workspace) -> PathBuf {
        ws.store_path()
    }

    /// The store of a workspace that has the setting on and a store on disk.
    pub fn open(ws: &Workspace) -> Option<Self> {
        if !ws.settings().local_store_on() || !Self::path(ws).join("HEAD").is_file() {
            return None;
        }
        let store = Repo::open(&Self::path(ws), None, None)
            .ok()
            .map(|repo| Self { repo })?;
        let _ = store.adopt_upstreams();
        Some(store)
    }

    /// The store, created empty on the branch if the workspace has none yet.
    pub fn open_or_create(ws: &Workspace, branch: &str) -> Result<Self> {
        match Self::open(ws) {
            Some(s) => Ok(s),
            None => Self::create(ws, branch),
        }
    }

    fn create(ws: &Workspace, branch: &str) -> Result<Self> {
        let path = Self::path(ws);
        pyn_git::init(&path, true, ALGORITHM, branch)
            .with_context(|| format!("creating {}", path.display()))?;
        let store = Self {
            repo: Repo::open(&path, None, None)?,
        };
        store.adopt_upstreams()?;
        Ok(store)
    }

    pub(crate) fn repo(&self) -> &Repo {
        &self.repo
    }

    pub(crate) fn has(&self, id: &ObjectId) -> Result<bool> {
        Ok(self.repo.objects().contains(id)?)
    }

    /// Stores every commit of the branch the store lacks, with its trees, and moves the branch's refs to the tip.
    pub fn sync_history<R: Remote + ?Sized>(&self, remote: &R, branch: (&str, &str)) -> Result<()> {
        self.sync_bounded(remote, branch, (MAX_PAGES, MAX_COMMITS))
    }

    fn sync_bounded<R: Remote + ?Sized>(
        &self,
        remote: &R,
        branch: (&str, &str),
        (max_pages, max_commits): (usize, usize),
    ) -> Result<()> {
        let mut pending: HashMap<ObjectId, api::Commit> = HashMap::new();
        let mut tip = None;
        let mut before: Option<String> = None;
        for pages in 1usize.. {
            if pages > max_pages {
                return Err(too_much_history(max_pages, max_commits));
            }
            let page = remote.fetch_commits(before.as_deref(), PAGE)?;
            let mut fresh = 0;
            for c in page.commits {
                let id = parse_id(&c.id)?;
                tip.get_or_insert(id);
                if !self.has(&id)? {
                    fresh += 1;
                    pending.insert(id, c);
                }
            }
            if pending.len() > max_commits {
                return Err(too_much_history(max_pages, max_commits));
            }
            match page.next_before {
                Some(next) if fresh > 0 && before.as_ref() != Some(&next) => before = Some(next),
                _ => break,
            }
        }
        let Some(tip) = tip else { return Ok(()) };
        let mut wanted: Vec<ObjectId> = pending
            .values()
            .flat_map(|c| &c.parents)
            .map(|p| parse_id(p))
            .collect::<Result<_>>()?;
        while let Some(id) = wanted.pop() {
            if pending.contains_key(&id) || self.has(&id)? {
                continue;
            }
            if pending.len() > max_commits {
                return Err(too_much_history(max_pages, max_commits));
            }
            let c = remote.fetch_commit(&id.to_hex())?;
            for p in &c.parents {
                wanted.push(parse_id(p)?);
            }
            pending.insert(id, c);
        }
        for id in oldest_first(&pending)? {
            self.store_commit(remote, &pending[&id])?;
        }
        self.move_refs(branch, &tip)
    }

    /// Moves the server branch's tracking ref and, while it has no local commits, the local branch following it.
    fn move_refs(&self, (server, local): (&str, &str), tip: &ObjectId) -> Result<()> {
        let refs = self.repo.refs();
        let tracking = format!("refs/remotes/{REMOTE}/{server}");
        let local_ref = format!("refs/heads/{local}");
        let previous = refs.resolve(&tracking)?;
        refs.update(&tracking, tip)?;
        let current = refs.resolve(&local_ref)?;
        if current.is_none() {
            refs.update(&local_ref, tip)?;
            self.set_upstream(local, Some(server))?;
        } else if current == previous && self.upstream(local)?.as_deref() == Some(server) {
            refs.update(&local_ref, tip)?;
        }
        refs.set_symbolic("HEAD", &local_ref)?;
        Ok(())
    }

    fn store_commit<R: Remote + ?Sized>(&self, remote: &R, c: &api::Commit) -> Result<()> {
        let git = git_commit(c)?;
        let body = git.encode(ALGORITHM)?;
        if hash_object(ALGORITHM, Kind::Commit, &body)?.to_hex() != c.id {
            bail!(
                "commit {} cannot be rebuilt from what the server returns, so it is not stored",
                c.id
            );
        }
        let trees = self.trees_of(remote, c, &git)?;
        let objects = self.repo.objects();
        for (id, body) in trees {
            if objects.write(Kind::Tree, &body)? != id {
                bail!("a tree was stored under the wrong id");
            }
        }
        objects.write(Kind::Commit, &body)?;
        Ok(())
    }

    /// The trees of `c`: its first parent's tree with the commit's revisions applied, or its snapshot when that
    /// does not give the server's tree id (merges).
    fn trees_of<R: Remote + ?Sized>(
        &self,
        remote: &R,
        c: &api::Commit,
        git: &Commit,
    ) -> Result<Trees> {
        let mut files = match git.parents.first() {
            Some(p) => {
                let tree = self.repo.commit(p)?.tree;
                self.repo.flatten(Some(&tree), &Pathspec::default())?
            }
            None => Files::new(),
        };
        for rev in &c.revisions {
            let path = rev.path.clone().into_bytes();
            if rev.deleted {
                files.remove(&path);
            } else {
                files.insert(
                    path,
                    Entry {
                        mode: file_mode(rev.file_mode),
                        id: parse_id(&rev.content)?,
                    },
                );
            }
        }
        let (root, trees) = build_trees(&files)?;
        if root == git.tree {
            return Ok(trees);
        }
        let mut files = Files::new();
        for e in remote.fetch_snapshot(&c.id)? {
            files.insert(
                e.path.into_bytes(),
                Entry {
                    mode: file_mode(e.file_mode),
                    id: parse_id(&e.content)?,
                },
            );
        }
        let (root, trees) = build_trees(&files)?;
        if root != git.tree {
            bail!(
                "the tree of commit {} cannot be rebuilt from what the server returns, so it is not stored",
                c.id
            );
        }
        Ok(trees)
    }

    /// Stores the blob of every file the workspace has checked out and unchanged.
    pub fn store_checked_out(
        &self,
        ws: &Workspace,
        state: &BTreeMap<String, FileState>,
    ) -> Result<()> {
        let objects = self.repo.objects();
        for (path, known) in state.iter().filter(|(_, k)| !k.legacy) {
            let id = parse_id(&known.hash)?;
            if objects.contains(&id)? {
                continue;
            }
            let Ok((_, bytes)) = localfile::read(&ws.abs(path)) else {
                continue;
            };
            if known.matches(&bytes) {
                objects.write(Kind::Blob, &bytes)?;
            }
        }
        Ok(())
    }

    /// A blob by its full id: from the store, else read from the server by id, checked against it and stored.
    pub fn blob<R: Remote + ?Sized>(&self, remote: &R, id: &str) -> Result<Vec<u8>> {
        let id = parse_id(id)?;
        if let Some(found) = self.repo.objects().read_opt(&id)?
            && found.kind == Kind::Blob
        {
            return Ok(found.data);
        }
        let bytes = remote.fetch_object(&id.to_hex())?;
        if hash_object(ALGORITHM, Kind::Blob, &bytes)? != id {
            bail!("the content downloaded for {id} does not match it");
        }
        self.repo.objects().write(Kind::Blob, &bytes)?;
        Ok(bytes)
    }
}

/// Parents before children, among the commits in `pending`.
fn oldest_first(pending: &HashMap<ObjectId, api::Commit>) -> Result<Vec<ObjectId>> {
    let mut starts: Vec<&ObjectId> = pending.keys().collect();
    starts.sort_by_key(|id| (pending[id].created_at, id.to_hex()));
    let mut order = Vec::new();
    let mut done = HashSet::new();
    for start in starts {
        let mut stack = vec![(*start, false)];
        while let Some((id, expanded)) = stack.pop() {
            let Some(c) = pending.get(&id) else { continue };
            if done.contains(&id) {
                continue;
            }
            if expanded {
                done.insert(id);
                order.push(id);
            } else {
                stack.push((id, true));
                for p in &c.parents {
                    stack.push((parse_id(p)?, false));
                }
            }
        }
    }
    Ok(order)
}

/// Brings the store of a workspace with `local_store` on up to date: creates it, then stores history and blobs.
pub fn refresh<R: Remote + ?Sized>(remote: &R, ws: &Workspace) -> Result<()> {
    if !ws.settings().local_store_on() {
        return Ok(());
    }
    let branch = branch::name(Some(ws));
    let store = LocalStore::open_or_create(ws, &branch)?;
    let fresh = store.tip(&branch)?.is_none().then(|| branch.clone());
    if let Some(server) = branch::server_branch(Some(ws)).or(fresh) {
        store.sync_history(remote, (&server, &branch))?;
    }
    store.store_checked_out(ws, &ws.load_state()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    #[test]
    fn trees_are_built_to_a_fixed_depth_and_no_deeper() {
        let leaf = Entry {
            mode: FileMode::Regular,
            id: hash_object(ALGORITHM, Kind::Blob, b"x").unwrap(),
        };
        let at = |folders: usize| -> Files {
            let path = format!("{}f", "d/".repeat(folders));
            BTreeMap::from([(path.into_bytes(), leaf)])
        };
        let ok = pyn_gitwork::MAX_DEPTH - 1;
        assert_eq!(build_trees(&at(ok)).unwrap().1.len(), ok + 1);
        let err = build_trees(&at(ok + 1)).unwrap_err().to_string();
        assert!(err.contains("nested more than"), "{err}");
    }

    #[derive(Default)]
    struct Fake {
        commits: Vec<api::Commit>,
        objects: HashMap<String, Vec<u8>>,
        calls: std::cell::RefCell<Vec<String>>,
    }

    fn person() -> api::Person {
        api::Person {
            name: "Ann".into(),
            email: "ann@example.com".into(),
            offset_minutes: 60,
        }
    }

    fn blob_id(bytes: &[u8]) -> String {
        hash_object(ALGORITHM, Kind::Blob, bytes).unwrap().to_hex()
    }

    fn revision(n: u64, path: &str, bytes: &[u8], deleted: bool, commit: &str) -> api::Revision {
        api::Revision {
            id: n,
            path: path.into(),
            content: blob_id(bytes),
            legacy_content: None,
            author: "ann".into(),
            author_identity: None,
            hash_version: 3,
            message: "m".into(),
            created_at: Utc.timestamp_opt(1_700_000_000, 0).unwrap(),
            restored_from: None,
            mode: None,
            changeset: None,
            file_mode: api::FileMode::Regular,
            deleted,
            commit: commit.into(),
            hash: String::new(),
            previous: None,
            base: None,
            stranded: Vec::new(),
        }
    }

    /// A commit over `files` (the full tree after it) on `parent`, with the revisions it made.
    fn commit(
        parent: Option<&api::Commit>,
        files: &[(&str, &str)],
        revs: &[(&str, &str, bool)],
        at: i64,
    ) -> api::Commit {
        let map: Files = files
            .iter()
            .map(|(p, t)| {
                (
                    p.as_bytes().to_vec(),
                    Entry {
                        mode: FileMode::Regular,
                        id: parse_id(&blob_id(t.as_bytes())).unwrap(),
                    },
                )
            })
            .collect();
        let (tree, _) = build_trees(&map).unwrap();
        let mut c = api::Commit {
            id: String::new(),
            parents: parent.map(|p| p.id.clone()).into_iter().collect(),
            tree: tree.to_hex(),
            author: person(),
            committer: person(),
            created_at: Utc.timestamp_opt(at, 0).unwrap(),
            authored_at: None,
            message: format!("commit {at}"),
            signature: None,
            changeset: None,
            revisions: Vec::new(),
        };
        let body = git_commit(&c).unwrap().encode(ALGORITHM).unwrap();
        c.id = hash_object(ALGORITHM, Kind::Commit, &body)
            .unwrap()
            .to_hex();
        c.revisions = revs
            .iter()
            .enumerate()
            .map(|(i, (p, t, d))| revision(i as u64 + 1, p, t.as_bytes(), *d, &c.id))
            .collect();
        c
    }

    impl Remote for Fake {
        fn fetch_commits(&self, before: Option<&str>, limit: usize) -> Result<api::CommitPage> {
            self.calls.borrow_mut().push("commits".into());
            let start = before.map_or(0, |b| {
                self.commits.iter().position(|c| c.id == b).unwrap() + 1
            });
            let rest = &self.commits[start..];
            let take = rest.len().min(limit);
            Ok(api::CommitPage {
                commits: rest[..take].to_vec(),
                next_before: (take < rest.len()).then(|| rest[take - 1].id.clone()),
            })
        }

        fn fetch_commit(&self, id: &str) -> Result<api::Commit> {
            self.calls.borrow_mut().push(format!("commit {id}"));
            Ok(self.commits.iter().find(|c| c.id == id).unwrap().clone())
        }

        fn fetch_snapshot(&self, commit: &str) -> Result<Vec<api::SnapshotEntry>> {
            self.calls.borrow_mut().push(format!("snapshot {commit}"));
            Ok(Vec::new())
        }

        fn fetch_object(&self, id: &str) -> Result<Vec<u8>> {
            self.calls.borrow_mut().push(format!("object {id}"));
            self.objects
                .get(id)
                .cloned()
                .with_context(|| format!("object_not_found: {id}"))
        }
    }

    struct Endless(std::cell::Cell<u64>);

    impl Remote for Endless {
        fn fetch_commits(&self, _: Option<&str>, limit: usize) -> Result<api::CommitPage> {
            let commits = (0..limit)
                .map(|_| {
                    let n = self.0.get() + 1;
                    self.0.set(n);
                    let mut c = commit(None, &[("a", "x")], &[("a", "x", false)], n as i64);
                    c.id = format!("{n:064x}");
                    c
                })
                .collect();
            Ok(api::CommitPage {
                commits,
                next_before: Some(format!("{:064x}", self.0.get())),
            })
        }
        fn fetch_commit(&self, _: &str) -> Result<api::Commit> {
            unreachable!("every commit arrives in a page")
        }
        fn fetch_snapshot(&self, _: &str) -> Result<Vec<api::SnapshotEntry>> {
            unreachable!()
        }
        fn fetch_object(&self, _: &str) -> Result<Vec<u8>> {
            unreachable!()
        }
    }

    #[test]
    fn a_server_that_pages_without_end_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        let store = LocalStore::open_or_create(&ws, "main").unwrap();
        let remote = Endless(std::cell::Cell::new(0));
        let err = store
            .sync_bounded(&remote, ("main", "main"), (3, 1000))
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to store"), "{err}");
        assert_eq!(remote.0.get(), 3 * PAGE as u64);
        let err = store
            .sync_bounded(&Endless(Default::default()), ("main", "main"), (1000, 250))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than 250 commits"), "{err}");
    }

    fn workspace(dir: &std::path::Path) -> Workspace {
        let ws = Workspace::create(dir).unwrap();
        std::fs::write(ws.config_path(), "local_store = \"true\"\n").unwrap();
        ws
    }

    /// Two commits on a chain (newest first in the fake, as the server lists them).
    fn chain() -> Fake {
        let one = commit(
            None,
            &[("a.txt", "one"), ("dir/b.txt", "bee")],
            &[("a.txt", "one", false), ("dir/b.txt", "bee", false)],
            1_700_000_000,
        );
        let two = commit(
            Some(&one),
            &[("a.txt", "one"), ("dir/c.txt", "sea")],
            &[("dir/b.txt", "bee", true), ("dir/c.txt", "sea", false)],
            1_700_000_100,
        );
        let mut fake = Fake {
            commits: vec![two, one],
            ..Fake::default()
        };
        fake.objects.insert(blob_id(b"sea"), b"sea".to_vec());
        fake
    }

    #[test]
    fn the_author_time_comes_from_authored_at_and_the_committer_time_from_created_at() {
        let mut c = commit(None, &[("a.txt", "x")], &[], 2_000);
        c.authored_at = Some(Utc.timestamp_opt(1_000, 0).unwrap());
        let g = git_commit(&c).unwrap();
        assert_eq!((g.author.time, g.committer.time), (1_000, 2_000));
        c.authored_at = None;
        let g = git_commit(&c).unwrap();
        assert_eq!((g.author.time, g.committer.time), (2_000, 2_000));
    }

    #[test]
    fn history_is_stored_with_ids_matching_the_server_and_no_blobs() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        let fake = chain();
        refresh(&fake, &ws).unwrap();

        let store = LocalStore::open(&ws).unwrap();
        let (two, one) = (&fake.commits[0], &fake.commits[1]);
        let head = store.repo.head_commit().unwrap().unwrap();
        assert_eq!(head.to_hex(), two.id);
        let stored = store.repo.commit(&head).unwrap();
        assert_eq!(stored.tree.to_hex(), two.tree);
        assert_eq!(stored.parents[0].to_hex(), one.id);
        assert_eq!(stored.author.offset_minutes, 60);
        let files = store
            .repo
            .flatten(Some(&stored.tree), &Pathspec::default())
            .unwrap();
        assert_eq!(
            files.keys().map(|p| p.as_slice()).collect::<Vec<_>>(),
            [&b"a.txt"[..], b"dir/c.txt"]
        );
        let sea = parse_id(&blob_id(b"sea")).unwrap();
        assert!(
            !store.has(&sea).unwrap(),
            "blobs are not fetched with history"
        );
        let tracking = store
            .repo
            .refs()
            .resolve("refs/remotes/origin/main")
            .unwrap();
        assert_eq!(tracking, Some(head));
    }

    #[test]
    fn a_second_refresh_fetches_only_new_commits() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        let mut fake = chain();
        refresh(&fake, &ws).unwrap();
        let three = commit(
            Some(&fake.commits[0]),
            &[("a.txt", "one"), ("dir/c.txt", "sea"), ("e", "e")],
            &[("e", "e", false)],
            1_700_000_200,
        );
        fake.commits.insert(0, three.clone());
        fake.calls.borrow_mut().clear();
        refresh(&fake, &ws).unwrap();

        let store = LocalStore::open(&ws).unwrap();
        assert_eq!(
            store.repo.head_commit().unwrap().unwrap().to_hex(),
            three.id
        );
        assert!(
            fake.calls
                .borrow()
                .iter()
                .all(|c| !c.starts_with("snapshot")),
            "{:?}",
            fake.calls
        );
    }

    #[test]
    fn checked_out_files_are_stored_as_blobs_and_edited_ones_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one").unwrap();
        std::fs::write(dir.path().join("edited"), "changed").unwrap();
        let known = |text: &str| FileState {
            revision: 1,
            hash: blob_id(text.as_bytes()),
            legacy: false,
            mode: "shared".into(),
            file_mode: api::FileMode::Regular,
        };
        ws.save_state(&BTreeMap::from([
            ("a.txt".to_string(), known("one")),
            ("edited".to_string(), known("original")),
        ]))
        .unwrap();
        refresh(&chain(), &ws).unwrap();

        let store = LocalStore::open(&ws).unwrap();
        let has = |t: &str| {
            store
                .has(&parse_id(&blob_id(t.as_bytes())).unwrap())
                .unwrap()
        };
        assert!(has("one"));
        assert!(!has("original") && !has("changed"));
    }

    #[test]
    fn a_missing_blob_is_fetched_by_id_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        let fake = chain();
        refresh(&fake, &ws).unwrap();
        let store = LocalStore::open(&ws).unwrap();

        let id = blob_id(b"sea");
        assert_eq!(store.blob(&fake, &id).unwrap(), b"sea");
        assert!(store.has(&parse_id(&id).unwrap()).unwrap());
        fake.calls.borrow_mut().clear();
        assert_eq!(store.blob(&fake, &id).unwrap(), b"sea");
        assert!(fake.calls.borrow().is_empty(), "the second read is local");

        let err = store.blob(&fake, &blob_id(b"nowhere")).unwrap_err();
        assert!(err.to_string().contains("object_not_found"), "{err}");
    }

    #[test]
    fn content_that_does_not_match_the_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        let mut fake = chain();
        refresh(&fake, &ws).unwrap();
        fake.objects.insert(blob_id(b"sea"), b"forged".to_vec());
        let store = LocalStore::open(&ws).unwrap();
        let err = store.blob(&fake, &blob_id(b"sea")).unwrap_err();
        assert!(err.to_string().contains("does not match"), "{err}");
        assert!(!store.has(&parse_id(&blob_id(b"forged")).unwrap()).unwrap());
    }

    #[test]
    fn a_tree_that_cannot_be_rebuilt_fails_and_stores_nothing_of_the_commit() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        let mut fake = chain();
        fake.commits[1].revisions.clear();
        let err = refresh(&fake, &ws).unwrap_err();
        assert!(err.to_string().contains("cannot be rebuilt"), "{err}");
        let store = LocalStore::open(&ws).unwrap();
        assert!(store.repo.head_commit().unwrap().is_none());
    }

    #[test]
    fn without_the_setting_nothing_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::create(dir.path()).unwrap();
        refresh(&chain(), &ws).unwrap();
        assert!(!LocalStore::path(&ws).exists());
        assert!(LocalStore::open(&ws).is_none());
    }

    #[test]
    fn a_local_branch_with_its_own_commits_is_not_moved() {
        let dir = tempfile::tempdir().unwrap();
        let ws = workspace(dir.path());
        let mut fake = chain();
        refresh(&fake, &ws).unwrap();
        let store = LocalStore::open(&ws).unwrap();
        let local = store.repo.head_commit().unwrap().unwrap();
        let other = parse_id(&fake.commits[1].id).unwrap();
        store.repo.refs().update("refs/heads/main", &other).unwrap();
        let three = commit(
            Some(&fake.commits[0]),
            &[("a.txt", "one")],
            &[("dir/c.txt", "sea", true)],
            1_700_000_300,
        );
        fake.commits.insert(0, three.clone());
        refresh(&fake, &ws).unwrap();

        assert_eq!(store.repo.head_commit().unwrap(), Some(other));
        let tracking = store
            .repo
            .refs()
            .resolve("refs/remotes/origin/main")
            .unwrap();
        assert_eq!(tracking.unwrap().to_hex(), three.id);
        assert_ne!(local.to_hex(), three.id);
    }
}

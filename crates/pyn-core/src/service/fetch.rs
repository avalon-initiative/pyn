//! Enumerating what a client lacks (decision #300): the commits reachable from the wanted branches and not from the
//! client's haves, parents first in stored order, each with its pointers and the trees it adds.

use std::collections::{HashMap, HashSet};

use pyn_object::{ObjectId, parse_tree};
use sha2::{Digest, Sha256};

use super::RepoService;
use crate::error::{PynError, Result};
use crate::object::REPO_ALGORITHM;
use crate::types::{Commit, Revision};

pub const FETCH_DEFAULT_LIMIT: usize = 100;
pub const FETCH_MAX_LIMIT: usize = 500;
/// Haves per request; a client sends its branch heads, not its history.
pub const FETCH_MAX_HAVES: usize = 1000;
/// Commits one fetch request may examine; a larger walk is refused.
pub const FETCH_MAX_EXAMINED: usize = 1_000_000;

/// What a client asks for. With a `cursor` the branches are those it pinned and `branches` and `haves` must be empty.
#[derive(Debug, Default, Clone)]
pub struct FetchQuery {
    /// Branches to fetch; all of them when empty.
    pub branches: Vec<String>,
    /// Commits the client has; unknown ones are ignored.
    pub haves: Vec<ObjectId>,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

/// One missing commit: its exact git bytes, the pointers it records and the trees it adds against its first parent.
#[derive(Debug, Clone)]
pub struct FetchedCommit {
    pub id: ObjectId,
    pub raw: Vec<u8>,
    pub pointers: Vec<Revision>,
    pub trees: Vec<(ObjectId, Vec<u8>)>,
}

#[derive(Debug, Clone)]
pub struct FetchPage {
    /// The wanted branches and the heads the fetch is pinned to.
    pub refs: Vec<(String, Option<ObjectId>)>,
    pub commits: Vec<FetchedCommit>,
    pub next: Option<String>,
}

/// The state a cursor carries: the pinned heads, the haves it was made for and the last commit already sent.
struct Cursor {
    haves: String,
    refs: Vec<(String, Option<ObjectId>)>,
    last: Option<ObjectId>,
}

impl Cursor {
    fn encode(&self) -> String {
        let id = |i: &Option<ObjectId>| i.map_or("-".to_string(), |i| i.to_hex());
        let mut text = format!("2\n{}\n{}", id(&self.last), self.haves);
        for (name, head) in &self.refs {
            text.push_str(&format!("\n{name}\t{}", id(head)));
        }
        hex::encode(text)
    }

    fn decode(cursor: &str) -> Option<Self> {
        let text = String::from_utf8(hex::decode(cursor).ok()?).ok()?;
        let mut lines = text.split('\n');
        if lines.next()? != "2" {
            return None;
        }
        let id = |s: &str| match s {
            "-" => Some(None),
            s => ObjectId::from_hex(REPO_ALGORITHM, s).ok().map(Some),
        };
        let last = id(lines.next()?)?;
        let haves = lines.next()?.to_string();
        let refs = lines
            .map(|l| {
                let (name, head) = l.split_once('\t')?;
                Some((name.to_string(), id(head)?))
            })
            .collect::<Option<Vec<_>>>()?;
        (!refs.is_empty()).then_some(Self { haves, refs, last })
    }
}

/// A short digest of the have set, so a cursor is only good for the haves it was made with.
fn haves_digest(haves: &[ObjectId]) -> String {
    let mut hex: Vec<String> = haves.iter().map(ObjectId::to_hex).collect();
    hex.sort_unstable();
    hex.dedup();
    let digest = Sha256::digest(hex.join("\n"));
    hex::encode(&digest[..8])
}

fn bad_cursor() -> PynError {
    PynError::InvalidRequest("the cursor is not one this fetch gave".into())
}

impl RepoService {
    /// The missing commits of the wanted branches, parents before children, one page at a time. A cursor keeps the
    /// heads the first page saw, so a branch that moves meanwhile does not change the walk.
    pub async fn fetch(&self, query: &FetchQuery) -> Result<FetchPage> {
        if query.haves.len() > FETCH_MAX_HAVES {
            return Err(PynError::InvalidRequest(format!(
                "a fetch takes at most {FETCH_MAX_HAVES} haves"
            )));
        }
        let limit = query
            .limit
            .unwrap_or(FETCH_DEFAULT_LIMIT)
            .clamp(1, FETCH_MAX_LIMIT);
        let digest = haves_digest(&query.haves);
        let (refs, last) = match &query.cursor {
            Some(text) => {
                if !query.branches.is_empty() {
                    return Err(PynError::InvalidRequest(
                        "a cursor already names the branches".into(),
                    ));
                }
                let cursor = Cursor::decode(text)
                    .filter(|c| c.haves == digest)
                    .ok_or_else(bad_cursor)?;
                for head in cursor.refs.iter().filter_map(|(_, h)| h.as_ref()) {
                    self.meta
                        .get_commit(&self.repo, head)
                        .await?
                        .ok_or_else(bad_cursor)?;
                }
                (cursor.refs, cursor.last)
            }
            None => (self.wanted_heads(&query.branches).await?, None),
        };

        let tips: Vec<ObjectId> = refs.iter().filter_map(|(_, h)| *h).collect();
        let mut batch = self
            .meta
            .missing_commits(
                &self.repo,
                &tips,
                &query.haves,
                last.as_ref(),
                limit + 1,
                FETCH_MAX_EXAMINED,
            )
            .await
            .map_err(|e| match e {
                PynError::CommitNotFound(_) if last.is_some() => bad_cursor(),
                e => e,
            })?;
        let more = batch.len() > limit;
        batch.truncate(limit);
        let page: Vec<ObjectId> = batch.iter().map(|c| c.id).collect();
        let missing: HashMap<ObjectId, Commit> = batch.into_iter().map(|c| (c.id, c)).collect();

        let mut commits = Vec::with_capacity(page.len());
        for id in &page {
            let commit = &missing[id];
            commit.verify()?;
            let pointers = self.meta.commit_pointers(&self.repo, id).await?;
            pointers.iter().try_for_each(Revision::verify)?;
            let trees = self.added_trees(commit).await?;
            commits.push(FetchedCommit {
                id: *id,
                raw: commit.raw.clone(),
                pointers,
                trees,
            });
        }
        let next = match (more, page.last()) {
            (true, Some(last)) => Some(
                Cursor {
                    haves: digest,
                    refs: refs.clone(),
                    last: Some(*last),
                }
                .encode(),
            ),
            _ => None,
        };
        Ok(FetchPage {
            refs,
            commits,
            next,
        })
    }

    async fn wanted_heads(&self, names: &[String]) -> Result<Vec<(String, Option<ObjectId>)>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let branches = if names.is_empty() {
            self.branches().await?
        } else {
            let mut found = Vec::new();
            for name in names {
                found.push(self.branch(name).await?);
            }
            found
        };
        for b in branches {
            if seen.insert(b.name.clone()) {
                out.push((b.name, b.head));
            }
        }
        Ok(out)
    }

    /// The trees under the commit's root that its first parent's tree does not have at the same place.
    async fn added_trees(&self, commit: &Commit) -> Result<Vec<(ObjectId, Vec<u8>)>> {
        let parent_root = match commit.parents.first() {
            Some(p) => Some(
                self.meta
                    .get_commit(&self.repo, p)
                    .await?
                    .ok_or_else(|| PynError::CommitNotFound(p.to_string()))?
                    .tree,
            ),
            None => None,
        };
        let mut out = Vec::new();
        let mut stack = vec![(commit.tree, parent_root)];
        while let Some((tree, against)) = stack.pop() {
            if against == Some(tree) {
                continue;
            }
            let body = self.tree_body(&tree).await?;
            let old = match against {
                Some(id) => parse_tree(REPO_ALGORITHM, &self.tree_body(&id).await?)
                    .map_err(|e| PynError::Storage(format!("tree {id}: {e}")))?,
                None => Vec::new(),
            };
            let entries = parse_tree(REPO_ALGORITHM, &body)
                .map_err(|e| PynError::Storage(format!("tree {tree}: {e}")))?;
            let old_trees: HashMap<&[u8], ObjectId> = old
                .iter()
                .filter(|o| o.mode.is_tree())
                .map(|o| (o.name.as_slice(), o.id))
                .collect();
            for e in entries.iter().filter(|e| e.mode.is_tree()) {
                let before = old_trees.get(e.name.as_slice()).copied();
                stack.push((e.id, before));
            }
            out.push((tree, body));
        }
        Ok(out)
    }

    async fn tree_body(&self, id: &ObjectId) -> Result<Vec<u8>> {
        self.meta
            .get_tree(&self.repo, id)
            .await?
            .ok_or_else(|| PynError::Storage(format!("tree {id} is missing")))
    }
}

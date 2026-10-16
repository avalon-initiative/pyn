//! Enumerating what a client lacks (decision #300): the commits reachable from the wanted branches and not from the
//! client's haves, oldest first, each with its pointers and the trees it adds.

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};

use pyn_object::{ObjectId, parse_tree};

use super::RepoService;
use crate::error::{PynError, Result};
use crate::object::REPO_ALGORITHM;
use crate::types::{Commit, Revision};

pub const FETCH_DEFAULT_LIMIT: usize = 100;
pub const FETCH_MAX_LIMIT: usize = 500;
/// Haves per request; a client sends its branch heads, not its history.
pub const FETCH_MAX_HAVES: usize = 1000;

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

/// The state a cursor carries: the pinned heads and the last commit already sent.
struct Cursor {
    refs: Vec<(String, Option<ObjectId>)>,
    last: Option<ObjectId>,
}

impl Cursor {
    fn encode(&self) -> String {
        let id = |i: &Option<ObjectId>| i.map_or("-".to_string(), |i| i.to_hex());
        let mut text = format!("1\n{}", id(&self.last));
        for (name, head) in &self.refs {
            text.push_str(&format!("\n{name}\t{}", id(head)));
        }
        hex::encode(text)
    }

    fn decode(cursor: &str) -> Option<Self> {
        let text = String::from_utf8(hex::decode(cursor).ok()?).ok()?;
        let mut lines = text.split('\n');
        if lines.next()? != "1" {
            return None;
        }
        let id = |s: &str| match s {
            "-" => Some(None),
            s => ObjectId::from_hex(REPO_ALGORITHM, s).ok().map(Some),
        };
        let last = id(lines.next()?)?;
        let refs = lines
            .map(|l| {
                let (name, head) = l.split_once('\t')?;
                Some((name.to_string(), id(head)?))
            })
            .collect::<Option<Vec<_>>>()?;
        (!refs.is_empty()).then_some(Self { refs, last })
    }
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
        let (refs, last) = match &query.cursor {
            Some(text) => {
                if !query.branches.is_empty() {
                    return Err(PynError::InvalidRequest(
                        "a cursor already names the branches".into(),
                    ));
                }
                let cursor = Cursor::decode(text).ok_or_else(bad_cursor)?;
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

        let mut haves = Vec::new();
        for have in &query.haves {
            if !haves.contains(have) && self.meta.get_commit(&self.repo, have).await?.is_some() {
                haves.push(*have);
            }
        }
        let missing = self
            .missing_commits(refs.iter().filter_map(|(_, h)| *h), &haves)
            .await?;
        let order = parents_first(&missing);
        let start = match last {
            Some(id) => order.iter().position(|c| *c == id).ok_or_else(bad_cursor)? + 1,
            None => 0,
        };
        let page: Vec<ObjectId> = order[start..].iter().take(limit).copied().collect();
        let more = start + page.len() < order.len();

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

    /// Commits reachable from `tips` that no have descends from. A commit a have descends from has all its ancestors
    /// excluded too, so the walk stops there.
    async fn missing_commits(
        &self,
        tips: impl Iterator<Item = ObjectId>,
        haves: &[ObjectId],
    ) -> Result<HashMap<ObjectId, Commit>> {
        let mut found = HashMap::new();
        let mut skipped = HashSet::new();
        let mut stack: Vec<ObjectId> = tips.collect();
        while let Some(id) = stack.pop() {
            if found.contains_key(&id) || skipped.contains(&id) {
                continue;
            }
            let mut owned = false;
            for have in haves {
                if self.meta.is_ancestor(&self.repo, &id, have).await? {
                    owned = true;
                    break;
                }
            }
            if owned {
                skipped.insert(id);
                continue;
            }
            let commit = self
                .meta
                .get_commit(&self.repo, &id)
                .await?
                .ok_or_else(|| PynError::CommitNotFound(id.to_string()))?;
            stack.extend(commit.parents.iter().copied());
            found.insert(id, commit);
        }
        Ok(found)
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
            for e in entries.iter().filter(|e| e.mode.is_tree()) {
                let before = old
                    .iter()
                    .find(|o| o.name == e.name && o.mode.is_tree())
                    .map(|o| o.id);
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

/// Parents before children; among the commits ready, the earlier committer time first, then the id.
fn parents_first(commits: &HashMap<ObjectId, Commit>) -> Vec<ObjectId> {
    let mut waiting: HashMap<ObjectId, usize> = HashMap::new();
    let mut children: HashMap<ObjectId, Vec<ObjectId>> = HashMap::new();
    let mut ready: BTreeSet<(DateTime<Utc>, String)> = BTreeSet::new();
    for (id, c) in commits {
        let parents: HashSet<_> = c
            .parents
            .iter()
            .filter(|p| commits.contains_key(p))
            .collect();
        if parents.is_empty() {
            ready.insert((c.created_at, id.to_hex()));
        }
        waiting.insert(*id, parents.len());
        for p in parents {
            children.entry(*p).or_default().push(*id);
        }
    }
    let mut out = Vec::with_capacity(commits.len());
    while let Some((_, hex)) = ready.pop_first() {
        let id = ObjectId::from_hex(REPO_ALGORITHM, &hex).expect("written by to_hex");
        out.push(id);
        for child in children.get(&id).into_iter().flatten() {
            let n = waiting.get_mut(child).expect("every child is counted");
            *n -= 1;
            if *n == 0 {
                ready.insert((commits[child].created_at, child.to_hex()));
            }
        }
    }
    out
}

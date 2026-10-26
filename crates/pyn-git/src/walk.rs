use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use pyn_object::{Algorithm, Kind, ObjectId};

use crate::error::{Result, corrupt};
use crate::odb::ObjectDb;

const MAX_TAG_CHAIN: usize = 1000;
const SLOP: usize = 5;

/// Commits ordered newest first, ties in insertion order.
#[derive(Default)]
struct Queue {
    heap: BinaryHeap<(i64, Reverse<usize>)>,
    ids: Vec<ObjectId>,
}

impl Queue {
    fn push(&mut self, time: i64, id: ObjectId) {
        self.heap.push((time, Reverse(self.ids.len())));
        self.ids.push(id);
    }

    fn pop(&mut self) -> Option<ObjectId> {
        self.heap.pop().map(|(_, Reverse(i))| self.ids[i])
    }

    fn iter(&self) -> impl Iterator<Item = &ObjectId> {
        self.heap.iter().map(|(_, Reverse(i))| &self.ids[*i])
    }
}

struct CommitInfo {
    tree: ObjectId,
    parents: Vec<ObjectId>,
    time: i64,
}

fn parse_commit(alg: Algorithm, id: &ObjectId, body: &[u8]) -> Result<CommitInfo> {
    let bad = |d: &str| corrupt("commit", format!("{id}: {d}"));
    let hex_id = |v: &[u8]| {
        std::str::from_utf8(v)
            .ok()
            .and_then(|h| ObjectId::from_hex(alg, h).ok())
    };
    let mut tree = None;
    let mut parents = Vec::new();
    let mut time = 0;
    for line in body.split(|b| *b == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix(b"tree ") {
            tree = Some(hex_id(v).ok_or_else(|| bad("bad tree id"))?);
        } else if let Some(v) = line.strip_prefix(b"parent ") {
            parents.push(hex_id(v).ok_or_else(|| bad("bad parent id"))?);
        } else if let Some(v) = line.strip_prefix(b"committer ") {
            let mut it = v.rsplit(|b| *b == b' ');
            let _tz = it.next();
            time = it
                .next()
                .and_then(|t| std::str::from_utf8(t).ok())
                .and_then(|t| t.parse().ok())
                .unwrap_or(0);
        }
    }
    Ok(CommitInfo {
        tree: tree.ok_or_else(|| bad("missing tree"))?,
        parents,
        time,
    })
}

fn tag_target(alg: Algorithm, id: &ObjectId, body: &[u8]) -> Result<ObjectId> {
    body.strip_prefix(b"object ")
        .and_then(|r| r.split(|b| *b == b'\n').next())
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|h| ObjectId::from_hex(alg, h).ok())
        .ok_or_else(|| corrupt("tag", format!("{id}: bad object line")))
}

enum Child {
    Tree(ObjectId),
    Blob(ObjectId),
}

/// Subtrees and blobs of a tree body in stored order; submodule entries are skipped.
fn tree_children(alg: Algorithm, id: &ObjectId, body: &[u8]) -> Result<Vec<Child>> {
    let bad = || corrupt("tree", format!("{id}: malformed entry"));
    let n = alg.digest_len();
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let sp = rest.iter().position(|b| *b == b' ').ok_or_else(bad)?;
        let mode = std::str::from_utf8(&rest[..sp])
            .ok()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .ok_or_else(bad)?;
        rest = &rest[sp + 1..];
        let nul = rest.iter().position(|b| *b == 0).ok_or_else(bad)?;
        rest = &rest[nul + 1..];
        let raw = rest.get(..n).ok_or_else(bad)?;
        let child = ObjectId::from_bytes(alg, raw)?;
        rest = &rest[n..];
        match mode & 0o170000 {
            0o040000 => out.push(Child::Tree(child)),
            0o160000 => {}
            _ => out.push(Child::Blob(child)),
        }
    }
    Ok(out)
}

/// Follows tags from `id` to the first non-tag object; returns the tags passed and the target with its kind.
pub fn peel(db: &ObjectDb, id: ObjectId) -> Result<(Vec<ObjectId>, ObjectId, Kind)> {
    let mut tags = Vec::new();
    let mut cur = id;
    for _ in 0..MAX_TAG_CHAIN {
        let o = db.read(&cur)?;
        if o.kind != Kind::Tag {
            return Ok((tags, cur, o.kind));
        }
        tags.push(cur);
        cur = tag_target(db.algorithm(), &cur, &o.data)?;
    }
    Err(corrupt("tag", format!("{id}: chain too long")))
}

/// The objects a `want`/`have` pair needs sent, and the `have` side of its edge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reachable {
    /// Tags, then commits newest first, then trees and blobs, each once.
    pub objects: Vec<ObjectId>,
    /// Commits excluded by `have` that are parents of a commit in `objects`.
    pub boundary: Vec<ObjectId>,
}

struct Walker<'a> {
    db: &'a ObjectDb,
    commits: HashMap<ObjectId, CommitInfo>,
}

impl Walker<'_> {
    fn commit(&mut self, id: &ObjectId) -> Result<&CommitInfo> {
        if !self.commits.contains_key(id) {
            let o = self.db.read(id)?;
            if o.kind != Kind::Commit {
                return Err(corrupt("commit", format!("{id}: not a commit")));
            }
            let mut info = parse_commit(self.db.algorithm(), id, &o.data)?;
            if self.db.is_shallow(id) {
                info.parents.clear();
            }
            self.commits.insert(*id, info);
        }
        Ok(&self.commits[id])
    }

    /// Adds `root` and everything below it to `seen`, appending newly seen ids to `out` in tree order.
    fn walk_tree(
        &self,
        root: ObjectId,
        seen: &mut HashSet<ObjectId>,
        mut out: Option<&mut Vec<ObjectId>>,
    ) -> Result<()> {
        if !seen.insert(root) {
            return Ok(());
        }
        if let Some(o) = out.as_deref_mut() {
            o.push(root);
        }
        let mut stack = vec![root];
        while let Some(t) = stack.pop() {
            let obj = self.db.read(&t)?;
            if obj.kind != Kind::Tree {
                return Err(corrupt("tree", format!("{t}: not a tree")));
            }
            let kids = tree_children(self.db.algorithm(), &t, &obj.data)?;
            let mut subtrees = Vec::new();
            for k in kids {
                match k {
                    Child::Blob(b) => {
                        if seen.insert(b)
                            && let Some(o) = out.as_deref_mut()
                        {
                            o.push(b);
                        }
                    }
                    Child::Tree(s) => {
                        if seen.insert(s) {
                            if let Some(o) = out.as_deref_mut() {
                                o.push(s);
                            }
                            subtrees.push(s);
                        }
                    }
                }
            }
            stack.extend(subtrees.into_iter().rev());
        }
        Ok(())
    }
}

/// Like `git rev-list --objects --objects-edge-aggressive want ^have`.
///
/// Tags are peeled. A `have` this repository does not hold is ignored; any other missing object is an error.
pub fn reachable_objects(db: &ObjectDb, want: &[ObjectId], have: &[ObjectId]) -> Result<Reachable> {
    let mut w = Walker {
        db,
        commits: HashMap::new(),
    };
    let mut seen: HashSet<ObjectId> = HashSet::new();
    let mut uninteresting: HashMap<ObjectId, bool> = HashMap::new();
    let mut heap = Queue::default();
    let mut have_commits = Vec::new();

    for h in have {
        if !db.contains(h)? {
            continue;
        }
        let (tags, target, kind) = peel(db, *h)?;
        seen.extend(tags);
        match kind {
            Kind::Commit => have_commits.push(target),
            Kind::Tree => w.walk_tree(target, &mut seen, None)?,
            _ => {
                seen.insert(target);
            }
        }
    }

    let mut out = Vec::new();
    let mut want_trees = Vec::new();
    let mut want_blobs = Vec::new();
    let mut want_commits = Vec::new();
    for x in want {
        let (tags, target, kind) = peel(db, *x)?;
        for t in tags {
            if seen.insert(t) {
                out.push(t);
            }
        }
        match kind {
            Kind::Commit => want_commits.push(target),
            Kind::Tree => want_trees.push(target),
            _ => want_blobs.push(target),
        }
    }

    let push = |w: &mut Walker,
                heap: &mut Queue,
                id: ObjectId,
                un: bool,
                m: &mut HashMap<ObjectId, bool>|
     -> Result<()> {
        match m.get_mut(&id) {
            Some(flag) => {
                if un && !*flag {
                    *flag = true;
                    let mut stack = vec![id];
                    while let Some(c) = stack.pop() {
                        let parents = w.commit(&c)?.parents.clone();
                        for p in parents {
                            if let Some(f) = m.get_mut(&p)
                                && !*f
                            {
                                *f = true;
                                stack.push(p);
                            }
                        }
                    }
                }
            }
            None => {
                let time = w.commit(&id)?.time;
                m.insert(id, un);
                heap.push(time, id);
            }
        }
        Ok(())
    };
    for c in &have_commits {
        push(&mut w, &mut heap, *c, true, &mut uninteresting)?;
    }
    for c in &want_commits {
        push(&mut w, &mut heap, *c, false, &mut uninteresting)?;
    }

    let mut commits = Vec::new();
    let mut slop = SLOP;
    while let Some(id) = heap.pop() {
        let un = uninteresting[&id];
        let parents = w.commit(&id)?.parents.clone();
        if !un {
            commits.push(id);
        }
        for p in parents {
            push(&mut w, &mut heap, p, un, &mut uninteresting)?;
        }
        if heap.iter().all(|i| uninteresting[i]) {
            if slop == 0 {
                break;
            }
            slop -= 1;
        } else {
            slop = SLOP;
        }
    }
    commits.retain(|c| !uninteresting[c]);

    let mut boundary = Vec::new();
    let mut edge = HashSet::new();
    for c in &commits {
        for p in w.commit(c)?.parents.clone() {
            if uninteresting[&p] && edge.insert(p) {
                boundary.push(p);
                let t = w.commit(&p)?.tree;
                w.walk_tree(t, &mut seen, None)?;
            }
        }
    }
    for c in &have_commits {
        let t = w.commit(c)?.tree;
        w.walk_tree(t, &mut seen, None)?;
    }

    for c in &commits {
        out.push(*c);
    }
    for c in &commits {
        let t = w.commit(c)?.tree;
        w.walk_tree(t, &mut seen, Some(&mut out))?;
    }
    for t in want_trees {
        w.walk_tree(t, &mut seen, Some(&mut out))?;
    }
    for b in want_blobs {
        if seen.insert(b) {
            out.push(b);
        }
    }
    Ok(Reachable {
        objects: out,
        boundary,
    })
}

/// Commits reachable from `tips` (tags peeled), newest committer time first, at most `limit`.
pub fn commits_newest_first(
    db: &ObjectDb,
    tips: &[ObjectId],
    limit: usize,
) -> Result<Vec<ObjectId>> {
    let mut w = Walker {
        db,
        commits: HashMap::new(),
    };
    let mut seen = HashSet::new();
    let mut heap = Queue::default();
    for t in tips {
        let (_, target, kind) = peel(db, *t)?;
        if kind == Kind::Commit && seen.insert(target) {
            heap.push(w.commit(&target)?.time, target);
        }
    }
    let mut out = Vec::new();
    while out.len() < limit {
        let Some(id) = heap.pop() else { break };
        out.push(id);
        for p in w.commit(&id)?.parents.clone() {
            if seen.insert(p) {
                heap.push(w.commit(&p)?.time, p);
            }
        }
    }
    Ok(out)
}

/// How a shallow request limits history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deepen {
    /// `n` commits from each want, or from each current shallow commit when `relative`.
    Depth { n: u32, relative: bool },
    /// Commits with a committer time at or after this.
    Since(i64),
    /// Commits not reachable from these.
    Not(Vec<ObjectId>),
}

/// What a shallow request changes: new boundary commits and former boundary commits that now have parents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShallowCut {
    pub shallow: Vec<ObjectId>,
    pub unshallow: Vec<ObjectId>,
}

/// The depth git sends for `--unshallow`.
pub const INFINITE_DEPTH: u32 = 0x7fff_ffff;

/// Computes the boundary a shallow request cuts `wants` at, given the commits the client already holds as shallow.
pub fn shallow_cut(
    db: &ObjectDb,
    wants: &[ObjectId],
    client: &HashSet<ObjectId>,
    deepen: &Deepen,
) -> Result<ShallowCut> {
    let mut w = Walker {
        db,
        commits: HashMap::new(),
    };
    let mut starts = Vec::new();
    for x in wants {
        let (_, target, kind) = peel(db, *x)?;
        if kind == Kind::Commit {
            starts.push(target);
        }
    }
    let excluded = match deepen {
        Deepen::Not(refs) => {
            let mut set = HashSet::new();
            let mut stack = Vec::new();
            for r in refs {
                let (_, t, k) = peel(db, *r)?;
                if k == Kind::Commit && set.insert(t) {
                    stack.push(t);
                }
            }
            while let Some(c) = stack.pop() {
                for p in w.commit(&c)?.parents.clone() {
                    if set.insert(p) {
                        stack.push(p);
                    }
                }
            }
            set
        }
        _ => HashSet::new(),
    };
    let (limit, relative) = match deepen {
        Deepen::Depth { n, relative } => (u64::from(*n), *relative),
        _ => (u64::MAX, false),
    };
    let limit = if relative {
        limit.saturating_add(1)
    } else {
        limit
    };
    if relative {
        let mut sorted: Vec<ObjectId> = client.iter().copied().collect();
        sorted.sort_by_key(ObjectId::to_hex);
        starts.extend(sorted);
    }
    let mut depth: HashMap<ObjectId, u64> = HashMap::new();
    let mut queue = std::collections::VecDeque::new();
    for s in starts {
        let keep = match deepen {
            Deepen::Since(t) => w.commit(&s)?.time >= *t,
            Deepen::Not(_) => !excluded.contains(&s),
            Deepen::Depth { .. } => true,
        };
        if keep && depth.insert(s, 1).is_none() {
            queue.push_back(s);
        }
    }
    let mut shallow = Vec::new();
    let mut reached = Vec::new();
    while let Some(c) = queue.pop_front() {
        reached.push(c);
        let d = depth[&c];
        let parents = w.commit(&c)?.parents.clone();
        if parents.is_empty() && !matches!(deepen, Deepen::Depth { .. }) {
            continue;
        }
        let cut = match deepen {
            Deepen::Depth { .. } => d >= limit,
            Deepen::Since(t) => {
                let mut older = false;
                for p in &parents {
                    older |= w.commit(p)?.time < *t;
                }
                older
            }
            Deepen::Not(_) => parents.iter().any(|p| excluded.contains(p)),
        };
        if cut {
            shallow.push(c);
            continue;
        }
        for p in parents {
            if depth.contains_key(&p) {
                continue;
            }
            depth.insert(p, d + 1);
            queue.push_back(p);
        }
    }
    if reached.is_empty() {
        return Err(corrupt(
            "shallow request",
            "no commits selected for shallow requests",
        ));
    }
    let boundary: HashSet<ObjectId> = shallow.iter().copied().collect();
    let mut unshallow: Vec<ObjectId> = reached
        .iter()
        .filter(|c| client.contains(c) && !boundary.contains(c))
        .copied()
        .collect();
    unshallow.sort_by_key(ObjectId::to_hex);
    shallow.retain(|c| !client.contains(c));
    shallow.sort_by_key(ObjectId::to_hex);
    Ok(ShallowCut { shallow, unshallow })
}

/// The parents `id` has when not treated as shallow.
pub fn real_parents(db: &ObjectDb, id: &ObjectId) -> Result<Vec<ObjectId>> {
    let o = db.read(id)?;
    Ok(parse_commit(db.algorithm(), id, &o.data)?.parents)
}

/// The ids of `ids` that this repository holds, in the order given.
pub fn existing(db: &ObjectDb, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
    let mut out = Vec::new();
    for id in ids {
        if db.contains(id)? {
            out.push(*id);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_children_skip_gitlinks_and_read_padded_modes() {
        let alg = Algorithm::Sha1;
        let id = |b: u8| ObjectId::from_bytes(alg, &[b; 20]).unwrap();
        let mut body = Vec::new();
        for (mode, name, b) in [
            ("040000", "d", 1u8),
            ("100644", "f", 2),
            ("160000", "sub", 3),
            ("120000", "l", 4),
        ] {
            body.extend_from_slice(format!("{mode} {name}\0").as_bytes());
            body.extend_from_slice(&[b; 20]);
        }
        let kids = tree_children(alg, &id(0), &body).unwrap();
        assert_eq!(kids.len(), 3);
        assert!(matches!(kids[0], Child::Tree(t) if t == id(1)));
        assert!(matches!(kids[2], Child::Blob(b) if b == id(4)));
        assert!(tree_children(alg, &id(0), &body[..body.len() - 1]).is_err());
    }

    #[test]
    fn commit_header_is_read_leniently() {
        let alg = Algorithm::Sha1;
        let t = "aa".repeat(20);
        let p = "bb".repeat(20);
        let body = format!(
            "tree {t}\nparent {p}\nauthor A <a@b> 1 +0000\ncommitter C <c@d> 1700000000 +0200\nodd-header x\n\nmsg"
        );
        let info =
            parse_commit(alg, &ObjectId::from_hex(alg, &t).unwrap(), body.as_bytes()).unwrap();
        assert_eq!(info.time, 1_700_000_000);
        assert_eq!(info.parents.len(), 1);
    }
}

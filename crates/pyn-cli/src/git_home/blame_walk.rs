//! Passing blame from a commit to its parents, line by line, as git's `blame.c` does: a parent that has the same
//! file takes all of it, else the lines it still shares with the file take it, in parent order.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::rc::Rc;

use anyhow::Result;
use pyn_gitwork::{Entry, Pathspec};
use pyn_object::{Commit, FileMode, ObjectId};

use super::GitHome;
use super::blame_fuzzy::{Fingerprint, fingerprints, guess_line_blames};
use super::blame_rename::find_source;
use crate::xdiff::hunks_ws;

pub(super) struct Origin {
    pub commit: ObjectId,
    pub path: Vec<u8>,
    blob: ObjectId,
    mode: FileMode,
    /// `(line in this file, line in the final file)`, by line in this file.
    suspects: Vec<(usize, usize)>,
    pub previous: Option<usize>,
    pub guilty: bool,
}

/// The work tree file standing in as a commit on top of `parents`.
pub(super) struct Fake {
    pub parents: Vec<ObjectId>,
    pub time: i64,
    pub content: Rc<Vec<u8>>,
    /// What the index holds, with the blamed path replaced by the file's content.
    pub tree: BTreeMap<Vec<u8>, Entry>,
}

#[derive(Clone, Copy)]
pub(super) struct Settings {
    pub ignore_whitespace: bool,
    pub indent_heuristic: bool,
    pub first_parent: bool,
    pub minimal: bool,
    pub reverse: bool,
}

struct Queued {
    time: i64,
    seq: Reverse<u64>,
    commit: ObjectId,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.time, &self.seq) == (other.time, &other.seq)
    }
}

impl Eq for Queued {}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.time, &self.seq).cmp(&(other.time, &other.seq))
    }
}

pub(super) struct Board<'a> {
    git: &'a GitHome,
    hidden: &'a HashSet<ObjectId>,
    settings: Settings,
    fake: Option<(ObjectId, Fake)>,
    pub origins: Vec<Origin>,
    index: HashMap<(ObjectId, Vec<u8>), usize>,
    by_commit: HashMap<ObjectId, Vec<usize>>,
    queue: BinaryHeap<Queued>,
    seq: u64,
    blobs: HashMap<ObjectId, Rc<Vec<u8>>>,
    commits: HashMap<ObjectId, Rc<Commit>>,
    /// Per line of the final file, the origin and its line number that the line is blamed on.
    pub lines: Vec<Option<(usize, usize)>>,
    /// Per line of the final file: blamed past an ignored commit, or kept on one that no parent line matched.
    pub ignored: Vec<bool>,
    pub unblamable: Vec<bool>,
    ignore: HashSet<ObjectId>,
    /// With `--reverse`, a commit's children in the range, which take the place of its parents.
    children: HashMap<ObjectId, Vec<ObjectId>>,
    /// `-S`: commits whose parents are replaced.
    grafts: HashMap<ObjectId, Vec<ObjectId>>,
    prints: HashMap<usize, Vec<Fingerprint>>,
    /// The origins that ended up with lines, in the order the walk settled them, with those lines.
    pub settled: Vec<(usize, Vec<(usize, usize)>)>,
    /// Per final line, the piece it travels in: lines passed on in one step from one piece stay one piece.
    pub piece: Vec<u64>,
}

fn is_link(mode: FileMode) -> bool {
    mode == FileMode::Symlink
}

impl<'a> Board<'a> {
    pub fn new(
        git: &'a GitHome,
        hidden: &'a HashSet<ObjectId>,
        settings: Settings,
        fake: Option<(ObjectId, Fake)>,
        line_count: usize,
    ) -> Self {
        Self {
            git,
            hidden,
            settings,
            fake,
            origins: Vec::new(),
            index: HashMap::new(),
            by_commit: HashMap::new(),
            queue: BinaryHeap::new(),
            seq: 0,
            blobs: HashMap::new(),
            commits: HashMap::new(),
            lines: vec![None; line_count],
            ignored: vec![false; line_count],
            unblamable: vec![false; line_count],
            ignore: HashSet::new(),
            children: HashMap::new(),
            grafts: HashMap::new(),
            prints: HashMap::new(),
            settled: Vec::new(),
            piece: vec![0; line_count],
        }
    }

    pub fn ignoring(&mut self, commits: HashSet<ObjectId>) {
        self.ignore = commits;
    }

    pub fn with_grafts(&mut self, grafts: HashMap<ObjectId, Vec<ObjectId>>) {
        self.grafts = grafts;
    }

    pub fn with_children(&mut self, children: HashMap<ObjectId, Vec<ObjectId>>) {
        self.children = children;
    }

    fn commit(&mut self, id: &ObjectId) -> Result<Rc<Commit>> {
        if let Some(c) = self.commits.get(id) {
            return Ok(c.clone());
        }
        let mut commit = self.git.repo.commit(id)?;
        if let Some(parents) = self.grafts.get(id) {
            commit.parents = parents.clone();
        }
        let c = Rc::new(commit);
        self.commits.insert(*id, c.clone());
        Ok(c)
    }

    fn parents_of(&mut self, id: &ObjectId) -> Result<Vec<ObjectId>> {
        if self.settings.reverse {
            return Ok(self.children.get(id).cloned().unwrap_or_default());
        }
        if let Some((f, fake)) = &self.fake
            && f == id
        {
            return Ok(self.limit(fake.parents.clone()));
        }
        let parents = self.commit(id)?.parents.clone();
        Ok(self.limit(parents))
    }

    fn limit(&self, mut parents: Vec<ObjectId>) -> Vec<ObjectId> {
        if self.settings.first_parent {
            parents.truncate(1);
        }
        parents
    }

    fn time_of(&mut self, id: &ObjectId) -> Result<i64> {
        if let Some((f, fake)) = &self.fake
            && f == id
        {
            return Ok(fake.time);
        }
        Ok(self.commit(id)?.committer.time)
    }

    fn origin(&mut self, commit: ObjectId, path: &[u8], blob: ObjectId, mode: FileMode) -> usize {
        if let Some(i) = self.index.get(&(commit, path.to_vec())) {
            return *i;
        }
        let i = self.origins.len();
        self.origins.push(Origin {
            commit,
            path: path.to_vec(),
            blob,
            mode,
            suspects: Vec::new(),
            previous: None,
            guilty: false,
        });
        self.index.insert((commit, path.to_vec()), i);
        self.by_commit.entry(commit).or_default().push(i);
        i
    }

    /// Starts the blame: the final file's origin with `lines` as its suspects.
    pub fn start(
        &mut self,
        commit: ObjectId,
        path: &[u8],
        blob: ObjectId,
        mode: FileMode,
        lines: impl Iterator<Item = usize>,
    ) -> Result<usize> {
        let o = self.origin(commit, path, blob, mode);
        self.origins[o].suspects = lines.map(|l| (l, l)).collect();
        let time = self.time_of(&commit)?;
        self.push(commit, time);
        Ok(o)
    }

    fn push(&mut self, commit: ObjectId, time: i64) {
        self.seq += 1;
        self.queue.push(Queued {
            time: if self.settings.reverse { -time } else { time },
            seq: Reverse(self.seq),
            commit,
        });
    }

    fn data(&mut self, o: usize) -> Result<Rc<Vec<u8>>> {
        let (commit, blob) = (self.origins[o].commit, self.origins[o].blob);
        if let Some((f, fake)) = &self.fake
            && *f == commit
        {
            return Ok(fake.content.clone());
        }
        if let Some(d) = self.blobs.get(&blob) {
            return Ok(d.clone());
        }
        let d = Rc::new(self.git.repo.blob(&blob)?);
        self.blobs.insert(blob, d.clone());
        Ok(d)
    }

    pub fn run(&mut self) -> Result<()> {
        while let Some(q) = self.queue.pop() {
            while let Some(o) = self.by_commit.get(&q.commit).and_then(|v| {
                v.iter()
                    .copied()
                    .find(|i| !self.origins[*i].suspects.is_empty())
            }) {
                if self.settings.reverse || !self.hidden.contains(&q.commit) {
                    self.pass_blame(o)?;
                }
                let rest = std::mem::take(&mut self.origins[o].suspects);
                if !rest.is_empty() {
                    self.origins[o].guilty = true;
                    for &(s, l) in &rest {
                        self.lines[l] = Some((o, s));
                    }
                    self.settled.push((o, rest));
                }
            }
        }
        Ok(())
    }

    fn queue_blames(&mut self, po: usize, mut add: Vec<(usize, usize)>) -> Result<()> {
        let commit = self.origins[po].commit;
        let was_empty = self.origins[po].suspects.is_empty();
        let sibling_busy = self.by_commit[&commit]
            .iter()
            .any(|i| !self.origins[*i].suspects.is_empty());
        if was_empty {
            self.origins[po].suspects = add;
        } else {
            self.origins[po].suspects.append(&mut add);
            self.origins[po].suspects.sort_unstable();
        }
        if was_empty && !sibling_busy {
            let time = self.time_of(&commit)?;
            self.push(commit, time);
        }
        Ok(())
    }

    /// The parent's origin of the same path, if the parent has it as a file of the same kind.
    fn find_origin(&mut self, parent: &ObjectId, o: usize) -> Result<Option<usize>> {
        let (path, mode) = (self.origins[o].path.clone(), self.origins[o].mode);
        if let Some(i) = self.index.get(&(*parent, path.clone())) {
            return Ok(Some(*i));
        }
        let tree = self.commit(parent)?.tree;
        let Some(e) = self.git.repo.entry_at(&tree, &path)? else {
            return Ok(None);
        };
        if e.mode.is_tree() || e.mode == FileMode::Gitlink || is_link(e.mode) != is_link(mode) {
            return Ok(None);
        }
        Ok(Some(self.origin(*parent, &path, e.id, e.mode)))
    }

    /// The parent's origin of the file `o`'s path was renamed from.
    fn find_rename(&mut self, parent: &ObjectId, o: usize) -> Result<Option<usize>> {
        let (path, commit, blob, mode) = {
            let x = &self.origins[o];
            (x.path.clone(), x.commit, x.blob, x.mode)
        };
        let ptree = self.commit(parent)?.tree;
        if let Some(e) = self.git.repo.entry_at(&ptree, &path)?
            && !e.mode.is_tree()
        {
            return Ok(None);
        }
        let sources: Vec<(Vec<u8>, Entry)> = if let Some((f, fake)) = &self.fake
            && *f == commit
        {
            let old = self.git.repo.flatten(Some(&ptree), &Pathspec::default())?;
            old.into_iter()
                .filter(|(p, _)| !fake.tree.contains_key(p))
                .collect()
        } else {
            let tree = self.commit(&commit)?.tree;
            self.git
                .repo
                .diff_trees(Some(&ptree), Some(&tree), &Pathspec::default())?
                .into_iter()
                .filter_map(|c| match (c.old, c.new) {
                    (Some(old), None) => Some((c.path, old)),
                    _ => None,
                })
                .collect()
        };
        if sources.is_empty() {
            return Ok(None);
        }
        let data = self.data(o)?;
        let dst = Entry { mode, id: blob };
        let Some(at) = find_source(self.git, &sources, &path, dst, &data)? else {
            return Ok(None);
        };
        let (src_path, src) = &sources[at];
        Ok(Some(self.origin(*parent, src_path, src.id, src.mode)))
    }

    fn pass_whole(&mut self, o: usize, po: usize) -> Result<()> {
        let suspects = std::mem::take(&mut self.origins[o].suspects);
        self.queue_blames(po, suspects)
    }

    fn pass_blame(&mut self, o: usize) -> Result<()> {
        let commit = self.origins[o].commit;
        let parents = self.parents_of(&commit)?;
        let mut found: Vec<Option<usize>> = vec![None; parents.len()];
        for pass in 0..2 {
            for (i, p) in parents.iter().enumerate() {
                if found[i].is_some() {
                    continue;
                }
                let po = if pass == 0 {
                    self.find_origin(p, o)?
                } else {
                    self.find_rename(p, o)?
                };
                let Some(po) = po else { continue };
                if self.origins[po].blob == self.origins[o].blob {
                    return self.pass_whole(o, po);
                }
                let same = found[..i]
                    .iter()
                    .flatten()
                    .any(|j| self.origins[*j].blob == self.origins[po].blob);
                if !same {
                    found[i] = Some(po);
                }
            }
        }
        for po in found.iter().flatten().copied() {
            if self.origins[o].previous.is_none() {
                self.origins[o].previous = Some(po);
            }
            self.pass_to_parent(o, po, false)?;
            if self.origins[o].suspects.is_empty() {
                return Ok(());
            }
        }
        if self.ignore.contains(&commit) {
            for po in found.into_iter().flatten() {
                self.pass_to_parent(o, po, true)?;
                self.prints.remove(&po);
                if self.origins[o].suspects.is_empty() {
                    break;
                }
            }
            self.prints.remove(&o);
        }
        Ok(())
    }

    /// With `ignore`, the lines of a changed hunk go to the parent line they most resemble, else stay as unblamable.
    fn pass_to_parent(&mut self, o: usize, po: usize, ignore: bool) -> Result<()> {
        if self.origins[o].suspects.is_empty() {
            return Ok(());
        }
        let (a, b) = (self.data(po)?, self.data(o)?);
        let mut ws = if self.settings.ignore_whitespace {
            pyn_diff::Ws::all()
        } else {
            pyn_diff::Ws::default()
        };
        ws.minimal = self.settings.minimal;
        let diff = hunks_ws(&a, &b, &ws, self.settings.indent_heuristic);
        let mut guesses: Vec<Vec<Option<usize>>> = Vec::new();
        if ignore {
            let mut parent = match self.prints.remove(&po) {
                Some(p) => p,
                None => fingerprints(&pyn_diff::split_lines(&a)),
            };
            let target = match self.prints.remove(&o) {
                Some(t) => t,
                None => fingerprints(&pyn_diff::split_lines(&b)),
            };
            for h in &diff {
                guesses.push(if h.b_count > 0 {
                    guess_line_blames(
                        &mut parent,
                        &target,
                        h.b_start,
                        h.a_start,
                        h.b_start + h.b_count,
                        h.a_count,
                    )
                } else {
                    Vec::new()
                });
            }
            self.prints.insert(po, parent);
            self.prints.insert(o, target);
        }
        let suspects = std::mem::take(&mut self.origins[o].suspects);
        let (mut keep, mut moved) = (Vec::new(), Vec::new());
        let mut after_hunks: HashMap<usize, usize> = HashMap::new();
        let (mut next, mut offset) = (0, 0i64);
        for (s, l) in suspects {
            while next < diff.len() && s >= diff[next].b_start + diff[next].b_count {
                let h = &diff[next];
                offset = (h.a_start + h.a_count) as i64 - (h.b_start + h.b_count) as i64;
                next += 1;
            }
            if next < diff.len() && s >= diff[next].b_start {
                match guesses.get(next).filter(|_| ignore) {
                    Some(g) => match g[s - diff[next].b_start] {
                        Some(ps) => {
                            self.ignored[l] = true;
                            after_hunks.insert(l, next);
                            moved.push((ps, l));
                        }
                        None => {
                            self.unblamable[l] = true;
                            keep.push((s, l));
                        }
                    },
                    None => keep.push((s, l)),
                }
            } else {
                after_hunks.insert(l, next);
                moved.push(((s as i64 + offset) as usize, l));
            }
        }
        if ignore {
            moved.sort_unstable();
        }
        // a piece is cut where a hunk lies between its lines
        let mut renamed: HashMap<(u64, usize), u64> = HashMap::new();
        let top = self.piece.iter().copied().max().unwrap_or(0);
        for &(_, l) in &moved {
            let next = top + 1 + renamed.len() as u64;
            self.piece[l] = *renamed
                .entry((self.piece[l], after_hunks[&l]))
                .or_insert(next);
        }
        self.origins[o].suspects = keep;
        self.queue_blames(po, moved)
    }
}

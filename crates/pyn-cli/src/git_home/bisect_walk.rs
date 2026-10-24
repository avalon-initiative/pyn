//! The revision walk behind `bisect`: git's `limit_list` over the bad commit and the good ones, with history
//! simplification for paths, and `find_bisection`'s choice of the commit that splits the candidates best.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use anyhow::Result;
use pyn_gitwork::{Pathspec, Repo};
use pyn_object::ObjectId;

/// How many extra uninteresting commits `limit_list` looks at before it stops.
const SLOP: u32 = 5;

#[derive(Default)]
struct Node {
    parsed: bool,
    parents: Vec<ObjectId>,
    tree: Option<ObjectId>,
    date: i64,
    uninteresting: bool,
    bottom: bool,
    seen: bool,
    added: bool,
    treesame: bool,
}

struct Queued {
    date: i64,
    order: Reverse<u64>,
    id: ObjectId,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.date, &self.order) == (other.date, &other.order)
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
        (self.date, &self.order).cmp(&(other.date, &other.order))
    }
}

pub(super) struct Walk<'a> {
    repo: &'a Repo,
    nodes: HashMap<ObjectId, Node>,
    spec: Pathspec,
    first_parent: bool,
    queue: BinaryHeap<Queued>,
    counter: u64,
}

/// What `find_bisection` settled on.
pub(super) struct Found {
    /// The best commit first; with `all`, every candidate best first.
    pub list: Vec<ObjectId>,
    /// How many candidates the best one reaches, itself included.
    pub reaches: i32,
    /// How many candidates there are.
    pub all: i32,
}

impl<'a> Walk<'a> {
    pub fn new(repo: &'a Repo) -> Self {
        Self {
            repo,
            nodes: HashMap::new(),
            spec: Pathspec::default(),
            first_parent: false,
            queue: BinaryHeap::new(),
            counter: 0,
        }
    }

    pub fn configure(&mut self, spec: Pathspec, first_parent: bool) {
        self.spec = spec;
        self.first_parent = first_parent;
    }

    /// Drops every mark a walk left on a commit; what was read of the commits stays, as in git's process.
    pub fn clear_marks(&mut self) {
        for n in self.nodes.values_mut() {
            n.uninteresting = false;
            n.bottom = false;
            n.seen = false;
            n.added = false;
            n.treesame = false;
        }
    }

    /// `reset_revision_walk`: forgets which commits a walk has seen, not which it found uninteresting.
    pub fn reset_walk(&mut self) {
        for n in self.nodes.values_mut() {
            n.seen = false;
            n.added = false;
        }
    }

    fn node(&mut self, id: &ObjectId) -> &mut Node {
        self.nodes.entry(*id).or_default()
    }

    fn parse(&mut self, id: &ObjectId) -> Result<()> {
        if self.nodes.get(id).is_some_and(|n| n.parsed) {
            return Ok(());
        }
        let c = self.repo.commit(id)?;
        let n = self.node(id);
        n.parsed = true;
        n.parents = c.parents;
        n.tree = Some(c.tree);
        n.date = c.committer.time;
        Ok(())
    }

    fn flag(&self, id: &ObjectId, f: impl Fn(&Node) -> bool) -> bool {
        self.nodes.get(id).is_some_and(f)
    }

    fn uninteresting(&self, id: &ObjectId) -> bool {
        self.flag(id, |n| n.uninteresting)
    }

    fn relevant(&self, id: &ObjectId) -> bool {
        self.nodes
            .get(id)
            .is_none_or(|n| !n.uninteresting || n.bottom)
    }

    fn parents(&self, id: &ObjectId) -> Vec<ObjectId> {
        self.nodes
            .get(id)
            .map(|n| n.parents.clone())
            .unwrap_or_default()
    }

    fn put(&mut self, id: ObjectId) {
        self.counter += 1;
        let date = self.node(&id).date;
        self.queue.push(Queued {
            date,
            order: Reverse(self.counter),
            id,
        });
    }

    fn mark_one(&mut self, id: &ObjectId, pending: &mut Vec<ObjectId>) {
        let n = self.node(id);
        if n.uninteresting {
            return;
        }
        n.uninteresting = true;
        pending.extend(n.parents.iter().copied());
    }

    fn mark_parents_uninteresting(&mut self, id: &ObjectId) {
        let mut pending = Vec::new();
        for p in self.parents(id) {
            self.mark_one(&p, &mut pending);
        }
        while let Some(c) = pending.pop() {
            self.mark_one(&c, &mut pending);
        }
    }

    fn same_tree(&mut self, parent: Option<&ObjectId>, commit: &ObjectId) -> Result<bool> {
        let new = self.node(commit).tree;
        let old = match parent {
            Some(p) => self.node(p).tree,
            None => None,
        };
        if old.is_some() && old == new {
            return Ok(true);
        }
        Ok(self
            .repo
            .diff_trees(old.as_ref(), new.as_ref(), &self.spec)?
            .is_empty())
    }

    fn try_to_simplify(&mut self, id: &ObjectId) -> Result<()> {
        if self.spec.is_empty() {
            return Ok(());
        }
        let parents = self.parents(id);
        if parents.is_empty() {
            if self.same_tree(None, id)? {
                self.node(id).treesame = true;
            }
            return Ok(());
        }
        let (mut relevant_change, mut irrelevant_change, mut relevant_parents) = (false, false, 0);
        for (nth, p) in parents.iter().enumerate() {
            if self.relevant(p) {
                relevant_parents += 1;
            }
            if nth == 1 && self.first_parent {
                break;
            }
            self.parse(p)?;
            if self.same_tree(Some(p), id)? {
                if !self.relevant(p) {
                    continue;
                }
                let n = self.node(id);
                n.parents = vec![*p];
                n.treesame = true;
                return Ok(());
            }
            if self.relevant(p) {
                relevant_change = true;
            } else {
                irrelevant_change = true;
            }
        }
        let treesame = if relevant_parents > 0 {
            !relevant_change
        } else {
            !irrelevant_change
        };
        if treesame {
            self.node(id).treesame = true;
        }
        Ok(())
    }

    fn process_parents(&mut self, id: &ObjectId) -> Result<()> {
        let n = self.node(id);
        if n.added {
            return Ok(());
        }
        n.added = true;
        if n.uninteresting {
            for p in self.parents(id) {
                self.node(&p).uninteresting = true;
                if self.parse(&p).is_err() {
                    continue;
                }
                if !self.parents(&p).is_empty() {
                    self.mark_parents_uninteresting(&p);
                }
                if self.node(&p).seen {
                    continue;
                }
                self.node(&p).seen = true;
                self.put(p);
            }
            return Ok(());
        }
        self.try_to_simplify(id)?;
        for p in self.parents(id) {
            self.parse(&p)?;
            if !self.node(&p).seen {
                self.node(&p).seen = true;
                self.put(p);
            }
            if self.first_parent {
                break;
            }
        }
        Ok(())
    }

    fn still_interesting(&self, date: i64, slop: u32, cache: &mut Option<ObjectId>) -> u32 {
        let Some(head) = self.queue.peek() else {
            return 0;
        };
        if date <= head.date {
            return SLOP;
        }
        if let Some(c) = cache
            && !self.uninteresting(c)
        {
            return SLOP;
        }
        if let Some(q) = self.queue.iter().find(|q| !self.uninteresting(&q.id)) {
            *cache = Some(q.id);
            return SLOP;
        }
        slop - 1
    }

    /// The commits `git rev-list` walks for `args` (an id, and whether it is excluded, as `^id`) before it
    /// filters them, in the order it walks them.
    pub fn limit(&mut self, args: &[(ObjectId, bool)]) -> Result<Vec<ObjectId>> {
        self.queue.clear();
        for (id, excluded) in args {
            let n = self.node(id);
            if *excluded {
                n.uninteresting = true;
                n.bottom = true;
            }
        }
        let mut start = Vec::new();
        for (id, _) in args {
            self.parse(id)?;
            if self.uninteresting(id) {
                self.mark_parents_uninteresting(id);
            }
            let n = self.node(id);
            if !n.seen {
                n.seen = true;
                start.push(*id);
            }
        }
        start.sort_by_key(|id| Reverse(self.nodes[id].date));
        for id in start {
            self.put(id);
        }
        let mut slop = SLOP;
        let mut date = i64::MAX;
        let mut cache: Option<ObjectId> = None;
        let mut list = Vec::new();
        while let Some(Queued { id, .. }) = self.queue.pop() {
            if cache == Some(id) {
                cache = None;
            }
            self.process_parents(&id)?;
            if self.uninteresting(&id) {
                self.mark_parents_uninteresting(&id);
                slop = self.still_interesting(date, slop, &mut cache);
                if slop != 0 {
                    continue;
                }
                break;
            }
            date = self.node(&id).date;
            list.push(id);
        }
        Ok(list)
    }

    /// `limit`'s commits that survived as candidates, in walk order.
    pub fn interesting(&self, popped: &[ObjectId]) -> Vec<ObjectId> {
        popped
            .iter()
            .filter(|id| !self.uninteresting(id))
            .copied()
            .collect()
    }

    fn count_distance(&self, start: &ObjectId) -> i32 {
        let mut seen = HashSet::new();
        let mut todo = vec![*start];
        let mut nr = 0;
        while let Some(id) = todo.pop() {
            if self.uninteresting(&id) || !seen.insert(id) {
                continue;
            }
            if !self.flag(&id, |n| n.treesame) {
                nr += 1;
            }
            todo.extend(self.parents(&id));
        }
        nr
    }

    fn interesting_parents(&self, id: &ObjectId) -> usize {
        let parents = self.parents(id);
        let limit = if self.first_parent { 1 } else { parents.len() };
        parents
            .iter()
            .take(limit)
            .filter(|p| !self.uninteresting(p))
            .count()
    }

    /// `find_bisection`: `popped` is `limit`'s result; `all` asks for every candidate, best first.
    pub fn find_bisection(&self, popped: &[ObjectId], all: bool) -> Found {
        let list: Vec<ObjectId> = self.interesting(popped).into_iter().rev().collect();
        let treesame = |id: &ObjectId| self.flag(id, |n| n.treesame);
        let nr = list.iter().filter(|id| !treesame(id)).count() as i32;
        let found = |list: Vec<ObjectId>, reaches: i32| Found {
            list,
            reaches,
            all: nr,
        };
        if list.is_empty() {
            return found(Vec::new(), 0);
        }
        let index: HashMap<ObjectId, usize> =
            list.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let mut weights = vec![0i32; list.len()];
        let mut counted = 0;
        for (i, id) in list.iter().enumerate() {
            match self.interesting_parents(id) {
                0 => {
                    if !treesame(id) {
                        weights[i] = 1;
                        counted += 1;
                    }
                }
                1 => weights[i] = -1,
                _ => weights[i] = -2,
            }
        }
        let halfway = |w: i32, id: &ObjectId| {
            if treesame(id) {
                return false;
            }
            let diff = 2 * w - nr;
            matches!(diff, -1..=1) || diff.abs() < nr / 1024
        };
        for (i, id) in list.iter().enumerate() {
            if weights[i] != -2 {
                continue;
            }
            weights[i] = self.count_distance(id);
            if !all && halfway(weights[i], id) {
                return found(vec![*id], weights[i]);
            }
            counted += 1;
        }
        while counted < nr {
            let mut progress = false;
            for (i, id) in list.iter().enumerate() {
                if weights[i] >= 0 {
                    continue;
                }
                let parents = self.parents(id);
                let limit = if self.first_parent { 1 } else { parents.len() };
                let known = parents.iter().take(limit).find_map(|q| {
                    if self.uninteresting(q) {
                        return None;
                    }
                    index.get(q).map(|j| weights[*j]).filter(|w| *w >= 0)
                });
                let Some(w) = known else { continue };
                progress = true;
                if treesame(id) {
                    weights[i] = w;
                } else {
                    weights[i] = w + 1;
                    counted += 1;
                }
                if !all && halfway(weights[i], id) {
                    return found(vec![*id], weights[i]);
                }
            }
            if !progress {
                break;
            }
        }
        let distance = |i: usize| weights[i].min(nr - weights[i]);
        if !all {
            let mut best = 0;
            let mut best_distance = -1;
            for (i, id) in list.iter().enumerate() {
                if treesame(id) {
                    continue;
                }
                if distance(i) > best_distance {
                    best = i;
                    best_distance = distance(i);
                }
            }
            return found(vec![list[best]], weights[best]);
        }
        let mut ranked: Vec<usize> = (0..list.len()).filter(|i| !treesame(&list[*i])).collect();
        ranked.sort_by(|a, b| {
            distance(*b)
                .cmp(&distance(*a))
                .then_with(|| list[*a].to_hex().cmp(&list[*b].to_hex()))
        });
        match ranked.first() {
            Some(first) => {
                let reaches = weights[*first];
                found(ranked.iter().map(|i| list[*i]).collect(), reaches)
            }
            None => found(vec![list[0]], weights[0]),
        }
    }
}

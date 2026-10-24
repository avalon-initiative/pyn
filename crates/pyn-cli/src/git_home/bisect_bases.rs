//! `git merge-base --all <one> <two>...` in git's order: the best common ancestors, newest first, equal times
//! in the order the painting walk finds them.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use anyhow::Result;
use pyn_gitwork::Repo;
use pyn_object::ObjectId;

const PARENT1: u8 = 1;
const PARENT2: u8 = 2;
const STALE: u8 = 4;
const RESULT: u8 = 8;
const ENQUEUED: u8 = 16;

struct Painter<'a> {
    repo: &'a Repo,
    commits: HashMap<ObjectId, (i64, Vec<ObjectId>)>,
}

#[derive(Default)]
struct Queue {
    heap: BinaryHeap<(i64, Reverse<usize>)>,
    ids: Vec<ObjectId>,
    /// The last non-stale commit in queue order; taking it ends the walk.
    max_nonstale: Option<(ObjectId, i64)>,
}

impl Queue {
    fn put(&mut self, flags: &mut HashMap<ObjectId, u8>, id: &ObjectId, date: i64) {
        let f = flags.entry(*id).or_default();
        if *f & ENQUEUED != 0 {
            return;
        }
        *f |= ENQUEUED;
        self.ids.push(*id);
        self.heap.push((date, Reverse(self.ids.len() - 1)));
        if *f & STALE == 0 && self.max_nonstale.is_none_or(|(_, old)| old >= date) {
            self.max_nonstale = Some((*id, date));
        }
    }
}

struct Painted {
    flags: HashMap<ObjectId, u8>,
    found: Vec<ObjectId>,
}

impl Painter<'_> {
    fn read(&mut self, id: &ObjectId) -> Result<(i64, Vec<ObjectId>)> {
        if let Some(c) = self.commits.get(id) {
            return Ok(c.clone());
        }
        let c = self.repo.commit(id)?;
        let entry = (c.committer.time, c.parents);
        self.commits.insert(*id, entry.clone());
        Ok(entry)
    }

    /// `paint_down_to_common`: mark what `one` and `twos` reach and collect the commits both reach.
    fn paint(&mut self, one: &ObjectId, twos: &[ObjectId]) -> Result<Painted> {
        let mut flags: HashMap<ObjectId, u8> = HashMap::new();
        let mut queue = Queue::default();
        let mut found = Vec::new();
        *flags.entry(*one).or_default() |= PARENT1;
        queue.put(&mut flags, one, self.read(one)?.0);
        for t in twos {
            *flags.entry(*t).or_default() |= PARENT2;
            queue.put(&mut flags, t, self.read(t)?.0);
        }
        while queue.max_nonstale.is_some() {
            let Some((_, Reverse(at))) = queue.heap.pop() else {
                break;
            };
            let id = queue.ids[at];
            *flags.entry(id).or_default() &= !ENQUEUED;
            if queue.max_nonstale.is_some_and(|(m, _)| m == id) {
                queue.max_nonstale = None;
            }
            let mut f = flags[&id] & (PARENT1 | PARENT2 | STALE);
            if f == PARENT1 | PARENT2 {
                let own = flags.entry(id).or_default();
                if *own & RESULT == 0 {
                    *own |= RESULT;
                    found.push(id);
                }
                f |= STALE;
            }
            for p in self.read(&id)?.1 {
                let pf = flags.entry(p).or_default();
                if *pf & f == f {
                    continue;
                }
                *pf |= f;
                queue.put(&mut flags, &p, self.read(&p)?.0);
            }
        }
        Ok(Painted { flags, found })
    }

    fn sorted(&mut self, ids: Vec<ObjectId>) -> Result<Vec<ObjectId>> {
        let mut dated = Vec::new();
        for id in ids {
            dated.push((self.read(&id)?.0, id));
        }
        dated.sort_by_key(|(t, _)| Reverse(*t));
        Ok(dated.into_iter().map(|(_, id)| id).collect())
    }

    /// `remove_redundant`: the commits that are not an ancestor of another one, in their order.
    fn independent(&mut self, all: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let mut redundant = vec![false; all.len()];
        for i in 0..all.len() {
            if redundant[i] {
                continue;
            }
            let others: Vec<usize> = (0..all.len())
                .filter(|j| *j != i && !redundant[*j])
                .collect();
            let work: Vec<ObjectId> = others.iter().map(|j| all[*j]).collect();
            let painted = self.paint(&all[i], &work)?;
            if painted.flags.get(&all[i]).is_some_and(|f| f & PARENT2 != 0) {
                redundant[i] = true;
            }
            for (k, j) in others.iter().enumerate() {
                if painted
                    .flags
                    .get(&work[k])
                    .is_some_and(|f| f & PARENT1 != 0)
                {
                    redundant[*j] = true;
                }
            }
        }
        Ok(all
            .iter()
            .zip(&redundant)
            .filter(|(_, r)| !**r)
            .map(|(id, _)| *id)
            .collect())
    }
}

/// The best common ancestors of `one` and the `twos` taken together, as `git merge-base --all` lists them.
pub(super) fn merge_bases_many(
    repo: &Repo,
    one: &ObjectId,
    twos: &[ObjectId],
) -> Result<Vec<ObjectId>> {
    if twos.contains(one) {
        return Ok(vec![*one]);
    }
    let mut p = Painter {
        repo,
        commits: HashMap::new(),
    };
    let painted = p.paint(one, twos)?;
    let found: Vec<ObjectId> = painted
        .found
        .into_iter()
        .filter(|id| painted.flags[id] & STALE == 0)
        .collect();
    let found = p.sorted(found)?;
    if found.len() < 2 {
        return Ok(found);
    }
    let kept = p.independent(&found)?;
    p.sorted(kept)
}

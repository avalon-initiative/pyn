//! `blame -M` and `-C`: after the diff against a parent, the lines it still holds are searched for runs that
//! the parent's file (`-M`) or another file of the parent (`-C`) has, as git's find_move_in_parent and
//! find_copy_in_parent do. Lines travel as entries: runs contiguous in both files.

use std::collections::{HashMap, VecDeque};

use anyhow::Result;
use pyn_gitwork::{Entry as TreeEntry, Pathspec};
use pyn_object::{FileMode, ObjectId};

use super::{Board, Rc};

struct Ent {
    s: usize,
    l: usize,
    n: usize,
    piece: u64,
}

/// The best run an entry shares with a file: `n` lines from entry line `at` and file line `from`.
#[derive(Clone, Copy)]
struct Pick {
    at: usize,
    from: usize,
    n: usize,
    score: usize,
}

type Lines = Vec<(usize, usize)>;

impl Board<'_> {
    fn score(&self, l: usize, n: usize) -> usize {
        let text = &self.final_text[self.final_off[l]..self.final_off[l + n]];
        1 + text.iter().filter(|b| b.is_ascii_alphanumeric()).count()
    }

    /// Entries from per-line suspects; runs of different sources interleave, so each is followed by offset and piece.
    fn entries(&self, suspects: &[(usize, usize)]) -> Vec<Ent> {
        let mut out: Vec<Ent> = Vec::new();
        let mut open: HashMap<(isize, u64), usize> = HashMap::new();
        for &(s, l) in suspects {
            let key = (l as isize - s as isize, self.piece[l]);
            match open.get(&key) {
                Some(&i) if out[i].s + out[i].n == s => out[i].n += 1,
                _ => {
                    open.insert(key, out.len());
                    out.push(Ent {
                        s,
                        l,
                        n: 1,
                        piece: key.1,
                    });
                }
            }
        }
        out
    }

    /// The lines with each entry kept whole, entries in the order of their first source line.
    pub(super) fn in_entry_order(&self, lines: &[(usize, usize)]) -> Lines {
        let mut ents = self.entries(lines);
        ents.sort_by_key(|e| e.s);
        ents.iter()
            .flat_map(|e| (0..e.n).map(move |i| (e.s + i, e.l + i)))
            .collect()
    }

    fn filter_small(&self, small: &mut Vec<Ent>, ents: &mut Vec<Ent>, score: usize) {
        let (low, high): (Vec<Ent>, Vec<Ent>) = std::mem::take(ents)
            .into_iter()
            .partition(|e| self.score(e.l, e.n) <= score);
        small.extend(low);
        *ents = high;
    }

    /// The longest-scoring run of the entry's lines found unchanged in `file`; a later run wins a tie.
    fn best_split(&self, e: &Ent, file: &[u8]) -> Option<Pick> {
        let text = &self.final_text[self.final_off[e.l]..self.final_off[e.l + e.n]];
        let mut ws = if self.settings.ignore_whitespace {
            pyn_diff::Ws::all()
        } else {
            pyn_diff::Ws::default()
        };
        ws.minimal = self.settings.minimal;
        let hunks = crate::xdiff::hunks_ws(file, text, &ws, self.settings.indent_heuristic);
        let mut best: Option<Pick> = None;
        let mut consider = |at: usize, from: usize, end: usize| {
            if at < end {
                let score = self.score(e.l + at, end - at);
                if best.is_none_or(|b| score >= b.score) {
                    best = Some(Pick {
                        at,
                        from,
                        n: end - at,
                        score,
                    });
                }
            }
        };
        let (mut at, mut from) = (0, 0);
        for h in &hunks {
            consider(at, from, h.b_start);
            at = h.b_start + h.b_count;
            from = h.a_start + h.a_count;
        }
        consider(at, from, e.n);
        best
    }

    /// Cuts the picked run out of the entry: the run's lines and the entries left before and after it.
    fn cut(e: &Ent, pick: Pick) -> (Lines, Vec<Ent>) {
        let run = (0..pick.n)
            .map(|i| (pick.from + i, e.l + pick.at + i))
            .collect();
        let mut rest = Vec::new();
        if pick.at > 0 {
            rest.push(Ent {
                n: pick.at,
                s: e.s,
                l: e.l,
                piece: e.piece,
            });
        }
        let end = pick.at + pick.n;
        if end < e.n {
            rest.push(Ent {
                s: e.s + end,
                l: e.l + end,
                n: e.n - end,
                piece: e.piece,
            });
        }
        (run, rest)
    }

    fn fresh_piece(&mut self) -> u64 {
        self.piece.iter().copied().max().unwrap_or(0) + 1
    }

    fn blame_run(&mut self, into: &mut Vec<(usize, Lines)>, origin: usize, run: Lines) {
        let piece = self.fresh_piece();
        for &(_, l) in &run {
            self.piece[l] = piece;
        }
        match into.iter_mut().find(|(o, _)| *o == origin) {
            Some((_, lines)) => lines.extend(run),
            None => into.push((origin, run)),
        }
    }

    pub(super) fn find_moves_copies(
        &mut self,
        o: usize,
        parents: &[ObjectId],
        found: &[Option<usize>],
    ) -> Result<()> {
        let (mv, cp) = (self.settings.move_score, self.settings.copy_score);
        let suspects = std::mem::take(&mut self.origins[o].suspects);
        let mut ents = self.entries(&suspects);
        let mut small = Vec::new();
        let mut blamed: Vec<(usize, Lines)> = Vec::new();
        self.filter_small(&mut small, &mut ents, mv);
        for po in found.iter().flatten().copied() {
            if ents.is_empty() {
                break;
            }
            self.find_move(po, &mut ents, &mut blamed)?;
        }
        if self.settings.copy_level > 0 {
            if cp < mv {
                ents.append(&mut small);
            }
            if cp != mv {
                self.filter_small(&mut small, &mut ents, cp);
            }
            for (i, parent) in parents.iter().enumerate() {
                if ents.is_empty() {
                    break;
                }
                self.find_copy(o, parent, found[i], &mut ents, &mut blamed)?;
            }
        }
        small.sort_by_key(|e| e.s);
        self.origins[o].suspects = small
            .iter()
            .chain(&ents)
            .flat_map(|e| (0..e.n).map(move |i| (e.s + i, e.l + i)))
            .collect();
        for (po, mut lines) in blamed {
            lines.sort_by_key(|(s, _)| *s);
            self.queue_blames(po, lines)?;
        }
        Ok(())
    }

    fn find_move(
        &mut self,
        po: usize,
        ents: &mut Vec<Ent>,
        blamed: &mut Vec<(usize, Lines)>,
    ) -> Result<()> {
        let file = self.data(po)?;
        let mut work: VecDeque<Ent> = std::mem::take(ents).into();
        while let Some(e) = work.pop_front() {
            match self.best_split(&e, &file) {
                Some(pick) if pick.score > self.settings.move_score => {
                    let (run, rest) = Self::cut(&e, pick);
                    self.blame_run(blamed, po, run);
                    rest.into_iter().rev().for_each(|r| work.push_front(r));
                }
                _ => ents.push(e),
            }
        }
        ents.sort_by_key(|e| e.s);
        Ok(())
    }

    fn find_copy(
        &mut self,
        o: usize,
        parent: &ObjectId,
        porigin: Option<usize>,
        ents: &mut Vec<Ent>,
        blamed: &mut Vec<(usize, Lines)>,
    ) -> Result<()> {
        let level = self.settings.copy_level;
        let skip = porigin.map(|p| self.origins[p].path.clone());
        let harder = level >= 3 || (level == 2 && skip.as_ref() != Some(&self.origins[o].path));
        let sources: Vec<(Vec<u8>, TreeEntry)> = self
            .copy_sources(o, parent, harder)?
            .into_iter()
            .filter(|(p, e)| e.mode != FileMode::Gitlink && Some(p) != skip.as_ref())
            .collect();
        // git makes an origin per candidate file up front, and the newest origin settles first
        let origins: Vec<usize> = sources
            .iter()
            .map(|(path, src)| self.origin(*parent, path, src.id, src.mode))
            .collect();
        let mut work = std::mem::take(ents);
        while !work.is_empty() {
            let mut picks: Vec<Option<(Pick, usize)>> = vec![None; work.len()];
            for (i, (_, src)) in sources.iter().enumerate() {
                let file = self.blob_data(&src.id)?;
                for (e, best) in work.iter().zip(&mut picks) {
                    if let Some(p) = self.best_split(e, &file)
                        && best.is_none_or(|(b, _)| p.score >= b.score)
                    {
                        *best = Some((p, i));
                    }
                }
            }
            let mut next = Vec::new();
            for (e, best) in work.into_iter().zip(picks) {
                match best {
                    Some((pick, i)) if pick.score > self.settings.copy_score => {
                        let po = origins[i];
                        let (run, rest) = Self::cut(&e, pick);
                        self.blame_run(blamed, po, run);
                        next.extend(rest);
                    }
                    _ => ents.push(e),
                }
            }
            work = next;
        }
        ents.sort_by_key(|e| e.s);
        Ok(())
    }

    fn blob_data(&mut self, id: &ObjectId) -> Result<Rc<Vec<u8>>> {
        if let Some(d) = self.blobs.get(id) {
            return Ok(d.clone());
        }
        let d = Rc::new(self.git.repo.blob(id)?);
        self.blobs.insert(*id, d.clone());
        Ok(d)
    }

    /// The parent's files a copy may come from: those the target changed, or every file with `all`.
    fn copy_sources(
        &mut self,
        o: usize,
        parent: &ObjectId,
        all: bool,
    ) -> Result<Vec<(Vec<u8>, TreeEntry)>> {
        let ptree = self.commit(parent)?.tree;
        if all {
            let old = self.git.repo.flatten(Some(&ptree), &Pathspec::default())?;
            return Ok(old.into_iter().collect());
        }
        let commit = self.origins[o].commit;
        if let Some((f, fake)) = &self.fake
            && *f == commit
        {
            let old = self.git.repo.flatten(Some(&ptree), &Pathspec::default())?;
            return Ok(old
                .into_iter()
                .filter(|(p, e)| fake.tree.get(p) != Some(e))
                .collect());
        }
        let tree = self.commit(&commit)?.tree;
        let changes = self
            .git
            .repo
            .diff_trees(Some(&ptree), Some(&tree), &Pathspec::default())?;
        Ok(changes
            .into_iter()
            .filter_map(|c| c.old.map(|old| (c.path, old)))
            .collect())
    }
}

//! Renames in the three-way tree merge, as `ort` resolves them: an edit follows a rename on the other side, a rename
//! meets a delete or another rename, and a file added to a renamed folder moves with it. What is not matched exactly
//! is refused before anything is written.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use pyn_gitwork::Entry;
use pyn_object::FileMode;

use super::GitHome;
use super::strategy::{DirectoryRenames, Renames, Settings};
use super::treemerge::{
    Both, Conflict, Conflicting, Labels, Leaves, Resolved, Worktree, both_present, regular,
};
use crate::diff::{Blob, Source, Tree};
use crate::rename::{self, Mode};

pub(super) struct Plan {
    pub paths: BTreeMap<Vec<u8>, Resolved>,
    pub line_merged: BTreeSet<Vec<u8>>,
    pub updates: BTreeMap<Vec<u8>, Vec<String>>,
}

/// Source path to destination path for the renames from the base to one side.
type Pairs = BTreeMap<Vec<u8>, Vec<u8>>;

fn text(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn refuse<T>(what: &str) -> Result<T> {
    bail!("{what}; pyn cannot merge that yet, and nothing was changed")
}

fn blob(e: &Entry) -> Blob {
    Blob {
        hash: e.id.to_string(),
        mode: e.mode,
        source: Source::Object {
            id: e.id.to_string(),
        },
    }
}

/// Pairs the files `side` removed with those it added: identical content among all of them, then by similarity among
/// the sources `ort` finds relevant (touched by the other side, or in a folder `side` emptied when folders are followed).
fn detect(git: &GitHome, [base, side, other]: [&Leaves; 3], cfg: &Renames) -> Result<Pairs> {
    let mut live: BTreeSet<&[u8]> = BTreeSet::new();
    for path in side.keys() {
        let mut dir = &path[..];
        while let Some(at) = dir.iter().rposition(|c| *c == b'/') {
            dir = &dir[..at];
            if !live.insert(dir) {
                break;
            }
        }
    }
    let relevant = |path: &Vec<u8>, entry: &Entry| {
        other.get(path) != Some(entry)
            || (cfg.directories != DirectoryRenames::Off
                && path
                    .iter()
                    .enumerate()
                    .any(|(i, c)| *c == b'/' && !live.contains(&path[..i])))
    };
    let mut gone: Vec<(&Vec<u8>, &Entry, bool)> = base
        .iter()
        .filter(|(p, _)| !side.contains_key(*p))
        .map(|(p, e)| (p, e, relevant(p, e)))
        .collect();
    let mut came: Vec<(&Vec<u8>, &Entry)> = side
        .iter()
        .filter(|(p, _)| !base.contains_key(*p))
        .collect();
    let mut pairs = Pairs::new();
    let mut ambiguous = false;
    came.retain(|(dst, entry)| {
        let mut same = gone
            .iter()
            .filter(|g| g.1.id == entry.id && !pairs.contains_key(g.0));
        let first = same.next();
        ambiguous |= same.next().is_some();
        match first {
            Some(g) => {
                pairs.insert(g.0.clone(), (*dst).clone());
                false
            }
            None => true,
        }
    });
    if ambiguous {
        return refuse("several removed files have the content of an added one");
    }
    gone.retain(|(p, _, rel)| *rel && !pairs.contains_key(*p));
    if gone.is_empty() || came.is_empty() {
        return Ok(pairs);
    }
    let (mut old, mut new) = (Tree::new(), Tree::new());
    for (tree, list) in [
        (
            &mut old,
            gone.iter().map(|g| (g.0, g.1)).collect::<Vec<_>>(),
        ),
        (&mut new, came),
    ] {
        for (path, entry) in list {
            let Ok(name) = String::from_utf8(path.clone()) else {
                return refuse("a path that is not UTF-8 was added or removed");
            };
            tree.insert(name, blob(entry));
        }
    }
    let opts = rename::Options {
        detect: true,
        brk: None,
        mode: Mode::Renames,
        score: cfg.score,
        limit: cfg.limit,
        harder: false,
    };
    let found = rename::detect(git, &old, &new, &opts)?;
    pairs.extend(
        found
            .moved
            .into_iter()
            .filter(|(_, m)| !m.copy)
            .map(|(dst, m)| (m.src.into_bytes(), dst.into_bytes())),
    );
    Ok(pairs)
}

struct Cx<'a> {
    git: &'a GitHome,
    b: &'a Leaves,
    sides: [&'a Leaves; 2],
    names: [&'a str; 2],
    labels: &'a Labels<'a>,
    settings: &'a Settings,
    plan: Plan,
}

impl Cx<'_> {
    fn claim(&mut self, path: &[u8], resolved: Resolved) -> Result<()> {
        if self.plan.paths.insert(path.to_vec(), resolved).is_some() {
            return refuse(&format!(
                "'{}' is involved in more than one rename",
                text(path)
            ));
        }
        Ok(())
    }

    /// Three-way merge of a renamed file; `paths` are where each version lives, base first.
    fn merge(
        &mut self,
        (base, ours, theirs): (&Entry, &Entry, &Entry),
        at: &[u8],
        paths: [&[u8]; 3],
    ) -> Result<(Resolved, bool)> {
        if ![base, ours, theirs].iter().all(|e| regular(e.mode)) {
            return refuse(&format!(
                "'{}' is a renamed file that is not a plain file",
                text(at)
            ));
        }
        let apart = paths[1] != paths[2];
        let name = |side: &str, p: &[u8]| match apart {
            true => format!("{side}:{}", text(p)),
            false => side.to_string(),
        };
        let labels = Labels {
            ours: &name(self.labels.ours, paths[1]),
            theirs: &name(self.labels.theirs, paths[2]),
            base: &name(self.labels.base, paths[0]),
        };
        let place = Both {
            path: at,
            labels: &labels,
            settings: self.settings,
        };
        let mut merged = false;
        let resolved = if ours == theirs || theirs == base {
            Resolved::Take(Some(*ours))
        } else if ours == base {
            Resolved::Take(Some(*theirs))
        } else {
            both_present(self.git, Some(base), ours, theirs, &place, &mut merged)?
        };
        Ok((resolved, merged))
    }

    fn settled(&mut self, src: &[u8], dst: &[u8], r: (Resolved, bool)) -> Result<()> {
        if r.1 {
            self.plan.line_merged.insert(dst.to_vec());
        }
        self.claim(src, Resolved::Take(None))?;
        self.claim(dst, r.0)
    }

    fn renamed_both(&mut self, src: &[u8], (d1, d2): (&[u8], &[u8])) -> Result<()> {
        let base = self.b[src];
        let [o, t] = self.sides;
        if d1 == d2 {
            if o[d1] == t[d1] {
                return Ok(());
            }
            let r = self.merge((&base, &o[d1], &t[d1]), d1, [src, d1, d1])?;
            return self.settled(src, d1, r);
        }
        if o.contains_key(d2) || t.contains_key(d1) {
            return refuse(&format!(
                "'{}' is renamed to two places, one already taken",
                text(src)
            ));
        }
        let (merged, line) = self.merge((&base, &o[d1], &t[d2]), src, [src, d1, d2])?;
        let Resolved::Take(Some(entry)) = merged else {
            return refuse(&format!(
                "'{}' is renamed to two places and its edits conflict",
                text(src)
            ));
        };
        if line {
            self.plan.line_merged.insert(src.to_vec());
        }
        let (ours, theirs) = (self.names[0], self.names[1]);
        let note = format!(
            "CONFLICT (rename/rename): {} renamed to {} in {ours} and to {} in {theirs}.",
            text(src),
            text(d1),
            text(d2)
        );
        let gone = Conflict {
            base: Some(base),
            ours: None,
            theirs: None,
            worktree: Worktree::Keep,
            kind: Conflicting::Renamed,
            moved_from: None,
            notes: vec![note],
        };
        let place = |side: usize, e: Entry, held: bool| -> Result<Resolved> {
            let worktree = if held {
                Worktree::Keep
            } else {
                Worktree::Write {
                    mode: e.mode,
                    bytes: self.git.repo.blob(&e.id)?,
                }
            };
            Ok(Resolved::Conflict(Conflict {
                base: None,
                ours: (side == 0).then_some(e),
                theirs: (side == 1).then_some(e),
                worktree,
                kind: Conflicting::Renamed,
                moved_from: None,
                notes: Vec::new(),
            }))
        };
        let held = o[d1] == entry;
        let (first, second) = (place(0, entry, held)?, place(1, entry, false)?);
        self.claim(src, Resolved::Conflict(gone))?;
        self.claim(d1, first)?;
        self.claim(d2, second)
    }

    /// `side` renamed `src` to `dst`; the other side left it, edited it or deleted it.
    fn renamed_one(&mut self, side: usize, src: &[u8], dst: &[u8]) -> Result<()> {
        let (x, y) = (side, 1 - side);
        let base = self.b[src];
        let Some(theirs_here) = self.sides[y].get(src).copied() else {
            return self.rename_delete(side, src, dst);
        };
        if theirs_here == base {
            return Ok(());
        }
        if self.sides[y].contains_key(dst) {
            return refuse(&format!(
                "'{}' is added on one side and renamed to on the other",
                text(dst)
            ));
        }
        let moved = self.sides[x][dst];
        let (versions, paths) = if x == 0 {
            ((&base, &moved, &theirs_here), [src, dst, src])
        } else {
            ((&base, &theirs_here, &moved), [src, src, dst])
        };
        let r = self.merge(versions, dst, paths)?;
        self.settled(src, dst, r)
    }

    fn rename_delete(&mut self, side: usize, src: &[u8], dst: &[u8]) -> Result<()> {
        let (x, y) = (side, 1 - side);
        if self.sides[y].contains_key(dst) {
            return refuse(&format!(
                "'{}' is added on one side and renamed to on the other",
                text(dst)
            ));
        }
        let base = self.b[src];
        let moved = self.sides[x][dst];
        let note = format!(
            "CONFLICT (rename/delete): {} renamed to {} in {}, but deleted in {}.",
            text(src),
            text(dst),
            self.names[x],
            self.names[y]
        );
        let edited = moved.id != base.id;
        if !edited && moved.mode != base.mode {
            return refuse(&format!(
                "'{}' is renamed with a mode change and deleted on the other side",
                text(src)
            ));
        }
        let kept_here = x == 0;
        let conflict = Conflict {
            base: Some(base),
            ours: kept_here.then_some(moved),
            theirs: (!kept_here).then_some(moved),
            worktree: if kept_here {
                Worktree::Keep
            } else {
                Worktree::Write {
                    mode: moved.mode,
                    bytes: self.git.repo.blob(&moved.id)?,
                }
            },
            kind: if edited {
                Conflicting::ModifyDelete {
                    ours_deleted: !kept_here,
                }
            } else {
                Conflicting::Renamed
            },
            moved_from: None,
            notes: vec![note],
        };
        self.claim(src, Resolved::Take(None))?;
        self.claim(dst, Resolved::Conflict(conflict))
    }

    fn renames(&mut self, pairs: &[Pairs; 2]) -> Result<()> {
        let srcs: BTreeSet<&Vec<u8>> = pairs[0].keys().chain(pairs[1].keys()).collect();
        for src in srcs {
            match (pairs[0].get(src), pairs[1].get(src)) {
                (Some(d1), Some(d2)) => self.renamed_both(src, (d1, d2))?,
                (Some(d), None) => self.renamed_one(0, src, d)?,
                (None, Some(d)) => self.renamed_one(1, src, d)?,
                (None, None) => {}
            }
        }
        Ok(())
    }
}

/// The folders `pairs` moved wholesale, as (old folder, new folder), and those whose destination is a tie.
fn moved_folders(side: &Leaves, pairs: &Pairs) -> (Pairs, BTreeSet<Vec<u8>>) {
    let mut votes: BTreeMap<Vec<u8>, BTreeMap<Vec<u8>, usize>> = BTreeMap::new();
    for (src, dst) in pairs {
        let (mut old, mut new) = (folders(src), folders(dst));
        while old.last().is_some() && old.last() == new.last() {
            old.pop();
            new.pop();
        }
        let (old, new) = (old.join(&b'/'), new.join(&b'/'));
        let kept = |dir: &[u8]| {
            side.keys()
                .any(|k| k.starts_with(dir) && k[dir.len()] == b'/')
        };
        if old.is_empty() || old == new || kept(&old) {
            continue;
        }
        *votes.entry(old).or_default().entry(new).or_default() += 1;
    }
    let (mut moved, mut tied) = (BTreeMap::new(), BTreeSet::new());
    for (old, to) in votes {
        let top = to.values().max().copied().unwrap_or(0);
        let mut best = to.into_iter().filter(|(_, n)| *n == top);
        match (best.next(), best.next()) {
            (Some((new, _)), None) => {
                moved.insert(old, new);
            }
            _ => {
                tied.insert(old);
            }
        }
    }
    (moved, tied)
}

fn folders(path: &[u8]) -> Vec<&[u8]> {
    let mut parts: Vec<&[u8]> = path.split(|c| *c == b'/').collect();
    parts.pop();
    parts
}

impl Cx<'_> {
    /// Files `adder` added inside a folder `mover` renamed go to the folder's new place.
    fn follow_folders(
        &mut self,
        mover: usize,
        pairs: &[Pairs; 2],
        mode: DirectoryRenames,
    ) -> Result<()> {
        let adder = 1 - mover;
        let (moved, tied) = moved_folders(self.sides[mover], &pairs[mover]);
        if moved.is_empty() && tied.is_empty() {
            return Ok(());
        }
        let inside = |path: &[u8], dir: &Vec<u8>| {
            path.starts_with(dir) && path.get(dir.len()) == Some(&b'/')
        };
        if pairs[adder]
            .values()
            .any(|d| moved.keys().chain(&tied).any(|k| inside(d, k)))
        {
            return refuse("a rename lands in a folder the other side renamed");
        }
        let (b, side) = (self.b, self.sides[adder]);
        let added: Vec<(&Vec<u8>, &Entry)> = side
            .iter()
            .filter(|(p, _)| !b.contains_key(*p) && !self.plan.paths.contains_key(*p))
            .collect();
        for (path, entry) in added {
            let deepest = |keys: &mut dyn Iterator<Item = &Vec<u8>>| {
                keys.filter(|k| inside(path, k))
                    .max_by_key(|k| k.len())
                    .cloned()
            };
            let (hit, tie) = (deepest(&mut moved.keys()), deepest(&mut tied.iter()));
            let Some(old) = hit.clone() else {
                if tie.is_some() {
                    return refuse("a folder was split across several places by renames");
                }
                continue;
            };
            if tie.is_some_and(|t| t.len() >= old.len()) {
                return refuse("a folder was split across several places by renames");
            }
            let mut target = moved[&old].clone();
            target.extend_from_slice(&path[old.len()..]);
            let taken = |p: &[u8]| {
                [self.b, self.sides[0], self.sides[1]]
                    .iter()
                    .any(|l| l.contains_key(p))
                    || self.plan.paths.contains_key(p)
            };
            if taken(&target) {
                return refuse(&format!(
                    "'{}' would move onto an existing path",
                    text(&target)
                ));
            }
            let (added_in, renamed_in) = (self.names[adder], self.names[mover]);
            self.claim(path, Resolved::Take(None))?;
            let from = text(path);
            if mode == DirectoryRenames::On {
                self.plan.updates.insert(
                    target.clone(),
                    vec![format!(
                        "Path updated: {from} added in {added_in} inside a directory that was renamed in {renamed_in}; moving it to {}.",
                        text(&target)
                    )],
                );
                self.claim(&target, Resolved::Take(Some(*entry)))?;
                continue;
            }
            if self.settings.marker_extra > 0 {
                return refuse("a folder rename conflicts inside the merged common ancestors");
            }
            let note = format!(
                "CONFLICT (file location): {from} added in {added_in} inside a directory that was renamed in {renamed_in}, suggesting it should perhaps be moved to {}.",
                text(&target)
            );
            let conflict = Conflict {
                base: None,
                ours: (adder == 0).then_some(*entry),
                theirs: (adder == 1).then_some(*entry),
                worktree: Worktree::Write {
                    mode: entry.mode,
                    bytes: self.git.repo.blob(&entry.id)?,
                },
                kind: Conflicting::Renamed,
                moved_from: None,
                notes: vec![note],
            };
            self.claim(&target, Resolved::Conflict(conflict))?;
        }
        Ok(())
    }
}

/// Resolves what renames change in a merge of `o` and `t` from `b`; every other path is left to the plain merge.
pub(super) fn plan(
    git: &GitHome,
    [b, o, t]: [&Leaves; 3],
    labels: &Labels,
    settings: &Settings,
    cfg: &Renames,
) -> Result<Plan> {
    let mut cx = Cx {
        git,
        b,
        sides: [o, t],
        names: ["HEAD", labels.theirs],
        labels,
        settings,
        plan: Plan {
            paths: BTreeMap::new(),
            line_merged: BTreeSet::new(),
            updates: BTreeMap::new(),
        },
    };
    let pairs = [detect(git, [b, o, t], cfg)?, detect(git, [b, t, o], cfg)?];
    if pairs[0].is_empty() && pairs[1].is_empty() {
        return Ok(cx.plan);
    }
    let gitlink = |l: &Leaves, p: &Vec<u8>| l.get(p).is_some_and(|e| e.mode == FileMode::Gitlink);
    if pairs.iter().flat_map(|p| p.keys()).any(|s| gitlink(b, s)) {
        return refuse("a submodule was renamed");
    }
    cx.renames(&pairs)?;
    if cfg.directories != DirectoryRenames::Off {
        for mover in 0..2 {
            cx.follow_folders(mover, &pairs, cfg.directories)?;
        }
    }
    Ok(cx.plan)
}

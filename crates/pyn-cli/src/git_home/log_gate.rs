//! Commits `pyn log` keeps by their diff: `--follow` and the pickaxe options `-S` and `-G`. A merge has no diff, so
//! these drop it, except with `--first-parent`.

use anyhow::Result;
use pyn_gitwork::{Change, Pathspec};
use pyn_object::ObjectId;
use regex::Regex;

use super::GitHome;
use super::commands::add_changes;
use crate::diff::{self, Tree};

pub(super) enum Pickaxe {
    /// `-S`: the number of occurrences of the text differs between the two sides.
    Text { needle: Vec<u8>, fold: bool },
    /// `-G`: a changed line matches.
    Lines(Regex),
}

pub(super) struct Gate {
    pub follow: Option<String>,
    pub pick: Option<Pickaxe>,
    pub first_parent: bool,
    pub spec: Pathspec,
}

fn count(hay: &[u8], needle: &[u8], fold: bool) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let fold_case = |b: &[u8]| b.to_ascii_lowercase();
    let (hay, needle) = if fold {
        (fold_case(hay), fold_case(needle))
    } else {
        (hay.to_vec(), needle.to_vec())
    };
    let (mut at, mut n) = (0, 0);
    while let Some(i) = hay[at..].windows(needle.len()).position(|w| w == needle) {
        n += 1;
        at += i + needle.len();
    }
    n
}

impl Pickaxe {
    fn differs(&self, old: &[u8], new: &[u8]) -> bool {
        match self {
            Self::Text { needle, fold } => count(old, needle, *fold) != count(new, needle, *fold),
            Self::Lines(re) => {
                if pyn_diff::is_binary(old) || pyn_diff::is_binary(new) {
                    return false;
                }
                let hunks = pyn_diff::unified_hunks(old, new, 0, false).unwrap_or_default();
                hunks
                    .lines()
                    .filter(|l| l.starts_with(['+', '-']))
                    .any(|l| re.is_match(&l[1..]))
            }
        }
    }
}

impl Gate {
    /// The changes of a commit against its parent (the first with `--first-parent`), or `None` for a merge.
    fn changes(
        &self,
        git: &GitHome,
        id: &ObjectId,
        spec: &Pathspec,
    ) -> Result<Option<(Vec<Change>, bool)>> {
        let c = git.repo.commit(id)?;
        if c.parents.len() > 1 && !self.first_parent {
            return Ok(None);
        }
        let parent_tree = match c.parents.first() {
            Some(p) => Some(git.repo.commit(p)?.tree),
            None => None,
        };
        let changes = git
            .repo
            .diff_trees(parent_tree.as_ref(), Some(&c.tree), spec)?;
        Ok(Some((changes, parent_tree.is_some())))
    }

    fn follow(&mut self, git: &GitHome, id: &ObjectId) -> Result<bool> {
        let Some(path) = self.follow.clone() else {
            return Ok(true);
        };
        let spec = Pathspec::new([path.as_bytes().to_vec()]);
        let Some((changes, has_parent)) = self.changes(git, id, &spec)? else {
            return Ok(false);
        };
        if changes.is_empty() {
            return Ok(false);
        }
        if has_parent && changes.len() == 1 && changes[0].old.is_none() {
            let (all, _) = self
                .changes(git, id, &Pathspec::default())?
                .unwrap_or_default();
            let (mut old, mut new) = (Tree::new(), Tree::new());
            add_changes(git, &mut old, &mut new, &all)?;
            if let Some(from) = diff::renamed_from(git, &old, &new, &path)? {
                self.follow = Some(from);
            }
        }
        Ok(true)
    }

    /// The paths a pickaxe kept, or `None` when the commit has none.
    fn picked(&self, git: &GitHome, id: &ObjectId, pick: &Pickaxe) -> Result<Option<Vec<String>>> {
        let Some((changes, _)) = self.changes(git, id, &self.spec)? else {
            return Ok(None);
        };
        let (mut old, mut new) = (Tree::new(), Tree::new());
        add_changes(git, &mut old, &mut new, &changes)?;
        let moved = diff::detect_renames(git, &old, &new)?;
        let sources: Vec<&str> = moved.values().map(|(src, _)| *src).collect();
        let mut kept = Vec::new();
        for path in old
            .keys()
            .chain(new.keys())
            .collect::<std::collections::BTreeSet<_>>()
        {
            let (src, dst) = match moved.get(path.as_str()) {
                Some((src, _)) => (*src, path),
                None if sources.contains(&path.as_str()) => continue,
                None => (path.as_str(), path),
            };
            let side = |tree: &Tree, name: &str| -> Result<Vec<u8>> {
                tree.get(name)
                    .map_or(Ok(Vec::new()), |b| diff::blob_bytes(git, name, b))
            };
            if pick.differs(&side(&old, src)?, &side(&new, dst)?) {
                kept.push(dst.clone());
            }
        }
        Ok((!kept.is_empty()).then_some(kept))
    }

    /// The paths to print for a commit that stays, or `None` when it is dropped.
    pub fn keeps(&mut self, git: &GitHome, id: &ObjectId) -> Result<Option<Vec<String>>> {
        if self.follow.is_some() {
            return Ok(self.follow(git, id)?.then(Vec::new));
        }
        match &self.pick {
            Some(pick) => self.picked(git, id, pick),
            None => Ok(Some(Vec::new())),
        }
    }
}

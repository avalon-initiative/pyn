//! `pyn rebase --rebase-merges`: the instruction sheet for a history with merges and the commands `label`, `reset`,
//! `merge` and `update-ref`, with the refs under `refs/rewritten/` and `rebase-merge/update-refs` they keep.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use pyn_git::Expect;
use pyn_object::{Commit, ObjectId};

use super::GitHome;
use super::pick::read_text;
use super::todo::{Cmd, Commits, Flag, Item};

pub(super) const REWRITTEN: &str = "refs/rewritten/";
const REFS_TO_DELETE: &str = "refs-to-delete";
const UPDATE_REFS: &str = "update-refs";

/// The commits a rebase replays: those reachable from the head but not from the base.
pub(super) struct Range {
    pub commits: HashMap<ObjectId, Commit>,
    /// Oldest first, in git's `--topo-order --reverse`.
    pub order: Vec<ObjectId>,
    /// Commits whose change the upstream already has.
    pub same_patch: HashSet<ObjectId>,
}

pub(super) struct Mode {
    pub cousins: bool,
    /// `--root --onto`: the roots are rebuilt on the new base rather than on a new root.
    pub root_with_onto: bool,
}

#[derive(Default)]
struct LabelState {
    by_commit: HashMap<ObjectId, String>,
    taken: HashSet<String>,
}

impl LabelState {
    fn add(&mut self, id: &ObjectId, label: String) -> String {
        self.taken.insert(label.clone());
        self.by_commit.insert(*id, label.clone());
        label
    }

    /// The label of a commit: the one it has, a unique abbreviation for a commit that is not replayed, or the
    /// given text made safe for a ref name and unique.
    fn label_oid(&mut self, git: &GitHome, id: &ObjectId, label: Option<&str>) -> Result<String> {
        if let Some(have) = self.by_commit.get(id) {
            return Ok(have.clone());
        }
        let hex_len = id.to_hex().len();
        let Some(text) = label else {
            let mut name = abbreviation(git, id);
            if self.taken.contains(&name) {
                let full = id.to_hex();
                name = (name.len() + 1..hex_len)
                    .map(|n| full[..n].to_string())
                    .find(|n| !self.taken.contains(n))
                    .unwrap_or(full);
            }
            return Ok(self.add(id, name));
        };
        let mut name = String::new();
        for c in text.chars() {
            if !c.is_ascii() || c.is_ascii_alphanumeric() {
                name.push(c);
            } else if !name.is_empty() && !name.ends_with('-') {
                name.push('-');
            }
        }
        if name.is_empty() {
            name = format!("rev-{}", abbreviation(git, id));
        }
        let looks_like_id = name.len() == hex_len && name.bytes().all(|b| b.is_ascii_hexdigit());
        if looks_like_id || name == "#" || self.taken.contains(&name) {
            let base = name.clone();
            name = (2..)
                .map(|n| format!("{base}-{n}"))
                .find(|n| !self.taken.contains(n))
                .expect("an endless range");
        }
        Ok(self.add(id, name))
    }
}

/// The shortest prefix of at least seven digits that names only this object.
fn abbreviation(git: &GitHome, id: &ObjectId) -> String {
    let full = id.to_hex();
    (7..full.len())
        .map(|n| &full[..n])
        .find(|p| git.repo.resolve(p).is_ok_and(|found| found == *id))
        .unwrap_or(&full)
        .to_string()
}

fn comment(text: &str) -> Item {
    Item::with_arg(Cmd::Comment, text)
}

/// The local branch to name a merged branch after: the last of the branches at the commit in ref order.
fn branch_names(git: &GitHome) -> Result<HashMap<ObjectId, String>> {
    let mut names = HashMap::new();
    let mut refs = git.repo.refs().list("refs/heads/")?;
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, id) in refs {
        if let Some(short) = name.strip_prefix("refs/heads/") {
            names.insert(id, short.to_string());
        }
    }
    Ok(names)
}

/// A merge's own name for the branch it merged: the quoted name of `Merge branch 'x'`, the branch of a pull request,
/// else the whole oneline.
fn label_from_oneline(oneline: &str) -> String {
    if let Some(rest) = oneline.strip_prefix("Merge ")
        && let Some((_, after)) = rest.split_once('\'')
        && let Some((name, _)) = after.split_once('\'')
    {
        return name.to_string();
    }
    if let Some(rest) = oneline.strip_prefix("Merge pull request ")
        && let Some((_, from)) = rest.split_once(" from ")
    {
        return from.to_string();
    }
    oneline.to_string()
}

/// The sheet for `--rebase-merges`: each branch is rebuilt by `reset` to where it started, its commits, and a `label`
/// where a later `merge` or branch needs it. `bottom` is the first merge base, which the sheet calls `onto`.
pub(super) fn make_script(
    git: &GitHome,
    range: &Range,
    bottom: Option<ObjectId>,
    mode: &Mode,
) -> Result<Vec<Item>> {
    let mut state = LabelState::default();
    if let Some(b) = bottom {
        state.add(&b, "onto".into());
    }
    let branches = branch_names(git)?;
    let interesting: HashSet<ObjectId> = range.order.iter().copied().collect();
    let mut todo: HashMap<ObjectId, Item> = HashMap::new();
    let mut tips: Vec<ObjectId> = Vec::new();
    for id in &range.order {
        let commit = &range.commits[id];
        let empty = Commits::is_empty(git, id)?;
        if !empty && range.same_patch.contains(id) {
            continue;
        }
        let oneline = Commits::label(git, id)?;
        if commit.parents.len() < 2 {
            todo.insert(*id, Item::pick(*id));
            continue;
        }
        let label = label_from_oneline(&oneline);
        let mut names = Vec::new();
        for parent in &commit.parents[1..] {
            if !interesting.contains(parent) {
                names.push(state.label_oid(git, parent, None)?);
                continue;
            }
            tips.push(*parent);
            let base = branches.get(parent).map_or(label.as_str(), String::as_str);
            names.push(state.label_oid(git, parent, Some(base))?);
        }
        let mut item = Item::with_arg(Cmd::Merge, &format!("{} # {oneline}", names.join(" ")));
        item.id = Some(*id);
        item.flag = Flag::Replace;
        todo.insert(*id, item);
    }
    let mut child_seen: HashSet<ObjectId> = HashSet::new();
    for (n, id) in range.order.iter().enumerate() {
        for parent in &range.commits[id].parents {
            if interesting.contains(parent) && !child_seen.insert(*parent) {
                state.label_oid(git, parent, Some("branch-point"))?;
            }
        }
        if n + 1 == range.order.len() {
            tips.push(*id);
        }
    }

    let mut out = vec![Item::with_arg(Cmd::Label, "onto")];
    let mut shown: HashSet<ObjectId> = HashSet::new();
    for tip in tips {
        if shown.contains(&tip) {
            continue;
        }
        match state.by_commit.get(&tip) {
            Some(name) => {
                out.push(comment(""));
                out.push(comment(&format!("# Branch {name}")));
            }
            None => out.push(comment("")),
        }
        let mut list = Vec::new();
        let mut at = Some(tip);
        while let Some(id) = at {
            if !interesting.contains(&id) || shown.contains(&id) {
                break;
            }
            list.push(id);
            at = range.commits[&id].parents.first().copied();
        }
        match at {
            None => out.push(Item::with_arg(
                Cmd::Reset,
                if mode.cousins || mode.root_with_onto {
                    "onto"
                } else {
                    "[new root]"
                },
            )),
            Some(base) => {
                let to = match state.by_commit.get(&base) {
                    Some(name) => Some(name.clone()),
                    None if !mode.cousins => Some(state.label_oid(git, &base, None)?),
                    None => None,
                };
                match to {
                    Some(to) if to != "onto" => out.push(Item::with_arg(
                        Cmd::Reset,
                        &format!("{to} # {}", Commits::label(git, &base)?),
                    )),
                    _ => out.push(Item::with_arg(Cmd::Reset, "onto")),
                }
            }
        }
        for id in list.iter().rev() {
            if let Some(item) = todo.get(id) {
                out.push(item.clone());
            }
            if let Some(name) = state.by_commit.get(id) {
                out.push(Item::with_arg(Cmd::Label, name));
            }
            shown.insert(*id);
        }
    }
    Ok(out)
}

/// `refs/rewritten/<name>` when it exists, else the name as any revision: the commit and the name conflicts call it.
pub(super) fn lookup_label(git: &GitHome, name: &str) -> Result<(ObjectId, String)> {
    let full = format!("{REWRITTEN}{name}");
    if pyn_git::check_ref_name(&full).is_ok()
        && let Some(id) = git.repo.refs().resolve(&full)?
        && git.repo.commit(&id).is_ok()
    {
        return Ok((id, full));
    }
    match git.repo.resolve(name) {
        Ok(id) if git.repo.commit(&id).is_ok() => Ok((id, name.to_string())),
        _ => bail!("could not resolve '{name}'"),
    }
}

/// Whether `name` can follow `refs/rewritten/`.
pub(super) fn valid_label(name: &str) -> bool {
    pyn_git::check_ref_name(&format!("{REWRITTEN}{name}")).is_ok()
}

pub(super) fn do_label(git: &GitHome, dir: &Path, name: &str) -> Result<()> {
    let head = git
        .repo
        .head_commit()?
        .ok_or_else(|| anyhow!("could not read HEAD"))?;
    let full = format!("{REWRITTEN}{name}");
    git.repo
        .refs()
        .update(&full, &head)
        .map_err(|e| anyhow!("{e}"))?;
    append(dir, REFS_TO_DELETE, &format!("{full}\n"))
}

fn append(dir: &Path, name: &str, text: &str) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(name))?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

/// Deletes the refs `label` made, as the end of any rebase does.
pub(super) fn delete_rewritten(git: &GitHome, dir: &Path) {
    let Some(text) = read_text(&dir.join(REFS_TO_DELETE)) else {
        return;
    };
    for name in text.lines().filter(|l| !l.is_empty()) {
        let _ = git.repo.refs().delete(name, Expect::Any);
    }
}

/// A ref `--update-refs` moves when the rebase is done: where it was and where `update-ref` found `HEAD`.
pub(super) struct Tracked {
    pub name: String,
    pub before: ObjectId,
    pub after: Option<ObjectId>,
}

pub(super) fn write_tracked(git: &GitHome, dir: &Path, tracked: &[Tracked]) -> Result<()> {
    let zero = git.zero_id();
    let mut sorted: Vec<&Tracked> = tracked.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let text: String = sorted
        .iter()
        .map(|t| {
            format!(
                "{}\n{}\n{}\n",
                t.name,
                t.before,
                t.after.as_ref().unwrap_or(&zero)
            )
        })
        .collect();
    if tracked.is_empty() {
        let _ = std::fs::remove_file(dir.join(UPDATE_REFS));
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(UPDATE_REFS), text)?;
    Ok(())
}

pub(super) fn read_tracked(git: &GitHome, dir: &Path) -> Vec<Tracked> {
    let Some(text) = read_text(&dir.join(UPDATE_REFS)) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let zero = git.zero_id();
    lines
        .chunks(3)
        .filter_map(|c| {
            let [name, before, after] = c else {
                return None;
            };
            let parse = |h: &str| ObjectId::from_hex(git.algorithm(), h).ok();
            let after = parse(after).filter(|a| *a != zero);
            Some(Tracked {
                name: (*name).to_string(),
                before: parse(before)?,
                after,
            })
        })
        .collect()
}

/// `update-ref <ref>`: remembers where `HEAD` is for the ref.
pub(super) fn do_update_ref(git: &GitHome, dir: &Path, name: &str) -> Result<()> {
    let mut tracked = read_tracked(git, dir);
    if let Some(t) = tracked.iter_mut().find(|t| t.name == name) {
        t.after = git.repo.head_commit()?;
    }
    write_tracked(git, dir, &tracked)
}

/// Moves the tracked refs to where `update-ref` saw `HEAD`; one that changed meanwhile stays. Returns what to tell
/// the user once the rebase is announced.
pub(super) fn update_refs(git: &GitHome, dir: &Path, quiet: bool) -> Vec<String> {
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    for t in read_tracked(git, dir) {
        let Some(after) = t.after.filter(|a| *a != t.before) else {
            continue;
        };
        match git.move_ref(&t.name, Some(&t.before), &after, "rewritten during rebase") {
            Ok(()) => moved.push(t.name),
            Err(_) => failed.push(t.name),
        }
    }
    moved.sort();
    failed.sort();
    let mut out = Vec::new();
    if !failed.is_empty() {
        out.push("Failed to update the following refs with --update-refs:".to_string());
        out.extend(failed.iter().map(|n| format!("\t{n}")));
    }
    if !moved.is_empty() && !quiet {
        out.push("Updated the following refs with --update-refs:".to_string());
        out.extend(moved.iter().map(|n| format!("\t{n}")));
    }
    out
}

/// Puts an `update-ref` line after the last command of each commit a local branch points at, as `--update-refs`
/// does, and returns the refs to track. Branches are in reverse ref order at a commit; the rebased branch is left out.
pub(super) fn add_update_refs(
    git: &GitHome,
    items: Vec<Item>,
    head_ref: &str,
) -> Result<(Vec<Item>, Vec<Tracked>)> {
    let mut at: HashMap<ObjectId, Vec<(String, ObjectId)>> = HashMap::new();
    let mut refs = git.repo.refs().list("refs/heads/")?;
    refs.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, id) in refs {
        if name != head_ref {
            at.entry(id).or_default().insert(0, (name, id));
        }
    }
    let mut out = Vec::new();
    let mut tracked = Vec::new();
    let mut pending: Vec<(String, ObjectId)> = Vec::new();
    for (n, item) in items.iter().enumerate() {
        out.push(item.clone());
        let Some(id) = item.id else { continue };
        pending.extend(at.remove(&id).unwrap_or_default());
        let joins = items[n + 1..]
            .iter()
            .find(|i| i.counts())
            .is_some_and(|i| i.cmd.melds());
        if joins {
            continue;
        }
        for (name, before) in pending.drain(..) {
            out.push(Item::with_arg(Cmd::UpdateRef, &name));
            out.push(comment(""));
            tracked.push(Tracked {
                name,
                before,
                after: None,
            });
        }
    }
    Ok((out, tracked))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_come_from_the_subject_of_the_merge() {
        assert_eq!(
            label_from_oneline("Merge branch 'feat/x' into main"),
            "feat/x"
        );
        assert_eq!(
            label_from_oneline("Merge pull request #4 from me/topic"),
            "me/topic"
        );
        assert_eq!(label_from_oneline("A plain subject"), "A plain subject");
    }
}

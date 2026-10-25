//! `add -p`, `restore -p` and `stash push -p`: finds the changed files, asks about their hunks with the shared
//! prompt in `hunk_pick`, and applies what was chosen to the index and the work tree.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use pyn_diff::{Hunk, apply_hunks};
use pyn_git::IndexEntry;
use pyn_gitwork::{Entry, Pathspec, fs_path, worktree_mode};
use pyn_object::{FileMode, Kind as ObjectKind, ObjectId, hash_object};

use super::editor::{self, Which};
use super::hunk_pick::{FileDiff, Io, Item, Use, Wording, pick};
use super::stage::{
    Entries, OpenIndex, entries_of, index_of, link_target, put_entry, raw_mode, remove_entry,
};
use super::worktree::{remove_entry_file, write_bytes};
use super::{GitHome, convert, path_string};

const CONTEXT: usize = 3;

/// A file's mode and content after a choice is applied; `None` when the file is gone.
type Applied = Option<(FileMode, Vec<u8>)>;

#[derive(Clone)]
enum Source {
    Index,
    Worktree,
    Tree(ObjectId),
}

/// A mode of the prompt: which two states the hunks lead between and where a choice is written.
pub(super) struct PatchMode {
    old: Source,
    new: Source,
    /// Choosing applies the hunks backwards, to the state shown on their new side.
    reverse: bool,
    index: bool,
    worktree: bool,
    wording: Wording,
    /// Stashing looks only at the files of `HEAD`, and checks an edited hunk against `HEAD`.
    stash: bool,
}

impl PatchMode {
    fn new(
        (old, new): (Source, Source),
        reverse: bool,
        (index, worktree): (bool, bool),
        wording: Wording,
    ) -> Self {
        Self {
            old,
            new,
            reverse,
            index,
            worktree,
            wording,
            stash: false,
        }
    }

    fn add() -> Self {
        let words = Wording {
            verb: "stage",
            tail: "",
            gerund: "staging",
        };
        Self::new(
            (Source::Index, Source::Worktree),
            false,
            (true, false),
            words,
        )
    }

    pub(super) fn stash(head: ObjectId) -> Self {
        let words = Wording {
            verb: "stash",
            tail: "",
            gerund: "stashing",
        };
        Self {
            stash: true,
            ..Self::new(
                (Source::Tree(head), Source::Worktree),
                true,
                (false, true),
                words,
            )
        }
    }

    /// The mode of `restore -p`; `source` is the revision as typed, since only the word `HEAD` counts as the head.
    fn restore(git: &GitHome, source: Option<&str>, staged: bool, worktree: bool) -> Result<Self> {
        let to_worktree = worktree || !staged;
        let tree = match source {
            Some(rev) => Some((git.repo.resolve(rev)?, rev == "HEAD")),
            None if staged => match git.repo.head_commit()? {
                Some(head) => Some((head, true)),
                None => bail!("could not resolve HEAD"),
            },
            None => None,
        };
        let from_head = tree.is_none_or(|(_, head)| head);
        let (tail, gerund) = match (staged, to_worktree, from_head) {
            (true, true, true) => (" from index and worktree", "discarding"),
            (true, true, false) => (" to index and worktree", "applying"),
            (true, false, true) => ("", "unstaging"),
            (true, false, false) => (" to index", "applying"),
            (false, _, true) => (" from worktree", "discarding"),
            (false, _, false) => (" to worktree", "applying"),
        };
        let verb = match (staged && !to_worktree, from_head) {
            (true, true) => "unstage",
            (_, true) => "discard",
            _ => "apply",
        };
        let words = Wording { verb, tail, gerund };
        let targets = (staged, to_worktree);
        let current = if staged && !to_worktree {
            Source::Index
        } else {
            Source::Worktree
        };
        Ok(match tree {
            Some((commit, true)) => {
                Self::new((Source::Tree(commit), current), true, targets, words)
            }
            Some((commit, false)) => {
                Self::new((current, Source::Tree(commit)), false, targets, words)
            }
            None => Self::new((Source::Index, Source::Worktree), true, targets, words),
        })
    }

    /// Whether the diff is shown from the current state towards the source, which swaps the `a/` and `b/` names.
    fn flipped(&self) -> bool {
        matches!(self.new, Source::Tree(_))
    }
}

/// One side of a file: the state in the index, the work tree or a tree.
#[derive(Clone)]
struct Side {
    mode: FileMode,
    bytes: Vec<u8>,
    id: ObjectId,
}

#[derive(Default)]
struct Sides {
    index: Option<Side>,
    worktree: Option<Side>,
    tree: Option<Side>,
}

impl Sides {
    fn of(&self, source: &Source) -> Option<&Side> {
        match source {
            Source::Index => self.index.as_ref(),
            Source::Worktree => self.worktree.as_ref(),
            Source::Tree(_) => self.tree.as_ref(),
        }
    }
}

/// The hunks and mode change chosen for one file.
pub(super) struct Chosen {
    path: Vec<u8>,
    sides: Sides,
    old: Option<Side>,
    new: Option<Side>,
    hunks: Vec<Hunk>,
    mode: bool,
}

impl Chosen {
    /// What `target` becomes when the choices are applied forwards, or backwards with `reverse`; `None` removes it.
    fn apply(&self, reverse: bool, target: Option<&Side>) -> Result<Applied> {
        let desired = if reverse { &self.old } else { &self.new };
        let applied = !self.hunks.is_empty();
        if (applied && desired.is_none()) || (target.is_none() && !applied) {
            return Ok(None);
        }
        let name = path_string(&self.path);
        if self.mode
            && let (Some(t), Some(shown)) = (target, if reverse { &self.new } else { &self.old })
            && t.mode != shown.mode
        {
            anyhow::bail!(
                "{name} has type {:o}, expected {:o}",
                raw_mode(t.mode),
                raw_mode(shown.mode)
            );
        }
        let content = target.map_or(&[][..], |t| t.bytes.as_slice());
        let bytes = if applied {
            let hunks: Vec<Hunk> = if reverse {
                self.hunks.iter().map(Hunk::reversed).collect()
            } else {
                self.hunks.clone()
            };
            let refs: Vec<&Hunk> = hunks.iter().collect();
            apply_hunks(content, &refs).map_err(|m| {
                anyhow::anyhow!("patch failed: {name}:{}\n{name}: patch does not apply", m.0)
            })?
        } else {
            content.to_vec()
        };
        let mode = match (target, desired) {
            (Some(_), Some(d)) if self.mode => d.mode,
            (Some(t), _) => t.mode,
            (None, Some(d)) => d.mode,
            (None, None) => return Ok(None),
        };
        Ok(Some((mode, bytes)))
    }
}

fn read_side(git: &GitHome, entry: &Entry) -> Result<Option<Side>> {
    if entry.mode == FileMode::Gitlink {
        return Ok(None);
    }
    Ok(Some(Side {
        mode: entry.mode,
        bytes: git.repo.blob(&entry.id)?,
        id: entry.id,
    }))
}

fn read_worktree(git: &GitHome, path: &[u8], index: Option<&Entry>) -> Result<Option<Side>> {
    let file = fs_path(git.repo.root()?, path);
    let Ok(meta) = std::fs::symlink_metadata(&file) else {
        return Ok(None);
    };
    let Some(mut mode) = worktree_mode(&meta) else {
        return Ok(None);
    };
    let bytes = if mode == FileMode::Symlink {
        link_target(&file)?
    } else {
        std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?
    };
    if mode != FileMode::Symlink && !git.repo.bool_setting("core.filemode", true) {
        mode = match index.map(|e| e.mode) {
            Some(m @ (FileMode::Regular | FileMode::Executable)) => m,
            _ => FileMode::Regular,
        };
    }
    let id = hash_object(git.algorithm(), ObjectKind::Blob, &bytes)?;
    Ok(Some(Side { mode, bytes, id }))
}

fn abbrev(side: Option<&Side>) -> String {
    side.map_or_else(|| "0000000".to_string(), |s| s.id.to_hex()[..7].to_string())
}

fn mode_text(mode: FileMode) -> String {
    format!("{:o}", raw_mode(mode))
}

fn header(path: &str, old: Option<&Side>, new: Option<&Side>, flipped: bool) -> String {
    let (from, to) = if flipped { ("b", "a") } else { ("a", "b") };
    let mut out = format!("diff --git {from}/{path} {to}/{path}\n");
    match (old, new) {
        (None, Some(n)) => out.push_str(&format!("new file mode {}\n", mode_text(n.mode))),
        (Some(o), None) => out.push_str(&format!("deleted file mode {}\n", mode_text(o.mode))),
        _ => {}
    }
    let same_mode = old.zip(new).is_some_and(|(o, n)| o.mode == n.mode);
    let tail = match old {
        Some(o) if same_mode => format!(" {}", mode_text(o.mode)),
        _ => String::new(),
    };
    let unchanged = old.zip(new).is_some_and(|(o, n)| o.id == n.id);
    if unchanged {
        return out;
    }
    out.push_str(&format!("index {}..{}{tail}\n", abbrev(old), abbrev(new)));
    let name = |side: Option<&Side>, prefix: &str| {
        side.map_or_else(|| "/dev/null".to_string(), |_| format!("{prefix}/{path}"))
    };
    out.push_str(&format!("--- {}\n+++ {}\n", name(old, from), name(new, to)));
    out
}

/// The files with changes between the mode's two states that the prompt will ask about.
struct Found {
    files: Vec<(FileDiff, Chosen)>,
    binary: usize,
}

fn gather(git: &GitHome, opened: &OpenIndex, mode: &PatchMode, spec: &Pathspec) -> Result<Found> {
    let leaves = git.repo.index_leaves(&opened.index);
    let tree = [&mode.old, &mode.new].into_iter().find_map(|s| match s {
        Source::Tree(commit) => Some(*commit),
        _ => None,
    });
    let tree_map = match tree {
        Some(commit) => {
            let root = git.repo.commit(&commit)?.tree;
            git.repo.flatten(Some(&root), spec)?
        }
        None => BTreeMap::new(),
    };
    let unmerged: BTreeSet<&[u8]> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.stage > 0)
        .map(|e| e.path.as_slice())
        .collect();
    let touches_worktree =
        matches!(mode.old, Source::Worktree) || matches!(mode.new, Source::Worktree);
    let unstaged = if touches_worktree {
        git.repo.unstaged_changes(&opened.index)?
    } else {
        BTreeMap::new()
    };
    let mut paths: BTreeSet<&[u8]> = tree_map.keys().map(Vec::as_slice).collect();
    if !mode.stash {
        paths.extend(leaves.keys().map(Vec::as_slice));
        paths.extend(
            opened
                .index
                .entries
                .iter()
                .filter(|e| e.intent_to_add)
                .map(|e| e.path.as_slice()),
        );
    }
    let mut found = Found {
        files: Vec::new(),
        binary: 0,
    };
    for path in paths.into_iter().filter(|p| spec.matches(p)) {
        if unmerged.contains(path) {
            eprintln!("ignoring unmerged: {}", path_string(path));
            continue;
        }
        let known = |source: &Source| match source {
            Source::Index => Some(leaves.get(path).cloned()),
            Source::Tree(_) => Some(tree_map.get(path).cloned()),
            Source::Worktree => (leaves.contains_key(path) && !unstaged.contains_key(path))
                .then(|| leaves.get(path).cloned()),
        };
        if let (Some(a), Some(b)) = (known(&mode.old), known(&mode.new))
            && a == b
        {
            continue;
        }
        let sides = Sides {
            index: leaves
                .get(path)
                .map(|e| read_side(git, e))
                .transpose()?
                .flatten(),
            worktree: if touches_worktree {
                read_worktree(git, path, leaves.get(path))?
            } else {
                None
            },
            tree: tree_map
                .get(path)
                .map(|e| read_side(git, e))
                .transpose()?
                .flatten(),
        };
        let (old, new) = (sides.of(&mode.old).cloned(), sides.of(&mode.new).cloned());
        let kinds = old.as_ref().zip(new.as_ref());
        if kinds
            .is_some_and(|(o, n)| (o.mode == FileMode::Symlink) != (n.mode == FileMode::Symlink))
        {
            continue;
        }
        let bytes = |s: &Option<Side>| s.as_ref().map_or(Vec::new(), |s| s.bytes.clone());
        let (old_bytes, new_bytes) = (bytes(&old), bytes(&new));
        if pyn_diff::is_binary(&old_bytes) || pyn_diff::is_binary(&new_bytes) {
            if old_bytes != new_bytes {
                found.binary += 1;
            }
            continue;
        }
        let hunks = pyn_diff::hunks(&old_bytes, &new_bytes, CONTEXT, false);
        let mode_change = kinds.filter(|(o, n)| o.mode != n.mode);
        let mut items: Vec<Item> = Vec::new();
        if let Some((o, n)) = mode_change {
            items.push(Item::Mode {
                old: mode_text(o.mode),
                new: mode_text(n.mode),
            });
        }
        items.extend(hunks.into_iter().map(Item::Hunk));
        if items.is_empty() {
            continue;
        }
        let name = path_string(path);
        let diff = FileDiff {
            header: header(&name, old.as_ref(), new.as_ref(), mode.flipped()),
            path: name,
            uses: vec![Use::Undecided; items.len()],
            items,
            deleted: new.is_none(),
            added: old.is_none(),
            old: old_bytes,
            new: new_bytes,
            reverse: mode.reverse && !mode.stash,
        };
        let chosen = Chosen {
            path: path.to_vec(),
            sides,
            old,
            new,
            hunks: Vec::new(),
            mode: false,
        };
        found.files.push((diff, chosen));
    }
    Ok(found)
}

struct Terminal<'a> {
    git: &'a GitHome,
}

impl Io for Terminal<'_> {
    fn read_line(&mut self) -> Option<String> {
        let mut line = Vec::new();
        match std::io::stdin().lock().read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(
                String::from_utf8_lossy(&line)
                    .trim_end_matches(['\n', '\r'])
                    .to_string(),
            ),
        }
    }

    fn out(&mut self, text: &str) {
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    fn err(&mut self, text: &str) {
        eprint!("{text}");
    }

    fn edit(&mut self, text: &str) -> Result<Vec<u8>, String> {
        let path = self.git.repo.git().git_dir().join("addp-hunk-edit.diff");
        std::fs::write(&path, text).map_err(|e| format!("error: {e}\n"))?;
        let shown = self
            .git
            .repo
            .root()
            .ok()
            .and_then(|r| path.strip_prefix(r).ok());
        editor::run(self.git, Which::Message, &path).map_err(|e| {
            let shown = shown.unwrap_or(&path).display();
            format!("error: {e}\nerror: could not edit '{shown}'\n")
        })?;
        std::fs::read(&path).map_err(|e| format!("error: {e}\n"))
    }
}

/// Runs the prompt over the changed files, calling `each` with a file's choices as soon as its questions are over;
/// `None` after saying why when there is nothing to ask about.
fn choose(
    git: &GitHome,
    opened: &OpenIndex,
    mode: &PatchMode,
    spec: &Pathspec,
    each: &mut dyn FnMut(&Chosen, &mut dyn Io) -> Result<()>,
) -> Result<Option<Vec<Chosen>>> {
    let found = gather(git, opened, mode, spec)?;
    if found.files.is_empty() {
        println!(
            "{}",
            if found.binary > 0 {
                "Only binary files changed."
            } else {
                "No changes."
            }
        );
        return Ok(None);
    }
    let (mut diffs, pending): (Vec<FileDiff>, Vec<Chosen>) = found.files.into_iter().unzip();
    let mut pending = pending.into_iter();
    let mut done: Vec<Chosen> = Vec::new();
    let mut failure = None;
    pick(
        &mut diffs,
        &mode.wording,
        &mut Terminal { git },
        &mut |d, io| {
            let mut c = pending.next().expect("a choice for every file");
            for (item, used) in d.items.iter().zip(&d.uses) {
                if *used == Use::Yes {
                    match item.hunk() {
                        Some(h) => c.hunks.push(h.clone()),
                        None => c.mode = true,
                    }
                }
            }
            if failure.is_none()
                && (c.mode || !c.hunks.is_empty())
                && let Err(e) = each(&c, io)
            {
                failure = Some(e);
            }
            done.push(c);
        },
    );
    if let Some(e) = failure {
        return Err(e);
    }
    done.retain(|c| c.mode || !c.hunks.is_empty());
    Ok(Some(done))
}

fn spec_of(git: &GitHome, cwd: &Path, paths: &[String]) -> Result<Pathspec> {
    git.pathspec(cwd, paths, false)
}

/// `pyn add -p`.
pub fn add(git: &GitHome, cwd: &Path, paths: &[String]) -> Result<()> {
    patch(git, cwd, paths, &PatchMode::add())
}

/// `pyn restore -p`.
pub fn restore(
    git: &GitHome,
    cwd: &Path,
    paths: &[String],
    source: Option<&str>,
    staged: bool,
    worktree: bool,
) -> Result<()> {
    patch(
        git,
        cwd,
        paths,
        &PatchMode::restore(git, source, staged, worktree)?,
    )
}

/// Writes one file's choices to the index entries in memory and to the work tree; true when the index changed.
fn write_file(
    git: &GitHome,
    mode: &PatchMode,
    entries: &mut Entries,
    c: &Chosen,
    io: &mut dyn Io,
) -> Result<bool> {
    let mut to_index = mode.index;
    let staged = if mode.index {
        match c.apply(mode.reverse, c.sides.index.as_ref()) {
            Ok(result) => Some(result),
            Err(e) if mode.worktree => {
                for line in e.to_string().lines() {
                    io.err(&format!("error: {line}\n"));
                }
                io.out("The selected hunks do not apply to the index!\n");
                loop {
                    io.out("Apply them to the worktree anyway? ");
                    match io.read_line().and_then(|l| l.chars().next()) {
                        Some('y' | 'Y') => break,
                        Some('n' | 'N') | None => {
                            io.out("Nothing was applied.\n\n");
                            return Ok(false);
                        }
                        Some(_) => {}
                    }
                }
                to_index = false;
                None
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    if mode.worktree {
        let result = c.apply(mode.reverse, c.sides.worktree.as_ref())?;
        if let Some((m, _)) = &result {
            convert::refuse_conversions(git, [(c.path.as_slice(), *m)])?;
        }
        match result {
            Some((m, bytes)) => {
                write_bytes(git, &c.path, m, &bytes)?;
            }
            None => remove_entry_file(git, &c.path)?,
        }
    }
    match staged {
        Some(Some((m, bytes))) => {
            let id = git.repo.objects().write(ObjectKind::Blob, &bytes)?;
            put_entry(entries, IndexEntry::new(c.path.as_slice(), raw_mode(m), id));
        }
        Some(None) => remove_entry(entries, &c.path),
        None => {}
    }
    Ok(to_index)
}

fn patch(git: &GitHome, cwd: &Path, paths: &[String], mode: &PatchMode) -> Result<()> {
    let opened = git.open_index()?;
    let spec = spec_of(git, cwd, paths)?;
    let mut entries: Entries = entries_of(&opened.index);
    let mut changed = false;
    choose(git, &opened, mode, &spec, &mut |c, io| {
        changed |= write_file(git, mode, &mut entries, c, io)?;
        Ok(())
    })?;
    if changed {
        git.save_index(&opened, &index_of(opened.index.version, entries))?;
    }
    Ok(())
}

/// What `stash push -p` saves and takes out of the work tree.
pub(super) struct StashPatch {
    /// The `HEAD` tree's entries with the chosen hunks applied.
    pub(super) work: Entries,
    /// The work tree files with the chosen hunks taken out; `None` removes the file.
    pub(super) reverts: Vec<(Vec<u8>, Applied)>,
    /// Why the hunks could not be taken out of the work tree, once the stash is saved.
    pub(super) failure: Option<String>,
}

/// Asks about the changes between `HEAD` and the work tree; `None` when no hunk was chosen.
pub(super) fn stash(
    git: &GitHome,
    opened: &OpenIndex,
    head: ObjectId,
    spec: &Pathspec,
) -> Result<Option<StashPatch>> {
    let mode = PatchMode::stash(head);
    let Some(chosen) = choose(git, opened, &mode, spec, &mut |_, _| Ok(()))? else {
        return Ok(None);
    };
    if chosen.is_empty() {
        return Ok(None);
    }
    let root = git.repo.commit(&head)?.tree;
    let mut work: Entries = Entries::new();
    for (path, entry) in git.repo.flatten(Some(&root), &Pathspec::default())? {
        work.insert(
            (path.clone(), 0),
            IndexEntry::new(path, raw_mode(entry.mode), entry.id),
        );
    }
    let mut reverts = Vec::new();
    let mut failure = None;
    for c in &chosen {
        match c.apply(false, c.sides.tree.as_ref())? {
            Some((mode, bytes)) => {
                let id = git.repo.objects().write(ObjectKind::Blob, &bytes)?;
                put_entry(
                    &mut work,
                    IndexEntry::new(c.path.as_slice(), raw_mode(mode), id),
                );
            }
            None => remove_entry(&mut work, &c.path),
        }
        match c.apply(true, c.sides.worktree.as_ref()) {
            Ok(back) => reverts.push((c.path.clone(), back)),
            Err(e) => {
                failure = Some(e.to_string());
                break;
            }
        }
    }
    Ok(Some(StashPatch {
        work,
        reverts,
        failure,
    }))
}

//! A `pyn pull` that stopped on conflicts in a workspace with local commits (docs/concepts/commits.md): the merge or
//! rebase in progress, and how it is concluded or given up. It shares the record, the markers and the commands of
//! `pyn merge` (docs/concepts/merging.md); the server is not involved until the result is pushed.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context, Result, bail};
use pyn_git::Expect;
use pyn_gitwork::{Entry, Pathspec};
use pyn_object::{Commit, Kind, ObjectId, Signature};
use pyn_proto as api;
use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::git_home::clean_message;
use crate::git_home::meld;
use crate::git_home::todo::{Cmd, Flag, Item};
use crate::local_commit::signature;
use crate::local_store::{ALGORITHM, Files, LocalStore, build_trees, file_mode, parse_id};
use crate::local_sync::{
    Conflict, Joined, Judge, Rejected, Step, api_mode, dirty_paths, local_ref, my_locks,
};
use crate::merging::{self, MergeState, STATE_VERSION, Touched};
use crate::snapshot::short;
use crate::staging::{self, Index};
use crate::workspace::{FileState, Workspace, hash_of, set_read_only};
use crate::{localfile, sync};

/// What a pull that has stopped can be told to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Abort,
    Continue,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Merge,
    Rebase,
    Pick,
    Revert,
    /// `pyn rebase` over unpublished local commits.
    Rewrite,
}

impl Op {
    fn command(self) -> &'static str {
        match self {
            Op::Pick => "pyn cherry-pick",
            Op::Revert => "pyn revert",
            Op::Rewrite => "pyn rebase",
            _ => "pyn pull",
        }
    }

    /// The command word of a pick, revert or rebase.
    pub fn noun(self) -> &'static str {
        match self {
            Op::Revert => "revert",
            Op::Rewrite => "rebase",
            _ => "cherry-pick",
        }
    }

    fn replays(self) -> bool {
        matches!(self, Op::Pick | Op::Revert | Op::Rewrite)
    }

    /// Whether each commit made is announced as it is made.
    fn announces(self) -> bool {
        matches!(self, Op::Pick | Op::Revert)
    }
}

/// One command of a replay: a commit to apply, or an `exec` line or `break`.
pub type Move = Item;

fn move_line(item: &Move) -> String {
    match (item.cmd, item.id) {
        (Cmd::Pick, Some(id)) => id.to_hex(),
        (Cmd::Exec, _) => format!("exec {}", item.arg),
        (Cmd::Break, _) => "break".into(),
        (cmd, Some(id)) => format!("{} {}", cmd.word(), id.to_hex()),
        (cmd, None) => cmd.word().to_string(),
    }
}

fn parse_move(line: &str) -> Result<Move> {
    let line = line.trim();
    let (first, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(w, r)| (w, r.trim()));
    match (first, rest) {
        ("", _) => bail!("the record has an empty step"),
        ("break", "") => Ok(Item::with_arg(Cmd::Break, "")),
        ("exec", line) if !line.is_empty() => Ok(Item::exec(line)),
        (hex, "") => Ok(Item::pick(id_of(hex)?)),
        (word, hex) => {
            let cmd = Cmd::from_word(word)
                .with_context(|| format!("unknown command '{word}' in the record"))?;
            Ok(Item {
                cmd,
                ..Item::pick(id_of(hex)?)
            })
        }
    }
}

/// One side of a conflicted path: a blob of the local store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    id: String,
    mode: api::FileMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ours: Option<Side>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theirs: Option<Side>,
    #[serde(default)]
    exclusive: bool,
}

/// A file `--autostash` put aside: what it was at the tip and what the working tree held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stashed {
    base: Side,
    stash: Side,
}

/// The part of the merge record that only a pull knows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalOp {
    op: Op,
    /// The local tip when the pull started.
    orig: String,
    /// The fetched tip.
    theirs: String,
    /// The merged tree with a placeholder for each conflicted path: what the working tree holds apart from them.
    result_tree: String,
    #[serde(default)]
    sides: BTreeMap<String, Sides>,
    /// Rebase: the tip of the replayed commits so far, the commit being replayed and those still to come.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<String>,
    #[serde(default)]
    todo: Vec<String>,
    #[serde(default)]
    total: usize,
    #[serde(default)]
    dropped: usize,
    /// Cherry-pick: add the `(cherry picked from commit ...)` line to each message.
    #[serde(default)]
    origin: bool,
    /// Rebase: stopped on `edit`, `break` or an `exec`, not on a conflict.
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    no_ff: bool,
    #[serde(default)]
    autostash: BTreeMap<String, Stashed>,
}

impl LocalOp {
    pub fn name(&self) -> &'static str {
        match self.op {
            Op::Merge => "pull",
            Op::Rebase => "rebase",
            Op::Pick => "cherry-pick",
            Op::Revert => "revert",
            Op::Rewrite => "rebase",
        }
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn how_to_end(&self) -> String {
        if self.paused {
            return "go on with `pyn rebase --continue`, or give the rebase up with `pyn rebase --abort`"
                .into();
        }
        match self.op {
            Op::Merge => "conclude it with `pyn commit` or give it up with `pyn pull --abort`".into(),
            Op::Rebase => {
                "conclude this step with `pyn commit` (or `pyn pull --continue`), drop the commit with `pyn pull --skip`, \
                 or give the rebase up with `pyn pull --abort`"
                    .into()
            }
            Op::Pick | Op::Revert | Op::Rewrite => {
                let cmd = self.op.command();
                format!(
                    "conclude this step with `pyn commit` (or `{cmd} --continue`), drop the commit with `{cmd} --skip`, \
                     or give the {} up with `{cmd} --abort`",
                    self.name()
                )
            }
        }
    }

    pub fn header(&self) -> String {
        if self.paused {
            let stop = self
                .current
                .as_deref()
                .and_then(|l| l.split_whitespace().next())
                .filter(|w| matches!(*w, "break" | "exec"))
                .unwrap_or("edit");
            return format!(
                "You are rebasing (stopped at {stop}, {} left).\n  \
                 (use \"pyn rebase --continue\" when you are done, \"pyn rebase --abort\" to give up the rebase)\n",
                self.todo.len()
            );
        }
        match self.op {
            Op::Merge => "  (use \"pyn pull --abort\" to abort the merge)\n".into(),
            Op::Rebase => {
                let at = self.total - self.todo.len();
                format!(
                    "You are rebasing your local commits (commit {at} of {}, {} left).\n  \
                     (use \"pyn pull --skip\" to drop this commit, \"pyn pull --abort\" to give up the rebase)\n",
                    self.total,
                    self.todo.len()
                )
            }
            Op::Pick | Op::Revert | Op::Rewrite => {
                let at = self.total - self.todo.len();
                let cmd = self.op.command();
                let doing = match self.op {
                    Op::Pick => "cherry-picking",
                    Op::Revert => "reverting",
                    _ => "rebasing",
                };
                format!(
                    "You are {doing} (commit {at} of {}, {} left).\n  \
                     (use \"{cmd} --skip\" to drop this commit, \"{cmd} --abort\" to give up)\n",
                    self.total,
                    self.todo.len()
                )
            }
        }
    }
}

fn id_of(hex: &str) -> Result<ObjectId> {
    parse_id(hex)
}

fn flat(store: &LocalStore, commit: Option<ObjectId>) -> Result<Files> {
    let tree = store.tree_of(commit)?;
    Ok(store
        .repo()
        .flatten(tree.as_ref(), &Pathspec::default())?
        .into_iter()
        .collect())
}

fn flat_tree(store: &LocalStore, tree: &ObjectId) -> Result<Files> {
    Ok(store
        .repo()
        .flatten(Some(tree), &Pathspec::default())?
        .into_iter()
        .collect())
}

fn side(entry: Option<Entry>) -> Result<Option<Side>> {
    entry
        .map(|e| {
            Ok(Side {
                id: e.id.to_hex(),
                mode: api_mode(e.mode)?,
            })
        })
        .transpose()
}

fn name(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn write_trees(store: &LocalStore, files: &Files) -> Result<ObjectId> {
    let (tree, trees) = build_trees(files)?;
    for (_, body) in trees {
        store.repo().objects().write(Kind::Tree, &body)?;
    }
    Ok(tree)
}

/// Brings the working tree from `from` to `to`, with `working` bytes where given; only `force` paths may overwrite
/// uncommitted changes and `editable` ones stay writable. Returns the paths written.
fn materialize(
    store: &LocalStore,
    remote: &dyn Backend,
    ws: &Workspace,
    (from, to): (&Files, &Files),
    working: &BTreeMap<Vec<u8>, Vec<u8>>,
    (force, editable): (&BTreeSet<String>, &BTreeSet<String>),
) -> Result<BTreeSet<String>> {
    let mut keys: BTreeSet<Vec<u8>> = from
        .keys()
        .chain(to.keys())
        .filter(|k| from.get(*k) != to.get(*k))
        .cloned()
        .collect();
    keys.extend(working.keys().cloned());
    keys.extend(
        force
            .iter()
            .map(|n| n.clone().into_bytes())
            .filter(|k| from.contains_key(k) || to.contains_key(k)),
    );
    let names: Vec<String> = keys.iter().map(|k| name(k)).collect();
    ws.check_paths(&names)?;
    let mut state = ws.load_state()?;
    let dirty = dirty_paths(ws)?;
    let blocked: Vec<&String> = names
        .iter()
        .zip(&keys)
        .filter(|(n, _)| !force.contains(*n))
        .filter(|(n, k)| {
            dirty.contains(*n)
                || (!state.contains_key(*n) && to.contains_key(*k) && localfile::exists(&ws.abs(n)))
        })
        .map(|(n, _)| n)
        .collect();
    if !blocked.is_empty() {
        return Err(Rejected(format!(
            "error: your local changes to the following files would be overwritten:\n\t{}\n\
             commit them, or put them back with `pyn checkout -- <path>`, and run the command again; nothing was changed",
            blocked
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join("\n\t")
        ))
        .into());
    }
    let mut written = BTreeSet::new();
    for (name, key) in names.iter().zip(&keys) {
        let abs = ws.abs(name);
        let was_read_only = localfile::exists(&abs)
            && std::fs::metadata(&abs).is_ok_and(|m| m.permissions().readonly());
        match to.get(key) {
            Some(entry) => {
                let bytes = match working.get(key) {
                    Some(b) => b.clone(),
                    None => store.blob(remote, &entry.id.to_hex())?,
                };
                set_read_only(&abs, false)?;
                ws.write_file(name, &bytes)?;
                let mode = api_mode(entry.mode)?;
                localfile::apply(&abs, name, mode)?;
                if was_read_only && !editable.contains(name) {
                    set_read_only(&abs, true)?;
                }
                let hash = hash_of(&bytes);
                match state.get_mut(name) {
                    Some(known) => {
                        if !working.contains_key(key) {
                            known.hash = hash;
                            known.legacy = false;
                        }
                        known.file_mode = mode;
                    }
                    None => {
                        state.insert(
                            name.clone(),
                            FileState {
                                revision: 0,
                                hash,
                                legacy: false,
                                mode: "shared".into(),
                                file_mode: mode,
                            },
                        );
                    }
                }
            }
            None => {
                set_read_only(&abs, false)?;
                let _ = std::fs::remove_file(&abs);
                state.remove(name);
            }
        }
        written.insert(name.clone());
    }
    ws.save_state(&state)?;
    Ok(written)
}

/// What a stopped merge needs to be recorded: the commits it joins and the message of the merge commit.
pub struct Stop {
    pub orig: ObjectId,
    pub theirs: ObjectId,
    pub message: String,
}

fn conflict_lines(conflicts: &BTreeMap<Vec<u8>, Conflict>, kept: &[String]) -> (String, bool) {
    let mut text = String::new();
    for path in kept {
        text.push_str(&format!("Kept your version of {path}: you hold its lock\n"));
    }
    let mut exclusive = false;
    for (path, c) in conflicts {
        let path = name(path);
        text.push_str(&if c.exclusive {
            exclusive = true;
            format!("CONFLICT (exclusive): {path} is exclusive and changed on both sides\n")
        } else if c.ours.is_none() || c.theirs.is_none() {
            format!("CONFLICT (modify/delete): {path} was removed on one side and changed on the other\n")
        } else if c.working.is_some() {
            format!("CONFLICT (content): Merge conflict in {path}\n")
        } else {
            format!("CONFLICT (binary or file mode): {path} cannot be merged; the working tree has your side\n")
        });
    }
    (text, exclusive)
}

const RESOLVE: &str = "fix conflicts, stage each with `pyn add <path>` (a file you deleted is staged the same way), then run `pyn commit`";
const SIDES: &str = "hint: To keep the version of a file on one side, use `pyn checkout --ours|--theirs -- <path>`.";
const LOCKS: &str = "hint: An exclusive file needs its lock to be concluded: choose a side, then `pyn lock <path>`.";

fn sides_of(conflicts: &BTreeMap<Vec<u8>, Conflict>) -> Result<BTreeMap<String, Sides>> {
    conflicts
        .iter()
        .map(|(p, c)| {
            Ok((
                name(p),
                Sides {
                    ours: side(c.ours)?,
                    theirs: side(c.theirs)?,
                    exclusive: c.exclusive,
                },
            ))
        })
        .collect()
}

/// Writes a stopped merge or replay into the working tree and records it; the caller reports it.
#[allow(clippy::too_many_arguments)]
fn record(
    store: &LocalStore,
    backend: &dyn Backend,
    ws: &Workspace,
    branch: &str,
    disk: &Files,
    files: &Files,
    conflicts: &BTreeMap<Vec<u8>, Conflict>,
    op: LocalOp,
    message: String,
) -> Result<()> {
    let working: BTreeMap<Vec<u8>, Vec<u8>> = conflicts
        .iter()
        .filter_map(|(p, c)| c.working.clone().map(|w| (p.clone(), w)))
        .collect();
    let editable: BTreeSet<String> = conflicts.keys().map(|p| name(p)).collect();
    let written = materialize(
        store,
        backend,
        ws,
        (disk, files),
        &working,
        (&BTreeSet::new(), &editable),
    )?;
    let mut paths = BTreeMap::new();
    let touched = if op.paused { BTreeSet::new() } else { written };
    for path in touched.iter().chain(editable.iter()) {
        let c = conflicts.get(path.as_bytes());
        paths.insert(
            path.clone(),
            Touched {
                mode: if c.is_some_and(|c| c.exclusive) {
                    "exclusive".into()
                } else {
                    "shared".into()
                },
                conflict: c.map(|c| c.label.to_string()),
                ours: None,
                theirs: None,
                line_head: None,
            },
        );
    }
    Index::default().save(ws)?;
    merging::save(
        ws,
        &MergeState {
            version: STATE_VERSION,
            branch: branch.to_string(),
            source: match op.op {
                Op::Pick | Op::Revert | Op::Rewrite => op.op.noun().to_string(),
                _ => format!("origin/{branch}"),
            },
            source_commit: op.theirs.clone(),
            target_commit: op.orig.clone(),
            message,
            no_ff: false,
            paths,
            local: Some(op),
        },
    )
}

/// Leaves a merge of the local branch and the fetched commits in progress, with markers in the conflicted files.
pub fn stop_merge(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    branch: &str,
    stop: Stop,
    joined: Joined,
) -> Result<()> {
    let result_tree = write_trees(store, &joined.files)?;
    let op = LocalOp {
        op: Op::Merge,
        orig: stop.orig.to_hex(),
        theirs: stop.theirs.to_hex(),
        result_tree: result_tree.to_hex(),
        sides: sides_of(&joined.conflicts)?,
        tip: None,
        current: None,
        todo: Vec::new(),
        total: 0,
        dropped: 0,
        origin: false,
        paused: false,
        no_ff: false,
        autostash: BTreeMap::new(),
    };
    let disk = flat(store, Some(stop.orig))?;
    record(
        store,
        backend,
        ws,
        branch,
        &disk,
        &joined.files,
        &joined.conflicts,
        op,
        stop.message,
    )?;
    let (mut text, exclusive) = conflict_lines(&joined.conflicts, &joined.kept);
    text.push_str(&format!("Automatic merge failed; {RESOLVE}.\n{SIDES}\n"));
    if exclusive {
        text.push_str(&format!("{LOCKS}\n"));
    }
    text.push_str("hint: To put everything back as it was, run `pyn pull --abort`.");
    Err(Rejected(text).into())
}

/// The local commits being replayed onto the fetched tip.
struct Run {
    op: Op,
    origin: bool,
    orig: ObjectId,
    onto: ObjectId,
    tip: ObjectId,
    todo: VecDeque<Move>,
    total: usize,
    dropped: usize,
    no_ff: bool,
    stash: BTreeMap<String, Stashed>,
}

impl Run {
    fn new(op: Op, orig: ObjectId, onto: ObjectId, tip: ObjectId, todo: VecDeque<Move>) -> Self {
        Run {
            op,
            origin: false,
            orig,
            onto,
            tip,
            total: todo.len(),
            todo,
            dropped: 0,
            no_ff: false,
            stash: BTreeMap::new(),
        }
    }

    /// The record of this run stopped at `current`: on conflicts, or `paused` on `edit`, `break` or `exec`.
    fn record_of(
        &self,
        result_tree: ObjectId,
        current: &Move,
        sides: BTreeMap<String, Sides>,
        paused: bool,
    ) -> LocalOp {
        LocalOp {
            op: self.op,
            origin: self.origin,
            orig: self.orig.to_hex(),
            theirs: self.onto.to_hex(),
            result_tree: result_tree.to_hex(),
            sides,
            tip: Some(self.tip.to_hex()),
            current: Some(move_line(current)),
            todo: self.todo.iter().map(move_line).collect(),
            total: self.total,
            dropped: self.dropped,
            paused,
            no_ff: self.no_ff,
            autostash: self.stash.clone(),
        }
    }
}

enum Replayed {
    Done(Run),
    Stopped {
        run: Run,
        current: Move,
        files: Files,
        conflicts: BTreeMap<Vec<u8>, Conflict>,
        kept: Vec<String>,
    },
    /// `edit` after its commit was made, `break`, or an `exec` line to run.
    Paused {
        run: Run,
        item: Move,
        files: Files,
    },
}

fn subject(message: &[u8]) -> String {
    String::from_utf8_lossy(message)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

/// Replays the commands of `run.todo` over its tip, stopping at the first that has a conflict.
fn replay(
    store: &LocalStore,
    backend: &dyn Backend,
    ws: &Workspace,
    (who, branch): (&Signature, &str),
    mut run: Run,
) -> Result<Replayed> {
    let repo = store.repo();
    let mut judge = Judge::new(backend, run.onto);
    if run.op.replays() {
        judge.never_keep();
    }
    let mut files = flat(store, Some(run.tip))?;
    let mut parent_tree = repo.commit(&run.tip)?.tree;
    while let Some(item) = run.todo.pop_front() {
        let cmd = item.cmd;
        match cmd {
            Cmd::Noop | Cmd::Drop | Cmd::Comment => continue,
            Cmd::Break | Cmd::Exec => return Ok(Replayed::Paused { run, item, files }),
            _ => {}
        }
        let id = item.id.context("a command without a commit")?;
        let commit = repo.commit(&id)?;
        let stops = cmd == Cmd::Edit;
        let same_parent = commit.parents.first() == Some(&run.tip);
        if run.op == Op::Rewrite
            && matches!(cmd, Cmd::Pick | Cmd::Edit)
            && same_parent
            && !run.no_ff
        {
            run.tip = id;
            files = flat_tree(store, &commit.tree)?;
            parent_tree = commit.tree;
            if stops {
                return Ok(Replayed::Paused { run, item, files });
            }
            continue;
        }
        let parent = store.tree_of(commit.parents.first().copied())?;
        // A revert merges with the commit as the base and its parent as the incoming side.
        let (before, after) = match run.op {
            Op::Revert => (Some(commit.tree), parent),
            _ => (parent, Some(commit.tree)),
        };
        let theirs_label = format!("{} ({})", short(&id.to_hex()), subject(&commit.message));
        let (mut conflicts, mut kept) = (BTreeMap::new(), Vec::new());
        for change in repo.diff_trees(before.as_ref(), after.as_ref(), &Pathspec::default())? {
            let ours = files.get(&change.path).copied();
            let sides = (change.old, ours, change.new);
            match store.join_path(
                backend,
                &mut judge,
                &change.path,
                sides,
                true,
                ("HEAD", &theirs_label),
            )? {
                Step::Take(entry) => set_entry(&mut files, &change.path, entry),
                Step::Kept(entry) => {
                    set_entry(&mut files, &change.path, entry);
                    kept.push(name(&change.path));
                }
                Step::Conflict(c) => {
                    set_entry(&mut files, &change.path, c.placeholder);
                    conflicts.insert(change.path.clone(), c);
                }
            }
        }
        if cmd.melds() {
            std::fs::create_dir_all(meld_dir(ws))?;
            let head = repo.commit(&run.tip)?;
            let text = meld::add(
                &meld_dir(ws),
                &String::from_utf8_lossy(&head.message),
                &String::from_utf8_lossy(&commit.message),
                cmd,
                Flag::None,
                &id,
            )?;
            if conflicts.is_empty() {
                let tree = write_trees(store, &files)?;
                let message = melded_message(ws, more_melds(&run), &text)?;
                let record = Commit {
                    tree,
                    parents: head.parents,
                    author: head.author,
                    committer: who.clone(),
                    extra_headers: Vec::new(),
                    message,
                };
                run.tip = repo
                    .objects()
                    .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
                parent_tree = tree;
                if !more_melds(&run) {
                    meld::clear(&meld_dir(ws));
                }
                continue;
            }
        }
        if !conflicts.is_empty() {
            return Ok(Replayed::Stopped {
                run,
                current: item,
                files,
                conflicts,
                kept,
            });
        }
        for path in kept {
            println!("Kept your version of {path}: you hold its lock");
        }
        let tree = write_trees(store, &files)?;
        if tree == parent_tree {
            run.dropped += 1;
            if run.op.replays() {
                println!(
                    "{} changes nothing here and is dropped",
                    short(&id.to_hex())
                );
            }
            continue;
        }
        let message = if cmd == Cmd::Reword {
            crate::local_rebase::edit_message(ws, &commit.message)?
        } else {
            replayed_message(run.op, run.origin, &id, commit.message)
        };
        let record = Commit {
            tree,
            parents: vec![run.tip],
            author: replayed_author(run.op, who, commit.author),
            committer: who.clone(),
            extra_headers: Vec::new(),
            message,
        };
        run.tip = repo
            .objects()
            .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
        parent_tree = tree;
        if run.op.announces() {
            print_pick(branch, &run.tip, &record.message);
        }
        if stops {
            return Ok(Replayed::Paused { run, item, files });
        }
    }
    Ok(Replayed::Done(run))
}

fn meld_dir(ws: &Workspace) -> std::path::PathBuf {
    ws.dir.join("local_only").join("state").join("rebase-meld")
}

fn more_melds(run: &Run) -> bool {
    run.todo.front().is_some_and(|item| item.cmd.melds())
}

/// The message of the commit a squash chain makes now: the collected text while more members follow, then the first
/// message alone when only fixups joined it, else what the person makes of the collected text.
fn melded_message(ws: &Workspace, more: bool, text: &str) -> Result<Vec<u8>> {
    if more {
        return Ok(text.as_bytes().to_vec());
    }
    if let Ok(only) = std::fs::read(meld_dir(ws).join(meld::FIXUP)) {
        return Ok(only);
    }
    crate::local_rebase::edit_message(ws, text.as_bytes())
}

fn set_entry(files: &mut Files, path: &[u8], entry: Option<Entry>) {
    match entry {
        Some(entry) => files.insert(path.to_vec(), entry),
        None => files.remove(path),
    };
}

fn replayed_message(op: Op, origin: bool, id: &ObjectId, message: Vec<u8>) -> Vec<u8> {
    match op {
        Op::Revert => crate::local_pick::revert_message(&message, id, None),
        _ if origin => crate::local_pick::with_origin(message, id),
        _ => message,
    }
}

/// A revert is the reverting person's change; anything else keeps its author.
fn replayed_author(op: Op, who: &Signature, author: Signature) -> Signature {
    if op == Op::Revert {
        who.clone()
    } else {
        author
    }
}

fn print_pick(branch: &str, id: &ObjectId, message: &[u8]) {
    println!("[{branch} {}] {}", short(&id.to_hex()), subject(message));
}

/// The commits a cherry-pick or revert applies on top of `tip`; `onto` is the commit whose modes the server is asked about.
pub struct Pick {
    pub op: Op,
    pub commits: Vec<ObjectId>,
    pub tip: ObjectId,
    pub onto: ObjectId,
    pub origin: bool,
}

/// Applies the commits one after the other; a conflict leaves the cherry-pick in progress.
pub fn begin_pick(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    branch: &str,
    pick: Pick,
    who: &Signature,
) -> Result<()> {
    let mut run = Run::new(
        pick.op,
        pick.tip,
        pick.onto,
        pick.tip,
        pick.commits.into_iter().map(Item::pick).collect(),
    );
    run.origin = pick.origin;
    let disk = flat(store, Some(pick.tip))?;
    proceed(backend, ws, store, (branch, who), run, Resume::new(&disk))
}

/// Replays the local commits onto the fetched tip; a conflict leaves the rebase in progress.
pub fn begin_rebase(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    branch: &str,
    outgoing: crate::local_sync::Outgoing,
    onto: ObjectId,
    who: &Signature,
) -> Result<()> {
    for id in &outgoing.commits {
        if store.repo().commit(id)?.parents.len() > 1 {
            return Err(Rejected(format!(
                "local commit {} is a merge commit, which cannot be rebased; use `pyn pull` to merge\nnothing was changed",
                short(&id.to_hex())
            ))
            .into());
        }
    }
    let orig = *outgoing.commits.last().expect("outgoing has commits");
    let run = Run::new(
        Op::Rebase,
        orig,
        onto,
        onto,
        outgoing.commits.into_iter().map(Item::pick).collect(),
    );
    let disk = flat(store, Some(orig))?;
    proceed(backend, ws, store, (branch, who), run, Resume::new(&disk))
}

/// The commands of a `pyn rebase` over unpublished commits: replayed over `base` from the local tip `tip`;
/// `published` is the commit whose modes the server is asked about.
pub struct Rewrite {
    pub moves: Vec<Move>,
    pub base: ObjectId,
    pub tip: ObjectId,
    pub published: ObjectId,
    pub no_ff: bool,
    pub stash: BTreeMap<String, Stashed>,
}

/// Replays the commands over the base; a conflict leaves the rebase in progress.
pub fn begin_rewrite(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    branch: &str,
    rewrite: Rewrite,
    who: &Signature,
) -> Result<()> {
    let _ = std::fs::remove_dir_all(meld_dir(ws));
    let mut run = Run::new(
        Op::Rewrite,
        rewrite.tip,
        rewrite.published,
        rewrite.base,
        rewrite.moves.into(),
    );
    run.no_ff = rewrite.no_ff;
    run.stash = rewrite.stash;
    let disk = flat(store, Some(rewrite.tip))?;
    proceed(backend, ws, store, (branch, who), run, Resume::new(&disk))
}

/// Where `proceed` starts: the files on disk, the paths that may overwrite uncommitted changes, and whether to stop
/// at the tip first (an `edit` whose conflict was just resolved).
struct Resume<'a> {
    disk: &'a Files,
    force: BTreeSet<String>,
    halt: bool,
}

impl<'a> Resume<'a> {
    fn new(disk: &'a Files) -> Self {
        Resume {
            disk,
            force: BTreeSet::new(),
            halt: false,
        }
    }
}

/// The commit message line `edit` and `break` stops report.
fn stop_line(store: &LocalStore, tip: &ObjectId) -> Result<String> {
    let commit = store.repo().commit(tip)?;
    Ok(format!(
        "{} ({})",
        short(&tip.to_hex()),
        subject(&commit.message)
    ))
}

/// Runs an `exec` line in the working tree root; a failure, or changes it leaves behind, stops the rebase.
fn run_exec(ws: &Workspace, line: &str) -> Result<()> {
    eprintln!("Executing: {line}");
    let ran = std::process::Command::new("sh")
        .arg("-c")
        .arg(line)
        .current_dir(&ws.root)
        .status();
    if !ran.is_ok_and(|s| s.success()) {
        return Err(Rejected(format!(
            "warning: execution failed: {line}\nYou can fix the problem, and then run\n\n  pyn rebase --continue\n"
        ))
        .into());
    }
    if !dirty_paths(ws)?.is_empty() || !Index::load(ws)?.entries.is_empty() {
        return Err(Rejected(format!(
            "error: execution succeeded: {line}\nbut left changes to the index and/or the working tree.\nCommit your changes, or put them back with `pyn checkout -- <path>`, and run\n\n  pyn rebase --continue\n"
        ))
        .into());
    }
    Ok(())
}

/// Carries a rebase or cherry-pick on from `run` with the files of `start` on disk: to its end, to the next
/// conflict, or to the next `edit`, `break` or failed `exec`.
fn proceed(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    (branch, who): (&str, &Signature),
    mut run: Run,
    start: Resume<'_>,
) -> Result<()> {
    let mut at = match store.tip(branch)? {
        Some(now) if run.op == Op::Rewrite => now,
        _ => run.orig,
    };
    let mut disk = start.disk.clone();
    let mut halt = start.halt;
    loop {
        let replayed = if std::mem::take(&mut halt) {
            let item = Item {
                cmd: Cmd::Edit,
                ..Item::pick(run.tip)
            };
            let files = flat(store, Some(run.tip))?;
            Replayed::Paused { run, item, files }
        } else {
            replay(store, backend, ws, (who, branch), run)?
        };
        match replayed {
            Replayed::Done(done) => {
                let end = flat(store, Some(done.tip))?;
                materialize(
                    store,
                    backend,
                    ws,
                    (&disk, &end),
                    &BTreeMap::new(),
                    (&start.force, &BTreeSet::new()),
                )?;
                store
                    .repo()
                    .refs()
                    .update_checked(&local_ref(branch), Expect::Id(at), &done.tip)
                    .context("the local branch moved while pulling; run the command again")?;
                merging::clear(ws)?;
                Index::default().save(ws)?;
                let _ = std::fs::remove_dir_all(meld_dir(ws));
                if done.op == Op::Rewrite {
                    println!("Successfully rebased and updated refs/heads/{branch}.");
                    unstash(backend, ws, store, &done.stash, (done.tip, done.onto))?;
                }
                if done.op.replays() {
                    return Ok(());
                }
                println!(
                    "Rebased {} local commit(s) onto {}{}",
                    done.total - done.dropped,
                    short(&done.onto.to_hex()),
                    if done.dropped > 0 {
                        format!(" ({} became empty and was dropped)", done.dropped)
                    } else {
                        String::new()
                    }
                );
                return sync::update(backend, ws, &[], None);
            }
            Replayed::Stopped {
                run: stopped,
                current,
                files,
                conflicts,
                kept,
            } => {
                let cmd = stopped.op.command();
                let id = current.id.context("a stop without a commit")?;
                let commit = store.repo().commit(&id)?;
                let result_tree = write_trees(store, &files)?;
                let op = stopped.record_of(result_tree, &current, sides_of(&conflicts)?, false);
                let message = match std::fs::read(meld_dir(ws).join(meld::SQUASH)) {
                    Ok(text) if current.cmd.melds() => String::from_utf8_lossy(&text).into_owned(),
                    _ => String::from_utf8_lossy(&replayed_message(
                        stopped.op,
                        stopped.origin,
                        &id,
                        commit.message.clone(),
                    ))
                    .into_owned(),
                };
                let at_n = stopped.total - stopped.todo.len();
                let (mut text, exclusive) = conflict_lines(&conflicts, &kept);
                text.push_str(&format!(
                    "error: could not apply {} ({}), commit {at_n} of {}\n{RESOLVE} (or `{cmd} --continue`).\n{SIDES}\n",
                    short(&id.to_hex()),
                    subject(&commit.message),
                    stopped.total
                ));
                if exclusive {
                    text.push_str(&format!("{LOCKS}\n"));
                }
                text.push_str(&format!(
                    "hint: To drop this commit, run `{cmd} --skip`; to put everything back as it was, `{cmd} --abort`."
                ));
                record(
                    store, backend, ws, branch, &disk, &files, &conflicts, op, message,
                )?;
                return Err(Rejected(text).into());
            }
            Replayed::Paused {
                run: paused,
                item,
                files,
            } => {
                run = paused;
                let result_tree = write_trees(store, &files)?;
                let op = run.record_of(result_tree, &item, BTreeMap::new(), true);
                record(
                    store,
                    backend,
                    ws,
                    branch,
                    &disk,
                    &files,
                    &BTreeMap::new(),
                    op,
                    String::new(),
                )?;
                if at != run.tip {
                    store.move_tip(branch, at, run.tip)?;
                    at = run.tip;
                }
                disk = files;
                match item.cmd {
                    Cmd::Exec => {
                        run_exec(ws, &item.arg)?;
                        if let Some(now) = store.tip(branch)? {
                            run.tip = now;
                            at = now;
                            disk = flat(store, Some(now))?;
                        }
                    }
                    Cmd::Break => {
                        eprintln!("Stopped at {}", stop_line(store, &run.tip)?);
                        return Ok(());
                    }
                    _ => {
                        eprintln!("Stopped at {}", stop_line(store, &run.tip)?);
                        eprintln!(
                            "You can change files and add commits now with `pyn add` and `pyn commit`.\n\nOnce you are satisfied, run\n\n  pyn rebase --continue"
                        );
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn open(ws: &Workspace) -> Result<(LocalStore, MergeState, LocalOp)> {
    let Some(mut state) = merging::load(ws)? else {
        bail!("there is no pull in progress");
    };
    let Some(op) = state.local.take() else {
        bail!(
            "the merge in progress was not made by `pyn pull`; use `pyn merge --abort` or `pyn commit`"
        );
    };
    let store = LocalStore::open(ws).context("the local store is missing")?;
    Ok((store, state, op))
}

fn in_progress(ws: &Workspace) -> Result<Option<Op>> {
    Ok(merging::load(ws)?.and_then(|s| s.local).map(|l| l.op))
}

/// Stops a cherry-pick or revert command when none of its kind is in progress.
pub fn require_op(ws: &Workspace, op: Op) -> Result<()> {
    match in_progress(ws)? {
        Some(found) if found == op => Ok(()),
        Some(found) if found.replays() => bail!(
            "a {} is in progress, not a {}; use `{} --continue`, `--skip` or `--abort`",
            found.noun(),
            op.noun(),
            found.command()
        ),
        _ => bail!("there is no {} in progress", op.noun()),
    }
}

/// Stops a pull command while a cherry-pick or revert is in progress.
pub fn require_pull(ws: &Workspace) -> Result<()> {
    if let Some(op) = in_progress(ws)?.filter(|o| o.replays()) {
        bail!(
            "a {} is in progress, not a pull; use `{} --continue`, `--skip` or `--abort`",
            op.noun(),
            op.command()
        );
    }
    Ok(())
}

/// Records the staged resolutions as what the working tree holds.
fn adopt(ws: &Workspace, index: &Index) -> Result<()> {
    let mut state = ws.load_state()?;
    for (path, staged) in &index.entries {
        if staged.deleted {
            state.remove(path);
        } else if let Some(known) = state.get_mut(path) {
            known.hash = staged.hash.clone();
            known.legacy = false;
            known.file_mode = staged.file_mode;
        }
    }
    ws.save_state(&state)
}

/// Concludes the pull in progress with the staged resolutions: the merge commit, or the replayed commit and the rest.
pub fn finish(backend: &dyn Backend, ws: &Workspace, message: Option<String>) -> Result<()> {
    let (store, state, op) = open(ws)?;
    if op.paused {
        return resume(backend, ws, &store, &state, &op);
    }
    let index = Index::load(ws)?;
    merging::check_resolved(&state, &index)?;
    let exclusive: Vec<&String> = op
        .sides
        .iter()
        .filter(|(p, s)| s.exclusive && state.paths.contains_key(*p))
        .map(|(p, _)| p)
        .collect();
    if !exclusive.is_empty() {
        let mine = my_locks(backend)?;
        let missing: Vec<String> = exclusive
            .iter()
            .filter(|p| !mine.contains(**p))
            .map(|p| format!("\t`pyn lock {p}`"))
            .collect();
        if !missing.is_empty() {
            bail!(
                "an exclusive file that both sides changed needs your lock to be concluded; take it with:\n{}\nthen commit again",
                missing.join("\n")
            );
        }
    }
    let mut files = flat_tree(&store, &id_of(&op.result_tree)?)?;
    for (path, staged) in &index.entries {
        if staged.deleted {
            files.remove(path.as_bytes());
            continue;
        }
        let bytes = std::fs::read(staging::content_path(ws, &staged.hash))
            .with_context(|| format!("reading the staged content of {path}"))?;
        let id = store.repo().objects().write(Kind::Blob, &bytes)?;
        files.insert(
            path.clone().into_bytes(),
            Entry {
                mode: file_mode(staged.file_mode),
                id,
            },
        );
    }
    let tree = write_trees(&store, &files)?;
    let who = signature(&backend.whoami()?);
    let (orig, theirs) = (id_of(&op.orig)?, id_of(&op.theirs)?);
    let repo = store.repo();
    match op.op {
        Op::Merge => {
            let record = Commit {
                tree,
                parents: vec![orig, theirs],
                author: who.clone(),
                committer: who,
                extra_headers: Vec::new(),
                message: clean_message(&[message.unwrap_or(state.message)]),
            };
            let tip = repo
                .objects()
                .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
            repo.refs()
                .update_checked(&local_ref(&state.branch), Expect::Id(orig), &tip)
                .context("the local branch moved while pulling; run the command again")?;
            adopt(ws, &index)?;
            Index::default().save(ws)?;
            merging::clear(ws)?;
            println!("Merge made by the 'ort' strategy.");
            sync::update(backend, ws, &[], None)
        }
        Op::Rebase | Op::Pick | Op::Revert | Op::Rewrite => {
            let item = parse_move(
                op.current
                    .as_deref()
                    .context("the rebase has no current commit")?,
            )?;
            let cmd = item.cmd;
            let current = item.id.context("the rebase has no current commit")?;
            let picked = repo.commit(&current)?;
            let tip = id_of(op.tip.as_deref().context("the rebase has no tip")?)?;
            let mut run = run_of(&op, tip)?;
            if cmd.melds() {
                let head = repo.commit(&tip)?;
                let message = match message {
                    Some(m) => clean_message(&[m]),
                    None => {
                        let text = std::fs::read_to_string(meld_dir(ws).join(meld::SQUASH))
                            .context("the squash chain has no collected message")?;
                        melded_message(ws, more_melds(&run), &text)?
                    }
                };
                let record = Commit {
                    tree,
                    parents: head.parents,
                    author: head.author,
                    committer: who.clone(),
                    extra_headers: Vec::new(),
                    message,
                };
                run.tip = repo
                    .objects()
                    .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
                if !more_melds(&run) {
                    let _ = std::fs::remove_dir_all(meld_dir(ws));
                }
            } else if tree == repo.commit(&tip)?.tree {
                run.dropped += 1;
            } else {
                let message = match message {
                    Some(m) => clean_message(&[m]),
                    None if cmd == Cmd::Reword => {
                        crate::local_rebase::edit_message(ws, &picked.message)?
                    }
                    None => replayed_message(op.op, op.origin, &current, picked.message),
                };
                let record = Commit {
                    tree,
                    parents: vec![tip],
                    author: replayed_author(op.op, &who, picked.author),
                    committer: who.clone(),
                    extra_headers: Vec::new(),
                    message,
                };
                run.tip = repo
                    .objects()
                    .write(Kind::Commit, &record.encode(ALGORITHM)?)?;
                if op.op.announces() {
                    print_pick(&state.branch, &run.tip, &record.message);
                }
            }
            adopt(ws, &index)?;
            let mut start = Resume::new(&files);
            start.halt = cmd == Cmd::Edit;
            proceed(backend, ws, &store, (&state.branch, &who), run, start)
        }
    }
}

/// The run a record describes, continued from `tip`.
fn run_of(op: &LocalOp, tip: ObjectId) -> Result<Run> {
    let todo: VecDeque<Move> = op
        .todo
        .iter()
        .map(|t| parse_move(t))
        .collect::<Result<_>>()?;
    Ok(Run {
        op: op.op,
        origin: op.origin,
        orig: id_of(&op.orig)?,
        onto: id_of(&op.theirs)?,
        tip,
        todo,
        total: op.total,
        dropped: op.dropped,
        no_ff: op.no_ff,
        stash: op.autostash.clone(),
    })
}

/// Puts every uncommitted change back to what the tip holds.
fn discard_changes(
    store: &LocalStore,
    backend: &dyn Backend,
    ws: &Workspace,
    tip: ObjectId,
) -> Result<Files> {
    let files = flat(store, Some(tip))?;
    let dirty: BTreeSet<String> = dirty_paths(ws)?.into_iter().collect();
    materialize(
        store,
        backend,
        ws,
        (&files, &files),
        &BTreeMap::new(),
        (&dirty, &BTreeSet::new()),
    )?;
    Index::default().save(ws)?;
    Ok(files)
}

/// `--continue` on a stop at `edit`, `break` or `exec`: the commits made since are kept and the rest is replayed.
fn resume(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    state: &MergeState,
    op: &LocalOp,
) -> Result<()> {
    if !Index::load(ws)?.entries.is_empty() {
        bail!(
            "you have staged changes in your working tree; commit them with `pyn commit`, or take them out with `pyn restore --staged`, then run `pyn rebase --continue` again"
        );
    }
    if !dirty_paths(ws)?.is_empty() {
        bail!(
            "you have uncommitted changes; stage and commit them with `pyn add` and `pyn commit`, or put them back with `pyn checkout -- <path>`, then run `pyn rebase --continue` again"
        );
    }
    let tip = store
        .tip(&state.branch)?
        .context("the rebase has no branch tip")?;
    let disk = flat(store, Some(tip))?;
    let who = signature(&backend.whoami()?);
    proceed(
        backend,
        ws,
        store,
        (&state.branch, &who),
        run_of(op, tip)?,
        Resume::new(&disk),
    )
}

/// Drops the commit a rebase stopped on and goes on with the next; at an `edit`, `break` or `exec` stop it only
/// puts the uncommitted changes back.
pub fn skip(backend: &dyn Backend, ws: &Workspace) -> Result<()> {
    let (store, state, op) = open(ws)?;
    if op.op == Op::Merge {
        bail!("`--skip` drops a commit of a rebase or cherry-pick; there is none in progress");
    }
    let who = signature(&backend.whoami()?);
    if op.paused {
        let tip = store
            .tip(&state.branch)?
            .context("the rebase has no branch tip")?;
        let back = discard_changes(&store, backend, ws, tip)?;
        return proceed(
            backend,
            ws,
            &store,
            (&state.branch, &who),
            run_of(&op, tip)?,
            Resume::new(&back),
        );
    }
    let tip = id_of(op.tip.as_deref().context("the rebase has no tip")?)?;
    if let Some(line) = op.current.as_deref()
        && parse_move(line)?.cmd.melds()
    {
        meld::drop_last(&meld_dir(ws));
    }
    let disk = flat_tree(&store, &id_of(&op.result_tree)?)?;
    let back = flat(&store, Some(tip))?;
    let touched: BTreeSet<String> = state.paths.keys().cloned().collect();
    materialize(
        &store,
        backend,
        ws,
        (&disk, &back),
        &BTreeMap::new(),
        (&touched, &BTreeSet::new()),
    )?;
    let mut index = Index::load(ws)?;
    index.entries.retain(|p, _| !touched.contains(p));
    index.save(ws)?;
    let mut run = run_of(&op, tip)?;
    run.dropped += 1;
    proceed(
        backend,
        ws,
        &store,
        (&state.branch, &who),
        run,
        Resume::new(&back),
    )
}

/// Gives the pull up: the working tree is what it was, and nothing in the local store or on the server changed.
pub fn abort(backend: &dyn Backend, ws: &Workspace) -> Result<()> {
    let (store, state, op) = open(ws)?;
    let orig = id_of(&op.orig)?;
    let at = store.tip(&state.branch)?;
    let back = flat(&store, Some(orig))?;
    let mut touched: BTreeSet<String> = state.paths.keys().cloned().collect();
    let disk = if op.paused {
        touched.extend(dirty_paths(ws)?);
        flat(&store, at)?
    } else {
        flat_tree(&store, &id_of(&op.result_tree)?)?
    };
    materialize(
        &store,
        backend,
        ws,
        (&disk, &back),
        &BTreeMap::new(),
        (&touched, &BTreeSet::new()),
    )?;
    let mut index = Index::load(ws)?;
    if op.paused {
        index = Index::default();
    } else {
        index.entries.retain(|p, _| !touched.contains(p));
    }
    index.save(ws)?;
    if op.op == Op::Rewrite
        && let Some(now) = at.filter(|now| *now != orig)
    {
        store.move_tip(&state.branch, now, orig)?;
    }
    merging::clear(ws)?;
    let _ = std::fs::remove_dir_all(meld_dir(ws));
    println!(
        "The {} was given up; your branch is back at {}",
        op.name(),
        short(&op.orig)
    );
    unstash(
        backend,
        ws,
        &store,
        &op.autostash,
        (orig, id_of(&op.theirs)?),
    )
}

/// Forgets the rebase in progress: the branch stays where the replay left it and the files are not touched.
pub fn quit(backend: &dyn Backend, ws: &Workspace) -> Result<()> {
    let (store, state, op) = open(ws)?;
    let at = store
        .tip(&state.branch)?
        .context("the rebase has no branch tip")?;
    merging::clear(ws)?;
    let _ = std::fs::remove_dir_all(meld_dir(ws));
    println!(
        "The rebase was forgotten; branch '{}' stays at {} (it was at {} before the rebase)",
        state.branch,
        short(&at.to_hex()),
        short(&op.orig)
    );
    unstash(backend, ws, &store, &op.autostash, (at, id_of(&op.theirs)?))
}

/// Lets `edit` change the commands still to do; the numbering follows the new count.
pub fn edit_todo(
    ws: &Workspace,
    edit: impl FnOnce(&str, &[Move]) -> Result<Vec<Move>>,
) -> Result<()> {
    let (_, mut state, mut op) = open(ws)?;
    let todo: Vec<Move> = op
        .todo
        .iter()
        .map(|t| parse_move(t))
        .collect::<Result<_>>()?;
    let edited = edit(&state.branch, &todo)?;
    op.total = op.total - op.todo.len() + edited.len();
    op.todo = edited.iter().map(move_line).collect();
    state.local = Some(op);
    merging::save(ws, &state)
}

/// Puts the changed files of the working tree aside in the local store and restores them to what `tip` holds.
pub fn stash_dirty(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    tip: ObjectId,
) -> Result<BTreeMap<String, Stashed>> {
    let files = flat(store, Some(tip))?;
    let mut from = files.clone();
    let mut stash = BTreeMap::new();
    for path in dirty_paths(ws)? {
        let key = path.clone().into_bytes();
        let Some(base) = files.get(&key).copied() else {
            continue;
        };
        let (mode, bytes) = localfile::read(&ws.abs(&path))?;
        let id = store.repo().objects().write(Kind::Blob, &bytes)?;
        from.insert(
            key,
            Entry {
                mode: file_mode(mode),
                id,
            },
        );
        let base = side(Some(base))?.expect("a base entry gives a side");
        let stash_side = Side {
            id: id.to_hex(),
            mode,
        };
        stash.insert(
            path,
            Stashed {
                base,
                stash: stash_side,
            },
        );
    }
    if !stash.is_empty() {
        let force: BTreeSet<String> = stash.keys().cloned().collect();
        materialize(
            store,
            backend,
            ws,
            (&from, &files),
            &BTreeMap::new(),
            (&force, &BTreeSet::new()),
        )?;
        eprintln!("Created autostash: {} file(s) put aside.", stash.len());
    }
    Ok(stash)
}

fn entry_of(side: &Side) -> Result<Entry> {
    Ok(Entry {
        mode: file_mode(side.mode),
        id: id_of(&side.id)?,
    })
}

/// Puts the autostashed changes back over `tip`, merging them with what the rebase changed; a conflict leaves
/// markers in the file. `onto` is the commit whose modes the server is asked about.
pub fn unstash(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    stash: &BTreeMap<String, Stashed>,
    (tip, onto): (ObjectId, ObjectId),
) -> Result<()> {
    if stash.is_empty() {
        return Ok(());
    }
    let ours_files = flat(store, Some(tip))?;
    let mut judge = Judge::new(backend, onto);
    let mut to = ours_files.clone();
    let mut working = BTreeMap::new();
    let mut conflicted = Vec::new();
    for (path, stashed) in stash {
        let key = path.clone().into_bytes();
        let (base, theirs) = (entry_of(&stashed.base)?, entry_of(&stashed.stash)?);
        let ours = ours_files.get(&key).copied();
        let step = store.join_path(
            backend,
            &mut judge,
            &key,
            (Some(base), ours, Some(theirs)),
            true,
            ("HEAD", "autostash"),
        )?;
        let (entry, bytes) = match step {
            Step::Take(Some(entry)) | Step::Kept(Some(entry)) => {
                let bytes = store.blob(backend, &entry.id.to_hex())?;
                (entry, bytes)
            }
            Step::Conflict(c) => {
                conflicted.push(path.as_str());
                let bytes = match c.working {
                    Some(marked) => marked,
                    None => store.blob(backend, &theirs.id.to_hex())?,
                };
                (theirs, bytes)
            }
            Step::Take(None) | Step::Kept(None) => {
                (theirs, store.blob(backend, &theirs.id.to_hex())?)
            }
        };
        to.insert(key.clone(), entry);
        working.insert(key, bytes);
    }
    let force: BTreeSet<String> = stash.keys().cloned().collect();
    materialize(
        store,
        backend,
        ws,
        (&ours_files, &to),
        &working,
        (&force, &BTreeSet::new()),
    )?;
    if conflicted.is_empty() {
        eprintln!("Applied autostash.");
    } else {
        eprintln!(
            "Applying autostash resulted in conflicts.\nYour changes are in the working tree, with conflict markers in: {}",
            conflicted.join(", ")
        );
    }
    Ok(())
}

/// Takes one side of a conflicted path into the working tree, as `pyn checkout --ours|--theirs -- <path>`; in a
/// rebase `--ours` is the fetched side and `--theirs` the commit being replayed.
pub fn take_side(
    backend: &dyn Backend,
    ws: &Workspace,
    theirs: bool,
    paths: &[String],
) -> Result<()> {
    let (store, state, op) = open(ws)?;
    for path in paths {
        if !state.paths.get(path).is_some_and(|t| t.conflict.is_some()) {
            bail!("{path} is not unmerged");
        }
        let sides = op
            .sides
            .get(path)
            .context("the pull recorded no sides of this path")?;
        let Some(side) = (if theirs { &sides.theirs } else { &sides.ours }) else {
            bail!("{path} does not exist on that side of the merge");
        };
        let bytes = store.blob(backend, &side.id)?;
        let abs = ws.abs(path);
        set_read_only(&abs, false)?;
        ws.write_file(path, &bytes)?;
        localfile::apply(&abs, path, side.mode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_round_trips_through_toml() {
        let mut sides = BTreeMap::new();
        sides.insert(
            "Content/m.umap".to_string(),
            Sides {
                ours: Some(Side {
                    id: "ab".repeat(32),
                    mode: api::FileMode::Regular,
                }),
                theirs: None,
                exclusive: true,
            },
        );
        let op = LocalOp {
            op: Op::Rebase,
            orig: "cd".repeat(32),
            theirs: "ef".repeat(32),
            result_tree: "01".repeat(32),
            sides,
            tip: Some("23".repeat(32)),
            current: Some("45".repeat(32)),
            todo: vec!["67".repeat(32)],
            total: 3,
            dropped: 1,
            origin: false,
            paused: false,
            no_ff: false,
            autostash: BTreeMap::new(),
        };
        let state = MergeState {
            version: STATE_VERSION,
            branch: "main".into(),
            source: "origin/main".into(),
            source_commit: op.theirs.clone(),
            target_commit: op.orig.clone(),
            message: "m".into(),
            no_ff: false,
            paths: BTreeMap::new(),
            local: Some(op.clone()),
        };
        let back: MergeState = toml::from_str(&toml::to_string_pretty(&state).unwrap()).unwrap();
        assert_eq!(back.local, Some(op));
    }

    #[test]
    fn a_step_is_a_bare_id_for_a_pick_and_a_word_and_id_otherwise() {
        let id = ObjectId::from_hex(pyn_object::Algorithm::Sha256, &"ab".repeat(32)).unwrap();
        for cmd in [Cmd::Pick, Cmd::Reword, Cmd::Squash, Cmd::Fixup, Cmd::Edit] {
            let item = Item {
                cmd,
                ..Item::pick(id)
            };
            let back = parse_move(&move_line(&item)).unwrap();
            assert_eq!((back.cmd, back.id), (cmd, Some(id)));
        }
        assert_eq!(move_line(&Item::pick(id)), id.to_hex());
        let exec = parse_move("exec make -j4 && echo done").unwrap();
        assert_eq!(
            (exec.cmd, exec.arg.as_str()),
            (Cmd::Exec, "make -j4 && echo done")
        );
        assert_eq!(move_line(&exec), "exec make -j4 && echo done");
        assert_eq!(parse_move("break").unwrap().cmd, Cmd::Break);
        assert!(parse_move("").is_err());
        assert!(parse_move("exec").is_err());
        assert!(parse_move(&format!("nope {id}")).is_err());
    }

    #[test]
    fn the_header_counts_the_commits_of_a_rebase() {
        let op = LocalOp {
            op: Op::Rebase,
            orig: String::new(),
            theirs: String::new(),
            result_tree: String::new(),
            sides: BTreeMap::new(),
            tip: None,
            current: None,
            todo: vec!["a".into()],
            total: 3,
            dropped: 0,
            origin: false,
            paused: false,
            no_ff: false,
            autostash: BTreeMap::new(),
        };
        assert!(op.header().contains("commit 2 of 3, 1 left"));
    }
}

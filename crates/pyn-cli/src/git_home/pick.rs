//! `pyn cherry-pick` and `pyn revert` on a git repository: each commit's change (or its inverse) is merged onto the
//! index and becomes a new commit (or stays staged with `-n`). A stop leaves `CHERRY_PICK_HEAD` or `REVERT_HEAD`,
//! `MERGE_MSG` and, for several commits, `.git/sequencer` as git writes them, so either tool can continue, skip or abort.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use pyn_gitwork::{Head, Pathspec};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId};

use super::GitHome;
use super::commit::append_signoff;
use super::hooks::{CommitHooks, HookRefused, author_env};
use super::merge::{Applied, Landing, apply_merge, write_state};
use super::signing::{SignFlag, sign_commit};
use super::stage::{
    clean_message, clear_merge_state, identity, merge_message, refuse_states, write_tree,
};
use super::strategy::{Replay, Settings};
use super::treemerge::{Labels, merge_trees};
use super::{convert, path_string, reset};
use crate::local_pick::{PickArgs, revert_message, select, with_origin};

const ABBREV: usize = 7;
const PICK_HEAD: &str = "CHERRY_PICK_HEAD";
const REVERT_HEAD: &str = "REVERT_HEAD";
const SEQUENCER: &str = "sequencer";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Verb {
    Pick,
    Revert,
}

impl Verb {
    fn error_word(self) -> &'static str {
        match self {
            Verb::Pick => "apply",
            Verb::Revert => "revert",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Verb::Pick => "pick",
            Verb::Revert => "revert",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Verb::Pick => "cherry-pick",
            Verb::Revert => "revert",
        }
    }

    fn head_file(self) -> &'static str {
        match self {
            Verb::Pick => PICK_HEAD,
            Verb::Revert => REVERT_HEAD,
        }
    }
}

type Todo = Vec<(Verb, ObjectId)>;

/// The options that outlive one command, kept in `sequencer/opts`.
#[derive(Default, Clone)]
struct Opts {
    no_commit: bool,
    record_origin: bool,
    signoff: bool,
    mainline: Option<u32>,
    sign: SignFlag,
    replay: Replay,
}

impl Opts {
    fn of(args: &PickArgs, replay: Replay) -> Self {
        Self {
            replay,
            no_commit: args.no_commit,
            record_origin: args.record_origin,
            signoff: args.signoff,
            mainline: args.mainline,
            sign: args.sign.clone(),
        }
    }

    fn save(&self) -> Option<String> {
        let mut text = String::new();
        for (key, on) in [
            ("no-commit", self.no_commit),
            ("record-origin", self.record_origin),
            ("signoff", self.signoff),
        ] {
            if on {
                text.push_str(&format!("\t{key} = true\n"));
            }
        }
        if let Some(n) = self.mainline {
            text.push_str(&format!("\tmainline = {n}\n"));
        }
        if let Some(key) = &self.sign.gpg_sign {
            text.push_str(&format!(
                "\tgpg-sign = {}\n",
                key.as_deref().unwrap_or_default()
            ));
        }
        if self.sign.no_gpg_sign {
            text.push_str("\tno-gpg-sign = true\n");
        }
        if let Some(name) = &self.replay.name {
            text.push_str(&format!("\tstrategy = {name}\n"));
        }
        for opt in &self.replay.xopts {
            text.push_str(&format!("\tstrategy-option = {opt}\n"));
        }
        (!text.is_empty()).then(|| format!("[options]\n{text}"))
    }

    fn load(text: &str) -> Self {
        let mut opts = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "no-commit" => opts.no_commit = value == "true",
                "record-origin" => opts.record_origin = value == "true",
                "signoff" => opts.signoff = value == "true",
                "mainline" => opts.mainline = value.parse().ok(),
                "gpg-sign" => {
                    opts.sign.gpg_sign = Some((!value.is_empty()).then(|| value.to_string()))
                }
                "no-gpg-sign" => opts.sign.no_gpg_sign = value == "true",
                "strategy" => opts.replay.name = Some(value.to_string()),
                "strategy-option" => opts.replay.xopts.push(value.to_string()),
                _ => {}
            }
        }
        opts
    }
}

enum Outcome {
    /// A commit was made; the branch is now at this id.
    Committed(ObjectId),
    /// `-n`: the changes are in the index and work tree.
    Applied,
    /// Conflicts, or an empty result: the state is written and the command ends with status 1.
    Stopped,
}

pub(super) fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

pub(super) fn subject_of(message: &[u8]) -> String {
    String::from_utf8_lossy(message)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

pub(super) fn git_dir(git: &GitHome) -> PathBuf {
    git.repo.git().git_dir().to_path_buf()
}

pub(super) fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

pub(super) fn read_id(git: &GitHome, path: &Path) -> Result<Option<ObjectId>> {
    let Some(text) = read_text(path) else {
        return Ok(None);
    };
    let hex = text.lines().next().unwrap_or_default().trim();
    Ok(Some(ObjectId::from_hex(git.repo.algorithm(), hex)?))
}

pub(super) fn fail(lines: &[&str]) -> anyhow::Error {
    for line in lines {
        eprintln!("{line}");
    }
    anyhow::Error::new(Failed)
}

/// A refusal whose reasons were already printed; the command names itself when it reports it.
#[derive(Debug)]
pub(super) struct Failed;

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("failed")
    }
}

impl std::error::Error for Failed {}

pub fn cherry_pick(git: &GitHome, args: &PickArgs) -> Result<()> {
    control_or_start(git, Verb::Pick, args).map_err(|e| named(e, Verb::Pick))
}

pub fn revert(git: &GitHome, args: &PickArgs) -> Result<()> {
    control_or_start(git, Verb::Revert, args).map_err(|e| named(e, Verb::Revert))
}

fn named(e: anyhow::Error, verb: Verb) -> anyhow::Error {
    if e.is::<Failed>() {
        anyhow!("{} failed", verb.command())
    } else {
        e
    }
}

fn control_or_start(git: &GitHome, verb: Verb, args: &PickArgs) -> Result<()> {
    if args.quit {
        return quit(git);
    }
    if args.abort {
        return abort(git);
    }
    if args.skip {
        return skip(git);
    }
    if args.continue_pick {
        return continue_pick(git);
    }
    start(git, verb, args)
}

fn in_progress(git: &GitHome) -> bool {
    let dir = git_dir(git);
    dir.join(PICK_HEAD).exists() || dir.join(REVERT_HEAD).exists() || dir.join(SEQUENCER).exists()
}

fn start(git: &GitHome, verb: Verb, args: &PickArgs) -> Result<()> {
    let cmd = verb.command();
    let replay = Replay::parse(cmd, args.strategy.as_deref(), &args.strategy_option);
    let opts = Opts::of(args, replay);
    refuse_states(git, &["MERGE_HEAD", "rebase-merge", "rebase-apply"])?;
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.skip_worktree) {
        bail!("this repository uses sparse checkout, which pyn does not support yet");
    }
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        return Err(fail(&[
            &format!(
                "error: {} is not possible because you have unmerged files.",
                if verb == Verb::Pick {
                    "Cherry-picking"
                } else {
                    "Reverting"
                }
            ),
            "hint: Fix them up in the work tree, and then use 'pyn add/rm <file>'",
            "hint: as appropriate to mark resolution and make a commit.",
        ]));
    }
    let head = git.repo.head()?;
    let picks = select(
        &git.repo,
        &|s| {
            git.repo
                .resolve(s)
                .map_err(|_| anyhow!("bad revision '{s}'"))
        },
        "HEAD",
        &args.commits,
        verb == Verb::Revert,
    )?;
    if picks.is_empty() {
        return Err(fail(&["error: empty commit set passed"]));
    }
    for id in &picks {
        parent_of(git, id, opts.mainline)?;
    }
    let picks: Todo = picks.into_iter().map(|id| (verb, id)).collect();
    if !opts.no_commit {
        let tree = match head.id() {
            Some(h) => Some(git.repo.commit(&h)?.tree),
            None => None,
        };
        let committed = git.repo.flatten(tree.as_ref(), &Pathspec::default())?;
        if git.repo.index_leaves(&opened.index) != committed {
            return Err(fail(&[
                &format!("error: your local changes would be overwritten by {cmd}."),
                "hint: commit your changes or stash them to proceed.",
            ]));
        }
    }
    let single = args.commits.len() == 1 && !args.commits[0].contains("..");
    if !single && in_progress(git) {
        return Err(fail(&[
            "error: a cherry-pick or revert is already in progress",
            &format!("hint: try \"pyn {cmd} (--continue | --skip | --abort | --quit)\""),
        ]));
    }
    if !single && head.id().is_none() {
        bail!("an unborn branch takes one commit at a time; pick the first, then the rest");
    }
    if !single {
        let seq = git_dir(git).join(SEQUENCER);
        std::fs::create_dir_all(&seq)?;
        if let Some(h) = head.id() {
            write_state(git, "sequencer/head", &format!("{h}\n"))?;
            write_state(git, "sequencer/abort-safety", &format!("{h}\n"))?;
        }
        if let Some(text) = opts.save() {
            write_state(git, "sequencer/opts", &text)?;
        }
    }
    run(git, &opts, &picks, !single)
}

/// The parent a commit is picked relative to: its only one, or the one `-m` names for a merge.
pub(super) fn parent_of(
    git: &GitHome,
    id: &ObjectId,
    mainline: Option<u32>,
) -> Result<Option<ObjectId>> {
    let parents = git.repo.commit(id)?.parents;
    match (parents.len(), mainline) {
        (0, None) => Ok(None),
        (1, None) => Ok(Some(parents[0])),
        (0 | 1, Some(1)) => Ok(parents.first().copied()),
        (_, None) => Err(fail(&[&format!(
            "error: commit {id} is a merge but no -m option was given."
        )])),
        (n, Some(m)) if m as usize > n => Err(fail(&[&format!(
            "error: commit {id} does not have parent {m}"
        )])),
        (_, Some(m)) => Ok(Some(parents[m as usize - 1])),
    }
}

fn write_todo(git: &GitHome, picks: &[(Verb, ObjectId)]) -> Result<()> {
    let mut text = String::new();
    for (verb, id) in picks {
        text.push_str(&format!(
            "{} {id} {}\n",
            verb.word(),
            subject_of(&git.repo.commit(id)?.message)
        ));
    }
    write_state(git, "sequencer/todo", &text)
}

/// Applies `picks` in order. With `sequenced` the todo list is kept current for `--continue`, `--skip` and `--abort`.
fn run(git: &GitHome, opts: &Opts, picks: &[(Verb, ObjectId)], sequenced: bool) -> Result<()> {
    for (i, (verb, id)) in picks.iter().enumerate() {
        if sequenced {
            write_todo(git, &picks[i..])?;
        }
        let outcome = match apply(git, opts, *verb, id) {
            Ok(outcome) => outcome,
            Err(e) => {
                if sequenced && i == 0 && !e.is::<HookRefused>() {
                    let _ = std::fs::remove_dir_all(git_dir(git).join(SEQUENCER));
                }
                return Err(e);
            }
        };
        match outcome {
            Outcome::Committed(head) if sequenced => {
                write_state(git, "sequencer/abort-safety", &format!("{head}\n"))?;
            }
            Outcome::Stopped => std::process::exit(1),
            _ => {}
        }
    }
    if sequenced {
        let _ = std::fs::remove_dir_all(git_dir(git).join(SEQUENCER));
    }
    Ok(())
}

/// A commit's change merged onto `HEAD`: the index and work tree are written, conflicts left as unmerged entries.
pub(super) struct Merged {
    pub picked: Commit,
    pub parent: Option<ObjectId>,
    pub label: String,
    pub ours_tree: ObjectId,
    pub applied: Applied,
}

pub(super) fn merge_pick(
    git: &GitHome,
    verb: Verb,
    id: &ObjectId,
    mainline: Option<u32>,
    settings: &Settings,
    quiet_when_clean: bool,
) -> Result<Merged> {
    let picked = git.repo.commit(id)?;
    let parent = parent_of(git, id, mainline)?;
    let parent_tree = match &parent {
        Some(p) => Some(git.repo.commit(p)?.tree),
        None => None,
    };
    let opened = git.open_index()?;
    let ours_tree = write_tree(git, &opened.index)?;
    let label = format!("{} ({})", short(id), subject_of(&picked.message));
    // A revert merges with the commit as the base and its parent as the incoming side.
    let (base_tree, theirs_tree) = match verb {
        Verb::Pick => (parent_tree, Some(picked.tree)),
        Verb::Revert => (Some(picked.tree), parent_tree),
    };
    let (theirs_label, base_label) = match (verb, &parent) {
        (Verb::Pick, Some(_)) => (label.clone(), format!("parent of {label}")),
        (Verb::Pick, None) => (label.clone(), "(empty tree)".to_string()),
        (Verb::Revert, _) => (format!("parent of {label}"), label.clone()),
    };
    let incoming = git.repo.diff_trees(
        base_tree.as_ref(),
        theirs_tree.as_ref(),
        &Pathspec::default(),
    )?;
    convert::refuse_unsupported(
        git,
        incoming
            .iter()
            .filter_map(|c| c.new.map(|e| (c.path.as_slice(), e.mode))),
    )?;
    let merged = merge_trees(
        git,
        base_tree.as_ref(),
        Some(&ours_tree),
        theirs_tree.as_ref(),
        &Labels {
            ours: "HEAD",
            theirs: &theirs_label,
            base: &base_label,
        },
        settings,
    )?;
    let landing = Landing {
        theirs: &theirs_label,
        orig_head: None,
        strict_index: false,
        quiet_when_clean,
    };
    let applied = match apply_merge(git, &opened, Some(&ours_tree), &merged, &landing)? {
        Ok(applied) => applied,
        Err(refusal) => {
            refusal.blocked.print("merge", "merge");
            return Err(fail(&["Aborting"]));
        }
    };
    Ok(Merged {
        picked,
        parent,
        label,
        ours_tree,
        applied,
    })
}

fn apply(git: &GitHome, opts: &Opts, verb: Verb, id: &ObjectId) -> Result<Outcome> {
    let Merged {
        picked,
        parent,
        label,
        ours_tree,
        applied,
    } = merge_pick(git, verb, id, opts.mainline, &opts.replay.settings(), false)?;
    let committer = identity(git, "committer")?;
    let mut message = match verb {
        Verb::Pick => picked.message.clone(),
        Verb::Revert => revert_message(
            &picked.message,
            id,
            parent.as_ref().filter(|_| picked.parents.len() > 1),
        ),
    };
    if opts.record_origin && verb == Verb::Pick {
        message = with_origin(message, id);
    }
    if opts.signoff {
        message = append_signoff(message, &committer.name, &committer.email);
    }
    let author = match verb {
        Verb::Pick => picked.author.clone(),
        Verb::Revert => identity(git, "author")?,
    };
    if !applied.conflicted.is_empty() {
        let mut text = message.clone();
        if !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        text.extend_from_slice(b"\n# Conflicts:\n");
        for p in &applied.conflicted {
            text.extend_from_slice(format!("#\t{}\n", path_string(p)).as_bytes());
        }
        write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&text))?;
        if opts.no_commit {
            if verb == Verb::Revert {
                write_state(git, REVERT_HEAD, &format!("{id}\n"))?;
            }
            eprintln!("error: could not {} {label}", verb.error_word());
            eprintln!("hint: after resolving the conflicts, mark the corrected paths");
            eprintln!("hint: with 'pyn add <paths>' or 'pyn rm <paths>'");
        } else {
            write_state(git, verb.head_file(), &format!("{id}\n"))?;
            let cmd = verb.command();
            eprintln!("error: could not {} {label}", verb.error_word());
            eprintln!("hint: After resolving the conflicts, mark them with");
            eprintln!("hint: \"pyn add/rm <pathspec>\", then run");
            eprintln!("hint: \"pyn {cmd} --continue\".");
            eprintln!("hint: You can instead skip this commit with \"pyn {cmd} --skip\".");
            eprintln!("hint: To abort and get back to the state before \"pyn {cmd}\",");
            eprintln!("hint: run \"pyn {cmd} --abort\".");
        }
        return Ok(Outcome::Stopped);
    }
    if opts.no_commit {
        if verb == Verb::Revert {
            write_state(git, REVERT_HEAD, &format!("{id}\n"))?;
        }
        write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&message))?;
        return Ok(Outcome::Applied);
    }
    let tree = write_tree(git, &applied.index)?;
    if tree == ours_tree {
        if verb == Verb::Pick {
            write_state(git, PICK_HEAD, &format!("{id}\n"))?;
        }
        write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&message))?;
        say_empty(git, verb)?;
        return Ok(Outcome::Stopped);
    }
    let pending = Pending {
        tree,
        ours_tree,
        author,
        message,
        reflog: verb.command().to_string(),
        sign: opts.sign.clone(),
    };
    let made = commit_pick(git, &pending, false).inspect_err(|e| {
        // Like git, a commit that cannot be made leaves the pick stopped, so `--continue` can retry it; a revert
        // stopped by a hook leaves no `REVERT_HEAD`.
        if verb == Verb::Pick || !e.is::<HookRefused>() {
            let _ = write_state(git, verb.head_file(), &format!("{id}\n"));
        }
        let _ = write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&pending.message));
    })?;
    Ok(Outcome::Committed(made))
}

fn say_empty(git: &GitHome, verb: Verb) -> Result<()> {
    git.repo.validate_diff_ignore_submodules()?;
    let cmd = verb.command();
    super::commands::status_table(&git.repo.status()?)?;
    eprintln!("The previous {cmd} is now empty, possibly due to conflict resolution.");
    eprintln!("pyn does not keep empty commits; drop it with 'pyn {cmd} --skip'.");
    Ok(())
}

#[derive(Clone)]
pub(super) struct Pending {
    pub(super) tree: ObjectId,
    /// The tree of `HEAD`, which the new commit's changes are counted against.
    pub(super) ours_tree: ObjectId,
    pub(super) author: pyn_object::Signature,
    pub(super) message: Vec<u8>,
    /// The reflog action: git words a commit made by `--continue` differently from one made directly.
    pub(super) reflog: String,
    pub(super) sign: SignFlag,
}

/// Writes the commit and moves the branch (or a detached `HEAD`) to it, logging the move as `pending.reflog`.
pub(super) fn make_commit(git: &GitHome, pending: &Pending) -> Result<ObjectId> {
    let parents = git.repo.head()?.id().into_iter().collect();
    make_commit_on(git, pending, parents)
}

/// Like [`make_commit`] with the parents given, so a commit can replace the one `HEAD` is on.
pub(super) fn make_commit_on(
    git: &GitHome,
    pending: &Pending,
    parents: Vec<ObjectId>,
) -> Result<ObjectId> {
    let committer = identity(git, "committer")?;
    let mut record = Commit {
        tree: pending.tree,
        parents,
        author: pending.author.clone(),
        committer: committer.clone(),
        extra_headers: Vec::new(),
        message: pending.message.clone(),
    };
    sign_commit(git, &pending.sign, &mut record)?;
    let id = git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode(git.algorithm())?)?;
    let first = subject_of(&pending.message);
    git.move_head(&id, &format!("{}: {first}", pending.reflog))
        .map_err(|e| e.context("HEAD moved while committing; run the command again"))?;
    Ok(id)
}

/// Like [`make_commit_on`] with `hooks` around it: `prepare-commit-msg` over the message unless the editor stage ran
/// it, then `post-commit`, and `post-rewrite amend` when the commit replaces `replaces`.
pub(super) fn make_commit_hooked(
    git: &GitHome,
    pending: &Pending,
    parents: Vec<ObjectId>,
    replaces: Option<ObjectId>,
    hooks: &CommitHooks,
) -> Result<ObjectId> {
    let mut pending = pending.clone();
    if !hooks.message_done() {
        pending.message = git.prepare_message(&pending.message, &["message"], hooks.env())?;
    }
    let id = make_commit_on(git, &pending, parents)?;
    git.after_commit(hooks, replaces.as_ref().map(|old| (old, &id)))?;
    Ok(id)
}

/// Makes the commit and reports it as `git commit` does; `resumed` is a commit made by `--continue`, which verifies.
fn commit_pick(git: &GitHome, pending: &Pending, resumed: bool) -> Result<ObjectId> {
    let head = git.repo.head()?;
    let committer = identity(git, "committer")?;
    let id = if resumed {
        let mut env = author_env(&pending.author);
        env.extend(CommitHooks::direct(git).env().iter().cloned());
        let mut made = pending.clone();
        made.message = git.verify_message(&pending.message, "merge", &env)?;
        let parents = git.repo.head()?.id().into_iter().collect();
        let id = make_commit_on(git, &made, parents)?;
        git.run_hook("post-commit", &[], None, &env)?;
        id
    } else {
        let parents = git.repo.head()?.id().into_iter().collect();
        make_commit_hooked(git, pending, parents, None, &CommitHooks::direct(git))?
    };
    let first = subject_of(&pending.message);
    let place = match &head {
        Head::Branch { name, id: None } => format!("{name} (root-commit)"),
        Head::Branch { name, .. } => name.clone(),
        Head::Detached(_) => "detached HEAD".into(),
    };
    println!("[{place} {}] {first}", short(&id));
    if (
        pending.author.name.as_slice(),
        pending.author.email.as_slice(),
    ) != (committer.name.as_slice(), committer.email.as_slice())
    {
        println!(
            " Author: {} <{}>",
            String::from_utf8_lossy(&pending.author.name),
            String::from_utf8_lossy(&pending.author.email)
        );
    }
    let changed = git
        .repo
        .diff_trees(
            Some(&pending.ours_tree),
            Some(&pending.tree),
            &Pathspec::default(),
        )?
        .len();
    println!("{changed} file(s) changed");
    Ok(id)
}

/// The picks the sequencer still has to do, the first being the one that stopped.
fn read_todo(git: &GitHome) -> Result<Todo> {
    let Some(text) = read_text(&git_dir(git).join(SEQUENCER).join("todo")) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let verb = match (words.next(), words.next()) {
            (Some("pick" | "p"), Some(rev)) => Some((Verb::Pick, rev)),
            (Some("revert" | "r"), Some(rev)) => Some((Verb::Revert, rev)),
            (None, _) => None,
            _ => bail!("unusable instruction sheet: {line}"),
        };
        if let Some((verb, rev)) = verb {
            let id = git
                .repo
                .resolve(rev)
                .map_err(|_| anyhow!("unusable instruction sheet: {line}"))?;
            out.push((verb, id));
        }
    }
    Ok(out)
}

fn saved_opts(git: &GitHome) -> Opts {
    read_text(&git_dir(git).join(SEQUENCER).join("opts"))
        .map_or_else(Opts::default, |t| Opts::load(&t))
}

fn continue_pick(git: &GitHome) -> Result<()> {
    let dir = git_dir(git);
    let stopped = match read_id(git, &dir.join(PICK_HEAD))? {
        Some(id) => Some((Verb::Pick, id)),
        None => read_id(git, &dir.join(REVERT_HEAD))?.map(|id| (Verb::Revert, id)),
    };
    let sequenced = dir.join(SEQUENCER).exists();
    if stopped.is_none() && !sequenced {
        return Err(fail(&["error: no cherry-pick or revert in progress"]));
    }
    let opts = saved_opts(git);
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        eprintln!("error: Committing is not possible because you have unmerged files.");
        eprintln!("hint: Fix them up in the work tree, and then use 'pyn add/rm <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
        bail!("Exiting because of an unresolved conflict.");
    }
    let head = git.repo.head()?;
    let head_tree = match head.id() {
        Some(h) => Some(git.repo.commit(&h)?.tree),
        None => None,
    };
    let tree = write_tree(git, &opened.index)?;
    match stopped {
        Some((verb, id)) => {
            let picked = git.repo.commit(&id)?;
            let ours_tree = match head_tree {
                Some(t) => t,
                None => write_tree(git, &pyn_git::Index::new(2))?,
            };
            if tree == ours_tree {
                say_empty(git, verb)?;
                std::process::exit(1);
            }
            let message = match merge_message(git, false)? {
                Some(m) => m,
                None => clean_message(&[String::from_utf8_lossy(&picked.message).into_owned()]),
            };
            let author = match verb {
                Verb::Pick => picked.author,
                Verb::Revert => identity(git, "author")?,
            };
            commit_pick(
                git,
                &Pending {
                    tree,
                    ours_tree,
                    author,
                    message,
                    reflog: match verb {
                        Verb::Pick => "commit (cherry-pick)",
                        Verb::Revert => "commit",
                    }
                    .to_string(),
                    sign: opts.sign.clone(),
                },
                true,
            )
            .inspect_err(|e| {
                // `git commit` ends a refused commit with status 1.
                if e.is::<HookRefused>() {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            })?;
            let _ = std::fs::remove_file(dir.join(verb.head_file()));
            clear_merge_state(git);
        }
        None => {
            let committed = git.repo.flatten(head_tree.as_ref(), &Pathspec::default())?;
            if git.repo.index_leaves(&opened.index) != committed {
                return Err(fail(&[
                    "error: your local changes would be overwritten by cherry-pick or revert.",
                    "hint: commit your changes or stash them to proceed.",
                ]));
            }
        }
    }
    if !sequenced {
        return Ok(());
    }
    let todo = read_todo(git)?;
    let rest = todo.get(1..).unwrap_or_default();
    if let Some(h) = git.repo.head_commit()? {
        write_state(git, "sequencer/abort-safety", &format!("{h}\n"))?;
    }
    run(git, &opts, rest, true)
}

/// Puts the index and work tree back to `rev` (`HEAD` when `None`), keeping unrelated local changes.
fn reset_merge(git: &GitHome, rev: Option<&ObjectId>) -> Result<()> {
    let head = git.repo.head_commit()?;
    let rev = rev.copied().or(head).map(|id| id.to_hex());
    reset::reset(
        git,
        &git.ws.root,
        &crate::local_reset::ResetArgs::quiet_merge(rev.as_deref()),
    )
}

fn abort(git: &GitHome) -> Result<()> {
    let dir = git_dir(git);
    let seq = dir.join(SEQUENCER);
    if seq.exists() {
        let head = git.repo.head_commit()?;
        let safe = read_id(git, &seq.join("abort-safety"))?;
        if safe.is_some() && safe != head {
            eprintln!("warning: You seem to have moved HEAD. Not rewinding, check your HEAD!");
            let _ = std::fs::remove_dir_all(&seq);
            return Ok(());
        }
        let orig = read_id(git, &seq.join("head"))?;
        reset_merge(git, orig.as_ref())?;
        let _ = std::fs::remove_dir_all(&seq);
        return Ok(());
    }
    if !dir.join(PICK_HEAD).exists() && !dir.join(REVERT_HEAD).exists() {
        return Err(fail(&["error: no cherry-pick or revert in progress"]));
    }
    reset_merge(git, None)
}

fn skip(git: &GitHome) -> Result<()> {
    let dir = git_dir(git);
    if !dir.join(PICK_HEAD).exists() && !dir.join(REVERT_HEAD).exists() {
        return Err(fail(&[
            "error: cannot skip - no cherry-pick or revert in progress",
        ]));
    }
    reset_merge(git, None)?;
    if !dir.join(SEQUENCER).exists() {
        return Ok(());
    }
    let opts = saved_opts(git);
    let todo = read_todo(git)?;
    run(git, &opts, todo.get(1..).unwrap_or_default(), true)
}

fn quit(git: &GitHome) -> Result<()> {
    let dir = git_dir(git);
    let _ = std::fs::remove_dir_all(dir.join(SEQUENCER));
    for name in [PICK_HEAD, REVERT_HEAD, "SQUASH_MSG"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    clear_merge_state(git);
    Ok(())
}

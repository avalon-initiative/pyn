//! `pyn cherry-pick` on a git repository: each commit's change is merged onto the index and becomes a new commit (or
//! stays staged with `-n`). A stop leaves `CHERRY_PICK_HEAD`, `MERGE_MSG` and, for several commits, `.git/sequencer`
//! as git writes them, so either tool can continue, skip or abort.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use pyn_gitwork::{Head, Pathspec};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId};

use super::GitHome;
use super::commit::append_signoff;
use super::merge::{Landing, apply_merge, write_state};
use super::signing::{SignFlag, header_name, header_value, signer_for};
use super::stage::{
    clean_message, clear_merge_state, identity, merge_message, refuse_states, warn_about_hooks_for,
    write_tree,
};
use super::treemerge::{Labels, merge_trees};
use super::{convert, path_string, reset};
use crate::local_pick::{PickArgs, select, with_origin};

const ABBREV: usize = 7;
const HOOKS: [&str; 3] = ["prepare-commit-msg", "commit-msg", "post-commit"];
const PICK_HEAD: &str = "CHERRY_PICK_HEAD";
const SEQUENCER: &str = "sequencer";

/// The options that outlive one command, kept in `sequencer/opts`.
#[derive(Default, Clone)]
struct Opts {
    no_commit: bool,
    record_origin: bool,
    signoff: bool,
    mainline: Option<u32>,
}

impl Opts {
    fn of(args: &PickArgs) -> Self {
        Self {
            no_commit: args.no_commit,
            record_origin: args.record_origin,
            signoff: args.signoff,
            mainline: args.mainline,
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

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

fn subject_of(message: &[u8]) -> String {
    String::from_utf8_lossy(message)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn git_dir(git: &GitHome) -> PathBuf {
    git.repo.git().git_dir().to_path_buf()
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn read_id(git: &GitHome, path: &Path) -> Result<Option<ObjectId>> {
    let Some(text) = read_text(path) else {
        return Ok(None);
    };
    let hex = text.lines().next().unwrap_or_default().trim();
    Ok(Some(ObjectId::from_hex(git.repo.algorithm(), hex)?))
}

fn fail(lines: &[&str]) -> anyhow::Error {
    for line in lines {
        eprintln!("{line}");
    }
    anyhow!("cherry-pick failed")
}

pub fn cherry_pick(git: &GitHome, args: &PickArgs) -> Result<()> {
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
    start(git, args)
}

fn in_progress(git: &GitHome) -> bool {
    let dir = git_dir(git);
    dir.join(PICK_HEAD).exists() || dir.join(SEQUENCER).exists()
}

fn start(git: &GitHome, args: &PickArgs) -> Result<()> {
    let opts = Opts::of(args);
    if in_progress(git) || git_dir(git).join("REVERT_HEAD").exists() {
        return Err(fail(&[
            "error: a cherry-pick or revert is already in progress",
            "hint: try \"pyn cherry-pick (--continue | --skip | --abort | --quit)\"",
        ]));
    }
    refuse_states(git, &["MERGE_HEAD", "rebase-merge", "rebase-apply"])?;
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.skip_worktree) {
        bail!("this repository uses sparse checkout, which pyn does not support yet");
    }
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        return Err(fail(&[
            "error: Cherry-picking is not possible because you have unmerged files.",
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
    )?;
    if picks.is_empty() {
        return Err(fail(&["error: empty commit set passed"]));
    }
    for id in &picks {
        parent_of(git, id, opts.mainline)?;
    }
    if !opts.no_commit {
        let tree = match head.id() {
            Some(h) => Some(git.repo.commit(&h)?.tree),
            None => None,
        };
        let committed = git.repo.flatten(tree.as_ref(), &Pathspec::default())?;
        if git.repo.index_leaves(&opened.index) != committed {
            return Err(fail(&[
                "error: your local changes would be overwritten by cherry-pick.",
                "hint: commit your changes or stash them to proceed.",
            ]));
        }
    }
    let single = args.commits.len() == 1 && !args.commits[0].contains("..");
    if !single && head.id().is_none() {
        bail!("an unborn branch takes one commit at a time; pick the first, then the rest");
    }
    warn_about_hooks_for(git, &HOOKS);
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
fn parent_of(git: &GitHome, id: &ObjectId, mainline: Option<u32>) -> Result<Option<ObjectId>> {
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

fn write_todo(git: &GitHome, picks: &[ObjectId]) -> Result<()> {
    let mut text = String::new();
    for id in picks {
        text.push_str(&format!(
            "pick {id} {}\n",
            subject_of(&git.repo.commit(id)?.message)
        ));
    }
    write_state(git, "sequencer/todo", &text)
}

/// Applies `picks` in order. With `sequenced` the todo list is kept current for `--continue`, `--skip` and `--abort`.
fn run(git: &GitHome, opts: &Opts, picks: &[ObjectId], sequenced: bool) -> Result<()> {
    for (i, id) in picks.iter().enumerate() {
        if sequenced {
            write_todo(git, &picks[i..])?;
        }
        let outcome = match apply(git, opts, id) {
            Ok(outcome) => outcome,
            Err(e) => {
                if sequenced && i == 0 {
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

fn apply(git: &GitHome, opts: &Opts, id: &ObjectId) -> Result<Outcome> {
    let picked = git.repo.commit(id)?;
    let parent = parent_of(git, id, opts.mainline)?;
    let base_tree = match &parent {
        Some(p) => Some(git.repo.commit(p)?.tree),
        None => None,
    };
    let opened = git.open_index()?;
    let ours_tree = write_tree(git, &opened.index)?;
    let label = format!("{} ({})", short(id), subject_of(&picked.message));
    let incoming =
        git.repo
            .diff_trees(base_tree.as_ref(), Some(&picked.tree), &Pathspec::default())?;
    convert::refuse_conversions(
        git,
        incoming
            .iter()
            .filter_map(|c| c.new.map(|e| (c.path.as_slice(), e.mode))),
    )?;
    let merged = merge_trees(
        git,
        base_tree.as_ref(),
        Some(&ours_tree),
        Some(&picked.tree),
        &Labels {
            ours: "HEAD",
            theirs: &label,
            base: &match parent {
                Some(_) => format!("parent of {label}"),
                None => "(empty tree)".to_string(),
            },
        },
    )?;
    let committer = identity(git, "committer")?;
    let mut message = picked.message.clone();
    if opts.record_origin {
        message = with_origin(message, id);
    }
    if opts.signoff {
        message = append_signoff(message, &committer.name, &committer.email);
    }
    let landing = Landing {
        theirs: &label,
        orig_head: None,
        strict_index: false,
    };
    let applied = match apply_merge(git, &opened, Some(&ours_tree), &merged, &landing)? {
        Ok(applied) => applied,
        Err(refusal) => {
            refusal.blocked.print("merge", "merge");
            return Err(fail(&["Aborting"]));
        }
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
            eprintln!("error: could not apply {label}");
            eprintln!("hint: after resolving the conflicts, mark the corrected paths");
            eprintln!("hint: with 'pyn add <paths>' or 'pyn rm <paths>'");
        } else {
            write_state(git, PICK_HEAD, &format!("{id}\n"))?;
            eprintln!("error: could not apply {label}");
            eprintln!("hint: After resolving the conflicts, mark them with");
            eprintln!("hint: \"pyn add/rm <pathspec>\", then run");
            eprintln!("hint: \"pyn cherry-pick --continue\".");
            eprintln!("hint: You can instead skip this commit with \"pyn cherry-pick --skip\".");
            eprintln!("hint: To abort and get back to the state before \"pyn cherry-pick\",");
            eprintln!("hint: run \"pyn cherry-pick --abort\".");
        }
        return Ok(Outcome::Stopped);
    }
    if opts.no_commit {
        write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&message))?;
        return Ok(Outcome::Applied);
    }
    let tree = write_tree(git, &applied.index)?;
    if tree == ours_tree {
        write_state(git, PICK_HEAD, &format!("{id}\n"))?;
        write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&message))?;
        say_empty();
        return Ok(Outcome::Stopped);
    }
    let made = commit_pick(
        git,
        &Pending {
            tree,
            ours_tree,
            author: picked.author,
            message,
            reflog: "cherry-pick",
        },
    )?;
    Ok(Outcome::Committed(made))
}

fn say_empty() {
    eprintln!("The previous cherry-pick is now empty, possibly due to conflict resolution.");
    eprintln!("pyn does not keep empty commits; drop it with 'pyn cherry-pick --skip'.");
}

struct Pending {
    tree: ObjectId,
    /// The tree of `HEAD`, which the new commit's changes are counted against.
    ours_tree: ObjectId,
    author: pyn_object::Signature,
    message: Vec<u8>,
    /// The reflog action: git words a commit made by `--continue` differently from one made directly.
    reflog: &'static str,
}

/// Writes the commit, moves the branch (or a detached `HEAD`) to it and reports it as `git commit` does.
fn commit_pick(git: &GitHome, pending: &Pending) -> Result<ObjectId> {
    let head = git.repo.head()?;
    let committer = identity(git, "committer")?;
    let mut record = Commit {
        tree: pending.tree,
        parents: head.id().into_iter().collect(),
        author: pending.author.clone(),
        committer: committer.clone(),
        extra_headers: Vec::new(),
        message: pending.message.clone(),
    };
    if let Some(signer) = signer_for(
        git,
        &SignFlag::default(),
        "commit.gpgsign",
        Some(&committer),
    )? {
        let sig = signer.sign(&record.encode(git.algorithm())?)?;
        record
            .extra_headers
            .push((header_name(git.algorithm()).to_vec(), header_value(sig)));
    }
    let id = git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode(git.algorithm())?)?;
    let first = subject_of(&pending.message);
    git.move_head(&id, &format!("{}: {first}", pending.reflog))
        .map_err(|e| e.context("HEAD moved while cherry-picking; run the command again"))?;
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
fn read_todo(git: &GitHome) -> Result<Vec<ObjectId>> {
    let Some(text) = read_text(&git_dir(git).join(SEQUENCER).join("todo")) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        match (words.next(), words.next()) {
            (Some("pick" | "p"), Some(rev)) => out.push(
                git.repo
                    .resolve(rev)
                    .map_err(|_| anyhow!("unusable instruction sheet: {line}"))?,
            ),
            (None, _) => {}
            _ => bail!("unusable instruction sheet: {line}"),
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
    let stopped = read_id(git, &dir.join(PICK_HEAD))?;
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
        Some(id) => {
            let picked = git.repo.commit(&id)?;
            let ours_tree = match head_tree {
                Some(t) => t,
                None => write_tree(git, &pyn_git::Index::new(2))?,
            };
            if tree == ours_tree {
                say_empty();
                std::process::exit(1);
            }
            let message = match merge_message(git)? {
                Some(m) => m,
                None => clean_message(&[String::from_utf8_lossy(&picked.message).into_owned()]),
            };
            commit_pick(
                git,
                &Pending {
                    tree,
                    ours_tree,
                    author: picked.author,
                    message,
                    reflog: "commit (cherry-pick)",
                },
            )?;
            let _ = std::fs::remove_file(dir.join(PICK_HEAD));
            clear_merge_state(git);
        }
        None => {
            let committed = git.repo.flatten(head_tree.as_ref(), &Pathspec::default())?;
            if git.repo.index_leaves(&opened.index) != committed {
                return Err(fail(&[
                    "error: your local changes would be overwritten by cherry-pick.",
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
    if !dir.join(PICK_HEAD).exists() {
        return Err(fail(&["error: no cherry-pick or revert in progress"]));
    }
    reset_merge(git, None)
}

fn skip(git: &GitHome) -> Result<()> {
    let dir = git_dir(git);
    if !dir.join(PICK_HEAD).exists() {
        return Err(fail(&["error: cannot skip - no cherry-pick in progress"]));
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
    for name in [PICK_HEAD, "REVERT_HEAD", "SQUASH_MSG"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    clear_merge_state(git);
    Ok(())
}

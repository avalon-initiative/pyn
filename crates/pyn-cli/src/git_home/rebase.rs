//! `pyn rebase` on a git repository: the commits of `upstream..HEAD` are replayed one by one onto a detached `HEAD` with
//! the cherry-pick merge, and the branch is moved at the end. A stop leaves `.git/rebase-merge/`, `REBASE_HEAD` and
//! `MERGE_MSG` as git's merge backend writes them, so either tool can continue, skip or abort. With `-i` the commits are
//! an instruction sheet (`todo`) the user edits: `pick`, `reword`, `edit`, `squash`, `fixup`, `drop`, `exec`, `break`; with
//! `--rebase-merges` also `label`, `reset`, `merge` and `update-ref` (see `rebase_merges`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use clap::{ArgGroup, Args};
use pyn_git::{Remote, map_ref, tracking};
use pyn_gitwork::{Head, Kind, Pathspec};
use pyn_object::{Commit, ObjectId, Signature};

use super::GitHome;
use super::editor::{self, Which};
use super::meld;
use super::merge::{Landing, apply_merge, write_orig_head_as, write_state};
use super::patch_file;
use super::pick::{
    Failed, Merged, Pending, Verb, git_dir, make_commit_on, merge_pick, read_id, read_text, short,
    subject_of,
};
use super::rebase_merges::{self as merges, Mode as MergeMode, Range};
use super::rewrite;
use super::stage::{clean_message, merge_heads, parse_date, raw_mode, write_tree};
use super::stash::{Applying, PushArgs, apply_commit, save, store_stash};
use super::strategy::Settings;
use super::todo::{self, Cmd, Flag, Item};
use super::treemerge::{Labels, merge_trees, result_tree};
use super::worktree::{Mode, move_tree};
use super::{path_string, reset};
use crate::local_reset::ResetArgs;
use pyn_object::Kind as ObjectKind;

const DIR: &str = "rebase-merge";
const HEAD_FILE: &str = "REBASE_HEAD";

/// Reapply the commits of the current branch on top of another base.
#[derive(Args, Debug, Default)]
#[command(group(ArgGroup::new("control").args(["continue_rebase", "abort", "skip", "quit", "edit_todo"])))]
pub struct RebaseArgs {
    /// The commit the current branch is compared with: the commits after it are replayed. Default: the branch's
    /// upstream.
    #[arg(conflicts_with = "control")]
    pub(crate) upstream: Option<String>,
    /// Switch to this branch first, then rebase it.
    #[arg(conflicts_with = "control")]
    pub(crate) branch: Option<String>,
    /// Replay onto this commit instead of the upstream.
    #[arg(long, value_name = "NEWBASE", conflicts_with = "control")]
    pub(crate) onto: Option<String>,
    /// Commit what was resolved and go on with the remaining commits.
    #[arg(long = "continue")]
    pub(crate) continue_rebase: bool,
    /// Edit the instruction sheet of the rebase in progress.
    #[arg(long)]
    pub(crate) edit_todo: bool,
    /// Give up and put the branch and the files back as they were before the rebase.
    #[arg(long)]
    pub(crate) abort: bool,
    /// Drop the commit that stopped on conflicts and go on with the rest.
    #[arg(long)]
    pub(crate) skip: bool,
    /// Forget the rebase in progress, leaving `HEAD` and the files as they are.
    #[arg(long)]
    pub(crate) quit: bool,
    /// Stash local changes before the rebase and apply them after it.
    #[arg(long, overrides_with = "no_autostash", conflicts_with = "control")]
    pub(crate) autostash: bool,
    #[arg(long, overrides_with = "autostash", conflicts_with = "control")]
    pub(crate) no_autostash: bool,
    /// Replay every commit, even those that would fast-forward.
    #[arg(
        long = "no-ff",
        short = 'f',
        visible_alias = "force-rebase",
        overrides_with = "ff",
        conflicts_with = "control"
    )]
    pub(crate) no_ff: bool,
    #[arg(
        long,
        visible_alias = "no-force-rebase",
        overrides_with = "no_ff",
        conflicts_with = "control"
    )]
    pub(crate) ff: bool,
    #[arg(short, long)]
    pub(crate) quiet: bool,
    /// Accepted: no editor is ever started and the merge backend is the only one.
    #[arg(short = 'm', long, hide = true)]
    pub(crate) merge: bool,
    #[arg(short = 'v', long, hide = true)]
    pub(crate) verbose: bool,
    #[arg(short = 'n', long, hide = true)]
    pub(crate) no_stat: bool,
    #[arg(long, hide = true)]
    pub(crate) stat: bool,
    #[arg(long, overrides_with = "fork_point")]
    pub(crate) no_fork_point: bool,
    #[arg(long, hide = true)]
    pub(crate) keep_empty: bool,
    #[arg(long, overrides_with = "autosquash")]
    pub(crate) no_autosquash: bool,
    #[arg(long, overrides_with_all = ["rebase_merges"])]
    pub(crate) no_rebase_merges: bool,
    #[arg(long, overrides_with = "update_refs")]
    pub(crate) no_update_refs: bool,
    /// Edit the list of commits (`pick`, `reword`, `edit`, `squash`, `fixup`, `drop`, `exec`, `label`, `reset`,
    /// `merge`, `update-ref`) before replaying them.
    #[arg(short = 'i', long, conflicts_with = "control")]
    pub(crate) interactive: bool,
    /// Recreate merge commits instead of flattening them; `=rebase-cousins` also rebases branches that do not start at
    /// the upstream.
    #[arg(short = 'r', long, value_name = "MODE", num_args = 0..=1, require_equals = true, overrides_with_all = ["no_rebase_merges", "rebase_merges"], conflicts_with = "control")]
    pub(crate) rebase_merges: Option<Option<String>>,
    /// Replay every commit down to the root; the first positional argument is then the branch.
    #[arg(long, conflicts_with = "control")]
    pub(crate) root: bool,
    /// Run this command after every commit that is made (repeatable).
    #[arg(short = 'x', long, value_name = "CMD", action = clap::ArgAction::Append, conflicts_with = "control")]
    pub(crate) exec: Vec<String>,
    #[arg(short = 's', long, hide = true)]
    pub(crate) strategy: Option<String>,
    #[arg(short = 'X', long, hide = true)]
    pub(crate) strategy_option: Option<String>,
    /// With `-i`, move `fixup!` and `squash!` commits under the commit they name.
    #[arg(long, overrides_with = "no_autosquash")]
    pub(crate) autosquash: bool,
    /// Take the commits to replay from where the upstream was in its reflog (the default without an upstream argument).
    #[arg(long, overrides_with = "no_fork_point", conflicts_with = "control")]
    pub(crate) fork_point: bool,
    #[arg(long, hide = true)]
    pub(crate) signoff: bool,
    #[arg(long, hide = true)]
    pub(crate) keep_base: bool,
    /// Move the other branches that point into the replayed commits.
    #[arg(long, overrides_with = "no_update_refs", conflicts_with = "control")]
    pub(crate) update_refs: bool,
    #[arg(long, hide = true)]
    pub(crate) reapply_cherry_picks: bool,
    #[arg(long, hide = true)]
    pub(crate) apply: bool,
    #[arg(long, hide = true)]
    pub(crate) empty: Option<String>,
    #[arg(short = 'S', long, hide = true, num_args = 0..=1, require_equals = true)]
    pub(crate) gpg_sign: Option<Option<String>>,
}

impl RebaseArgs {
    pub(crate) fn refuse_unsupported(&self) {
        let given = [
            (
                "--strategy",
                self.strategy.is_some(),
                "merge strategies are not built",
            ),
            (
                "--strategy-option",
                self.strategy_option.is_some(),
                "merge strategies are not built",
            ),
            ("--signoff", self.signoff, "trailers are not added"),
            ("--keep-base", self.keep_base, "the merge base is not kept"),
            (
                "--reapply-cherry-picks",
                self.reapply_cherry_picks,
                "commits already upstream are always dropped",
            ),
            ("--apply", self.apply, "only the merge backend is built"),
            (
                "--empty",
                self.empty.as_deref().is_some_and(|e| e != "drop"),
                "commits that become empty are always dropped",
            ),
            (
                "--gpg-sign",
                self.gpg_sign.is_some(),
                "set commit.gpgsign instead",
            ),
        ];
        if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
            crate::usage_error(&format!("`pyn rebase {flag}`: {why}"));
        }
    }
}

/// What a rebase is asked to do, from the command line or from `pull --rebase`.
pub(super) struct Plan {
    /// The commit the branch is compared with; none for `--root`.
    pub upstream: Option<ObjectId>,
    /// Where the upstream was in its reflog, which replaces it as the start of the commits to replay.
    pub restrict: Option<ObjectId>,
    pub onto: ObjectId,
    /// What the reflog calls the new base.
    pub onto_name: String,
    /// A branch (or commit) to switch to first.
    pub switch_to: Option<String>,
    pub no_ff: bool,
    pub quiet: bool,
    /// `Some` when the command line decided; otherwise `rebase.autostash`.
    pub autostash: Option<bool>,
    /// `rebase`, or `pull --rebase`: the reflog action.
    pub action: &'static str,
    /// `-i`: the user edits the sheet.
    pub interactive: bool,
    pub exec: Vec<String>,
    pub autosquash: bool,
    /// `--rebase-merges`: `Some(true)` for `rebase-cousins`.
    pub merges: Option<bool>,
    pub root: bool,
    /// `--root` without `--onto`: the empty commit the new root is built on.
    pub squash_onto: Option<ObjectId>,
    pub update_refs: bool,
}

impl Plan {
    /// What the commits to replay are counted from: the upstream, or the new base for `--root`.
    fn range_base(&self) -> ObjectId {
        self.upstream.unwrap_or(self.onto)
    }

    fn refusal_word(&self) -> &'static str {
        if self.action == "rebase" {
            "rebase"
        } else {
            "pull with rebase"
        }
    }
}

fn refuse(lines: &[&str], status: i32) -> ! {
    for line in lines {
        eprintln!("{line}");
    }
    std::process::exit(status);
}

fn die(message: &str) -> ! {
    eprintln!("fatal: {message}");
    std::process::exit(128);
}

pub fn rebase(git: &GitHome, args: &RebaseArgs) -> Result<()> {
    args.refuse_unsupported();
    let done = if args.edit_todo {
        edit_todo(git)
    } else if args.quit {
        quit(git)
    } else if args.abort {
        abort(git)
    } else if args.skip {
        skip(git)
    } else if args.continue_rebase {
        cont(git)
    } else {
        plan_of(git, args).and_then(|plan| begin(git, &plan))
    };
    done.map_err(|e| {
        if e.is::<Failed>() {
            anyhow!("rebase failed")
        } else {
            e
        }
    })
}

/// The commit a spec names, or git's `fatal:` line.
fn commit_named(git: &GitHome, spec: &str, what: &str) -> ObjectId {
    match git.repo.resolve(spec) {
        Ok(id) if git.repo.commit(&id).is_ok() => id,
        _ => die(&format!("{what} '{spec}'")),
    }
}

/// The remote-tracking ref the current branch's upstream configuration names, and its commit.
fn configured_upstream(git: &GitHome) -> Result<(ObjectId, String)> {
    let Head::Branch { name, .. } = git.repo.head()? else {
        eprintln!("You are not currently on a branch.");
        eprintln!("Please specify which branch you want to rebase against.");
        eprintln!("See pyn rebase --help for details.\n\n    pyn rebase '<branch>'\n");
        std::process::exit(1);
    };
    let config = git.config();
    let t = tracking(config, &name);
    let (Some(remote), Some(merge)) = (t.remote.as_deref(), t.merge.first()) else {
        eprintln!("There is no tracking information for the current branch.");
        eprintln!("Please specify which branch you want to rebase against.");
        eprintln!("See pyn rebase --help for details.\n\n    pyn rebase '<branch>'\n");
        eprintln!(
            "If you wish to set tracking information for this branch you can do so with:\n\n    pyn branch --set-upstream-to=<remote>/<branch> {name}\n"
        );
        std::process::exit(1);
    };
    let tracked = if remote == "." {
        Some(merge.clone())
    } else {
        Remote::read(config, remote).and_then(|r| map_ref(&r.fetch_specs(), merge))
    };
    match tracked.and_then(|t| git.repo.resolve(&t).ok().map(|id| (id, t))) {
        Some(found) => Ok(found),
        None => die(&format!("invalid upstream '{merge}'")),
    }
}

/// The commit the upstream's reflog says the branch forked from: the one best common ancestor of `head` and
/// every commit the upstream was at, if it is one of them.
fn fork_point(git: &GitHome, refname: &str, head: &ObjectId) -> Result<Option<ObjectId>> {
    let Some(full) = git.repo.expand_ref_name(refname)? else {
        die(&format!("No such ref: '{refname}'"));
    };
    let mut named: Vec<ObjectId> = Vec::new();
    for (n, entry) in git.repo.refs().read_reflog(&full)?.iter().enumerate() {
        if n == 0 {
            named.push(entry.old);
        }
        named.push(entry.new);
    }
    if named.is_empty()
        && let Some(id) = git.repo.refs().resolve(&full)?
    {
        named.push(id);
    }
    let mut seen: Vec<ObjectId> = Vec::new();
    for id in named {
        if !seen.contains(&id) && git.repo.commit(&id).is_ok() {
            seen.push(id);
        }
    }
    let mut theirs: HashSet<ObjectId> = HashSet::new();
    for id in &seen {
        theirs.extend(git.reachable(id)?);
    }
    let common: Vec<ObjectId> = git
        .reachable(head)?
        .into_iter()
        .filter(|id| theirs.contains(id))
        .collect();
    let mut covered: HashSet<ObjectId> = HashSet::new();
    for id in &common {
        for parent in &git.repo.commit(id)?.parents {
            if !covered.contains(parent) {
                covered.extend(git.reachable(parent)?);
            }
        }
    }
    let best: Vec<ObjectId> = common
        .into_iter()
        .filter(|id| !covered.contains(id))
        .collect();
    Ok(match best.as_slice() {
        [only] if seen.contains(only) => Some(*only),
        _ => None,
    })
}

/// `--rebase-merges[=<mode>]` or `rebase.rebaseMerges`: `Some(true)` for `rebase-cousins`.
fn merges_mode(git: &GitHome, args: &RebaseArgs) -> Option<bool> {
    let mode = |text: &str| match text {
        "" | "no-rebase-cousins" => Some(false),
        "rebase-cousins" => Some(true),
        other => die(&format!("unknown mode: {other}")),
    };
    if args.no_rebase_merges {
        return None;
    }
    if let Some(given) = &args.rebase_merges {
        return mode(given.as_deref().unwrap_or(""));
    }
    let configured = git.repo.config().get("rebase.rebasemerges")?;
    match configured.to_ascii_lowercase().as_str() {
        "false" | "no" | "off" | "0" => None,
        "true" | "yes" | "on" | "1" => Some(false),
        other => mode(other),
    }
}

/// The empty commit a `--root` rebase without `--onto` builds the new root on.
fn make_squash_onto(git: &GitHome) -> Result<ObjectId> {
    let empty = git.repo.objects().write(ObjectKind::Tree, &[])?;
    let record = Commit {
        tree: empty,
        parents: Vec::new(),
        author: super::stage::identity(git, "author")?,
        committer: super::stage::identity(git, "committer")?,
        extra_headers: Vec::new(),
        message: Vec::new(),
    };
    Ok(git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode(git.algorithm())?)?)
}

fn plan_of(git: &GitHome, args: &RebaseArgs) -> Result<Plan> {
    let (upstream_spec, switch_to) = if args.root {
        if args.branch.is_some() {
            crate::usage_error("`pyn rebase --root` takes at most one argument, the branch");
        }
        (None, args.upstream.clone())
    } else {
        (args.upstream.clone(), args.branch.clone())
    };
    if args.root && args.fork_point {
        die("options '--root' and '--fork-point' cannot be used together");
    }
    let mut upstream_name = upstream_spec.clone();
    let upstream = if args.root {
        None
    } else {
        Some(match &upstream_spec {
            Some(spec) => commit_named(git, spec, "invalid upstream"),
            None => {
                let (id, name) = configured_upstream(git)?;
                upstream_name = Some(name);
                id
            }
        })
    };
    let fork = if args.fork_point {
        true
    } else if args.no_fork_point {
        false
    } else {
        upstream_spec.is_none() && !args.root
    };
    let restrict = match (fork, &upstream_name) {
        (true, Some(name)) => {
            let orig = match &switch_to {
                Some(b) => match git.branch_commit(b)? {
                    Some(id) => Some(id),
                    None => git.repo.resolve(b).ok(),
                },
                None => git.repo.head_commit()?,
            };
            match orig {
                Some(head) => fork_point(git, name, &head)?,
                None => None,
            }
        }
        _ => None,
    };
    let mut squash_onto = None;
    let (onto, onto_name) = match (&args.onto, upstream) {
        (Some(spec), _) => (
            commit_named(git, spec, "Does not point to a valid commit"),
            spec.clone(),
        ),
        (None, Some(upstream)) => (
            upstream,
            upstream_spec
                .clone()
                .or_else(|| upstream_name.clone())
                .unwrap_or_else(|| upstream.to_hex()),
        ),
        (None, None) => {
            let empty = make_squash_onto(git)?;
            squash_onto = Some(empty);
            (empty, empty.to_hex())
        }
    };
    let update_refs = if args.update_refs {
        true
    } else if args.no_update_refs {
        false
    } else {
        git.repo.bool_setting("rebase.updaterefs", false)
    };
    let merges = merges_mode(git, args);
    Ok(Plan {
        upstream,
        restrict,
        onto,
        onto_name,
        switch_to,
        no_ff: args.no_ff,
        quiet: args.quiet,
        autostash: if args.autostash {
            Some(true)
        } else if args.no_autostash {
            Some(false)
        } else {
            None
        },
        action: "rebase",
        interactive: args.interactive,
        exec: args.exec.clone(),
        autosquash: !args.no_autosquash
            && (args.autosquash
                || (args.interactive && git.repo.bool_setting("rebase.autosquash", false))),
        merges,
        root: args.root,
        squash_onto,
        update_refs,
    })
}

/// Replays the local commits onto `theirs`, as `pull --rebase` does after fetching.
pub(super) fn pull_rebase(
    git: &GitHome,
    theirs: &ObjectId,
    autostash: Option<bool>,
    quiet: bool,
) -> Result<()> {
    let plan = Plan {
        upstream: Some(*theirs),
        restrict: None,
        onto: *theirs,
        onto_name: theirs.to_hex(),
        switch_to: None,
        no_ff: false,
        quiet,
        autostash,
        action: "pull --rebase",
        interactive: false,
        exec: Vec::new(),
        autosquash: false,
        merges: None,
        root: false,
        squash_onto: None,
        update_refs: false,
    };
    begin(git, &plan).map_err(|e| {
        if e.is::<Failed>() {
            anyhow!("pull failed")
        } else {
            e
        }
    })
}

/// A rebase in progress, as the files in `rebase-merge/` hold it.
struct State {
    /// `refs/heads/<name>`, or `detached HEAD`.
    head_name: String,
    onto: ObjectId,
    orig_head: ObjectId,
    todo: Vec<Item>,
    /// Why the sheet could not be read, in git's words; nothing runs until it is fixed.
    todo_errors: Vec<String>,
    done: Vec<Item>,
    autostash: Option<ObjectId>,
    quiet: bool,
    no_ff: bool,
    /// A commit that becomes empty stops the rebase (`-i`) instead of being dropped.
    stop_empty: bool,
    action: &'static str,
    /// The empty commit a new root is built on (`--root`, or `reset [new root]`).
    squash_onto: Cell<Option<ObjectId>>,
    /// What git leaves in `GIT_REFLOG_ACTION` for the commits it makes through `git commit`.
    reflog_env: RefCell<String>,
}

fn dir_of(git: &GitHome) -> PathBuf {
    git_dir(git).join(DIR)
}

/// Whether a rebase with git's merge backend is under way.
pub(super) fn in_progress(git: &GitHome) -> bool {
    dir_of(git).join("head-name").is_file()
}

impl State {
    fn put(&self, git: &GitHome, name: &str, text: &str) -> Result<()> {
        write_state(git, &format!("{DIR}/{name}"), text)
    }

    fn save_progress(&self, git: &GitHome) -> Result<()> {
        self.put(
            git,
            "git-rebase-todo",
            &todo::sheet(git, &self.todo, false)?,
        )?;
        self.put(git, "done", &todo::sheet(git, &self.done, false)?)?;
        let counted = |items: &[Item]| items.iter().filter(|i| i.counts()).count();
        self.put(git, "msgnum", &format!("{}\n", counted(&self.done)))?;
        self.put(
            git,
            "end",
            &format!("{}\n", counted(&self.done) + counted(&self.todo)),
        )
    }

    fn create(&self, git: &GitHome) -> Result<()> {
        std::fs::create_dir_all(dir_of(git))?;
        self.put(git, "head-name", &format!("{}\n", self.head_name))?;
        self.put(git, "onto", &format!("{}\n", self.onto))?;
        self.put(git, "orig-head", &format!("{}\n", self.orig_head))?;
        for empty in ["interactive", "no-reschedule-failed-exec"] {
            self.put(git, empty, "")?;
        }
        if !self.stop_empty {
            self.put(git, "drop_redundant_commits", "")?;
        }
        if let Some(stash) = &self.autostash {
            self.put(git, "autostash", &format!("{stash}\n"))?;
        }
        if let Some(empty) = self.squash_onto.get() {
            self.put(git, "squash-onto", &format!("{empty}\n"))?;
        }
        self.save_progress(git)
    }
}

fn load(git: &GitHome) -> Result<State> {
    let dir = dir_of(git);
    if !dir.is_dir() {
        die("No rebase in progress?");
    }
    let text = |name: &str| read_text(&dir.join(name));
    let id_of = |name: &str| -> Result<ObjectId> {
        read_id(git, &dir.join(name))?.ok_or_else(|| anyhow!("the rebase has no {name}"))
    };
    let head_name = text("head-name")
        .ok_or_else(|| anyhow!("the rebase has no head-name"))?
        .trim()
        .to_string();
    let done_text = text("done").unwrap_or_default();
    let done = todo::parse(git, &done_text, true).map_err(|errors| {
        anyhow!(
            "unusable list of finished commands ({}); finish this rebase with git",
            errors.join("; ")
        )
    })?;
    let (todo, todo_errors) = match todo::parse(
        git,
        &text("git-rebase-todo").unwrap_or_default(),
        !done.is_empty(),
    ) {
        Ok(items) => (items, Vec::new()),
        Err(errors) => (Vec::new(), errors),
    };
    Ok(State {
        head_name,
        onto: id_of("onto")?,
        orig_head: id_of("orig-head")?,
        todo,
        todo_errors,
        done,
        autostash: read_id(git, &dir.join("autostash"))?,
        quiet: dir.join("quiet").exists(),
        no_ff: false,
        stop_empty: !dir.join("drop_redundant_commits").exists(),
        action: "rebase",
        squash_onto: Cell::new(read_id(git, &dir.join("squash-onto"))?),
        reflog_env: RefCell::new("rebase".into()),
    })
}

/// Stops with git's report when the sheet cannot be read.
fn require_valid(state: &State) {
    if state.todo_errors.is_empty() {
        return;
    }
    for line in &state.todo_errors {
        eprintln!("{line}");
    }
    eprintln!("error: please fix this using 'pyn rebase --edit-todo'.");
    std::process::exit(1);
}

fn clear_state(git: &GitHome) {
    let dir = git_dir(git);
    merges::delete_rewritten(git, &dir.join(DIR));
    let _ = std::fs::remove_dir_all(dir.join(DIR));
    for name in [HEAD_FILE, "AUTO_MERGE", "SQUASH_MSG"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

/// Why the work tree or index is not clean enough to rebase, if it is not.
fn unclean(git: &GitHome) -> Result<Option<&'static str>> {
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        return Ok(Some("You have unstaged changes."));
    }
    let changed = git
        .repo
        .unstaged_changes(&opened.index)?
        .values()
        .any(|k| matches!(k, Kind::Modified | Kind::Deleted | Kind::TypeChange));
    if changed {
        return Ok(Some("You have unstaged changes."));
    }
    let tree = match git.repo.head_commit()? {
        Some(h) => Some(git.repo.commit(&h)?.tree),
        None => None,
    };
    let committed = git.repo.flatten(tree.as_ref(), &Pathspec::default())?;
    Ok((git.repo.index_leaves(&opened.index) != committed)
        .then_some("Your index contains uncommitted changes."))
}

/// `pull --rebase` refuses a dirty tree before it fetches, as git does.
pub(super) fn require_clean_for_pull(git: &GitHome, autostash: bool) -> Result<()> {
    if autostash || git.repo.bool_setting("rebase.autostash", false) {
        return Ok(());
    }
    if git.repo.head_commit()?.is_some()
        && let Some(why) = unclean(git)?
    {
        refuse(
            &[
                &format!("error: cannot pull with rebase: {why}"),
                "error: Please commit or stash them.",
            ],
            128,
        );
    }
    Ok(())
}

fn autostash_wanted(git: &GitHome, plan: &Plan) -> bool {
    plan.autostash
        .unwrap_or_else(|| git.repo.bool_setting("rebase.autostash", false))
}

fn begin(git: &GitHome, plan: &Plan) -> Result<()> {
    let dir = git_dir(git);
    if dir.join(DIR).exists() {
        eprintln!(
            "fatal: It seems that there is already a rebase-merge directory, and\nI wonder if you are in the middle of another rebase.  If that is the case, please try\n\tpyn rebase (--continue | --abort | --skip)\nIf that is not the case, please\n\trm -fr \"{}\"\nand run me again.  I am stopping in case you still have something valuable there.",
            dir.join(DIR).display()
        );
        std::process::exit(128);
    }
    if dir.join("rebase-apply").exists() {
        bail!(
            "a rebase with git's apply backend is in progress; finish or abort it with git first"
        );
    }
    super::stage::refuse_states(
        git,
        &["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "sequencer"],
    )?;
    if git
        .open_index()?
        .index
        .entries
        .iter()
        .any(|e| e.skip_worktree)
    {
        bail!("this repository uses sparse checkout, which pyn does not support yet");
    }
    let head = git.repo.head()?;
    let switch = match &plan.switch_to {
        Some(spec) => Some(match git.branch_commit(spec)? {
            Some(id) => (Some(GitHome::branch_ref(spec)), id),
            None => (
                None,
                match git.repo.resolve(spec) {
                    Ok(id) => id,
                    Err(_) => die(&format!("no such branch/commit '{spec}'")),
                },
            ),
        }),
        None => None,
    };
    let (head_name, orig_head) = match (&switch, &head) {
        (Some((Some(name), id)), _) => (name.clone(), *id),
        (Some((None, id)), _) => ("detached HEAD".to_string(), *id),
        (None, Head::Branch { name, id: Some(id) }) => (GitHome::branch_ref(name), *id),
        (None, Head::Detached(id)) => ("detached HEAD".to_string(), *id),
        (None, Head::Branch { id: None, .. }) => bail!("there is no commit to rebase yet"),
    };
    let stash_it = autostash_wanted(git, plan);
    if !stash_it && let Some(why) = unclean(git)? {
        refuse(
            &[
                &format!("error: cannot {}: {why}", plan.refusal_word()),
                "error: Please commit or stash them.",
            ],
            if plan.action == "rebase" { 1 } else { 128 },
        );
    }
    let sheet_run = plan.interactive || plan.autosquash || !plan.exec.is_empty();
    if !plan.no_ff && !sheet_run && can_fast_forward(git, plan, &orig_head)? {
        if let Some((target, _)) = &switch {
            switch_to(git, target.as_deref(), &orig_head, plan)?;
        }
        if !plan.quiet {
            say_up_to_date(&head_name);
        }
        return Ok(());
    }
    if plan.no_ff && !sheet_run && !plan.quiet && can_fast_forward(git, plan, &orig_head)? {
        match head_name.strip_prefix("refs/heads/") {
            Some(b) => println!("Current branch {b} is up to date, rebase forced."),
            None => println!("HEAD is up to date, rebase forced."),
        }
    }
    let base = plan.range_base();
    let mut items = match plan.merges {
        Some(cousins) => merge_items(git, plan, &orig_head, cousins)?,
        None => picks(git, &base, plan.restrict.as_ref(), &orig_head)?
            .into_iter()
            .map(Item::pick)
            .collect(),
    };
    if items.is_empty() && sheet_run {
        items.push(Item::noop());
    }
    let mut tracked = Vec::new();
    if plan.update_refs {
        (items, tracked) = merges::add_update_refs(git, items, &head_name)?;
    }
    if plan.autosquash {
        items = todo::autosquash(git, items, true)?;
    }
    if !plan.exec.is_empty() {
        items = todo::with_exec(items, &plan.exec);
    }
    let autostash = if stash_it && unclean(git)?.is_some() {
        let args = PushArgs {
            message: Some("autostash".into()),
            ..PushArgs::default()
        };
        save(git, &args, false, true)?.map(|(id, _)| id)
    } else {
        None
    };
    if let Some(stash) = &autostash {
        eprintln!("Created autostash: {}", short(stash));
    }
    let state = State {
        head_name,
        onto: plan.onto,
        orig_head,
        todo: items,
        todo_errors: Vec::new(),
        done: Vec::new(),
        autostash,
        quiet: plan.quiet,
        no_ff: plan.no_ff,
        stop_empty: plan.interactive,
        action: plan.action,
        squash_onto: Cell::new(plan.squash_onto),
        reflog_env: RefCell::new(plan.action.into()),
    };
    let mut state = state;
    let mut bad_sheet = None;
    let original = todo::sheet(git, &state.todo, false)?;
    if plan.interactive {
        let range = if !plan.root {
            format!("{}..{}", short(&base), short(&state.orig_head))
        } else {
            short(&state.orig_head)
        };
        bad_sheet = edit_sheet(git, &mut state, &range)?;
    }
    let skipped = if plan.no_ff {
        0
    } else {
        unchanged_leading(git, &state)?
    };
    let start = match skipped {
        0 => state.onto,
        n => state.todo[n - 1].id.expect("a skipped pick has a commit"),
    };
    detach_at_onto(git, &state, plan, &start)?;
    write_orig_head_as(git, &state.orig_head, Some("rebase"))?;
    state.create(git)?;
    merges::write_tracked(git, &dir_of(git), &tracked)?;
    if state.quiet {
        state.put(git, "quiet", "")?;
    }
    if plan.interactive {
        state.put(git, "git-rebase-todo.backup", &original)?;
    }
    if let Some((edited, errors)) = bad_sheet {
        state.put(git, "git-rebase-todo", &edited)?;
        for name in ["done", "msgnum", "end"] {
            let _ = std::fs::remove_file(dir_of(git).join(name));
        }
        for line in &errors {
            eprintln!("{line}");
        }
        eprintln!(
            "You can fix this with 'pyn rebase --edit-todo' and then run 'pyn rebase --continue'."
        );
        eprintln!("Or you can abort the rebase with 'pyn rebase --abort'.");
        std::process::exit(1);
    }
    run(git, state, skipped)
}

/// How many commands at the start are picks whose commits already sit on the base in order; git does not run them.
fn unchanged_leading(git: &GitHome, state: &State) -> Result<usize> {
    let mut at = state.onto;
    let mut count = 0;
    for item in &state.todo {
        let Some(id) = item.id.filter(|_| item.cmd == Cmd::Pick) else {
            break;
        };
        if git.repo.commit(&id)?.parents != [at] {
            break;
        }
        at = id;
        count += 1;
    }
    Ok(count)
}

/// Lets the user edit the sheet before anything is checked out. A sheet that cannot be read comes back with git's
/// error lines; one with nothing in it ends the command without a trace.
fn edit_sheet(
    git: &GitHome,
    state: &mut State,
    range: &str,
) -> Result<Option<(String, Vec<String>)>> {
    let level = missing_check(git);
    let text = todo::editor_text(git, &state.todo, range, &state.onto)?;
    std::fs::create_dir_all(dir_of(git))?;
    let edited = match run_sheet_editor(git, &text) {
        Ok(edited) => edited,
        Err(e) => {
            let _ = std::fs::remove_dir_all(dir_of(git));
            return Err(e);
        }
    };
    match todo::parse(git, &edited, false) {
        Ok(items) if items.is_empty() => {
            let _ = std::fs::remove_dir_all(dir_of(git));
            if let Some(stash) = &state.autostash {
                restore_autostash(git, stash);
            }
            eprintln!("error: nothing to do");
            std::process::exit(1);
        }
        Ok(items) => {
            let report = dropped_report(git, &state.todo, &items, level)?;
            state.todo = items;
            let _ = std::fs::remove_dir_all(dir_of(git));
            match report {
                Some(lines) if level == Check::Error => Ok(Some((edited, lines))),
                Some(lines) => {
                    print_report(&lines);
                    Ok(None)
                }
                None => Ok(None),
            }
        }
        Err(errors) => {
            let _ = std::fs::remove_dir_all(dir_of(git));
            Ok(Some((edited, errors)))
        }
    }
}

fn run_sheet_editor(git: &GitHome, text: &str) -> Result<String> {
    let path = dir_of(git).join("git-rebase-todo");
    std::fs::write(&path, text)?;
    editor::run(git, Which::Sequence, &path)?;
    Ok(todo::stripspace(&std::fs::read_to_string(&path)?))
}

/// `--edit-todo`: the commands still to do, in the editor.
fn edit_todo(git: &GitHome) -> Result<()> {
    let mut state = load(git)?;
    let mut text = if state.todo_errors.is_empty() {
        todo::sheet(git, &state.todo, true)?
    } else {
        read_text(&dir_of(git).join("git-rebase-todo")).unwrap_or_default()
    };
    text.push_str(&todo::edit_help(missing_check(git) != Check::Ignore));
    let edited = run_sheet_editor(git, &text)?;
    match todo::parse(git, &edited, !state.done.is_empty()) {
        Ok(items) => state.todo = items,
        Err(errors) => {
            state.put(git, "git-rebase-todo", &edited)?;
            for line in &errors {
                eprintln!("{line}");
            }
            std::process::exit(1);
        }
    }
    state.put(
        git,
        "git-rebase-todo",
        &todo::sheet(git, &state.todo, false)?,
    )?;
    refuse_if_dropped(git, &state)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Ignore,
    Warn,
    Error,
}

/// `rebase.missingCommitsCheck`.
fn missing_check(git: &GitHome) -> Check {
    match git.repo.config().get("rebase.missingcommitscheck") {
        None => Check::Ignore,
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "ignore" => Check::Ignore,
            "warn" => Check::Warn,
            "error" => Check::Error,
            _ => {
                eprintln!(
                    "warning: unrecognized setting {v} for option rebase.missingCommitsCheck. Ignoring."
                );
                Check::Ignore
            }
        },
    }
}

/// git's report of the commits in `before` that `after` lacks, newest first; `None` when nothing is missing or the check is off.
fn dropped_report(
    git: &GitHome,
    before: &[Item],
    after: &[Item],
    level: Check,
) -> Result<Option<Vec<String>>> {
    if level == Check::Ignore {
        return Ok(None);
    }
    let kept: HashSet<ObjectId> = after.iter().filter_map(|i| i.id).collect();
    let mut lines = Vec::new();
    for id in before
        .iter()
        .rev()
        .filter_map(|i| i.id)
        .filter(|id| !kept.contains(id))
    {
        let subject = subject_of(&git.repo.commit(&id)?.message);
        lines.push(format!(" - {} {subject}", short(&id)));
    }
    if lines.is_empty() {
        return Ok(None);
    }
    let mut out = vec![
        "Warning: some commits may have been dropped accidentally.".to_string(),
        "Dropped commits (newer to older):".to_string(),
    ];
    out.extend(lines);
    out.extend(
        [
            "To avoid this message, use \"drop\" to explicitly remove a commit.",
            "",
            "Use 'pyn config rebase.missingCommitsCheck' to change the level of warnings.",
            "The possible behaviours are: ignore, warn, error.",
            "",
        ]
        .map(String::from),
    );
    Ok(Some(out))
}

fn print_report(lines: &[String]) {
    for line in lines {
        eprintln!("{line}");
    }
    eprintln!(
        "You can fix this with 'pyn rebase --edit-todo' and then run 'pyn rebase --continue'."
    );
    eprintln!("Or you can abort the rebase with 'pyn rebase --abort'.");
}

/// Compares the finished and remaining commands with the sheet as first written; `error` stops, `warn` only says so.
fn refuse_if_dropped(git: &GitHome, state: &State) -> Result<()> {
    let level = missing_check(git);
    let Some(text) = read_text(&dir_of(git).join("git-rebase-todo.backup")) else {
        return Ok(());
    };
    let Ok(before) = todo::parse(git, &text, true) else {
        return Ok(());
    };
    let now: Vec<Item> = state.done.iter().chain(&state.todo).cloned().collect();
    if let Some(lines) = dropped_report(git, &before, &now, level)? {
        print_report(&lines);
        if level == Check::Error {
            std::process::exit(1);
        }
    }
    Ok(())
}

fn say_up_to_date(head_name: &str) {
    match head_name.strip_prefix("refs/heads/") {
        Some(b) => println!("Current branch {b} is up to date."),
        None => println!("HEAD is up to date."),
    }
}

/// Whether the branch already sits on the new base in a straight line and the upstream's merge base is that base.
fn can_fast_forward(git: &GitHome, plan: &Plan, head: &ObjectId) -> Result<bool> {
    let Some(upstream) = plan.upstream else {
        return Ok(false);
    };
    if plan.restrict.is_some_and(|r| r != plan.onto) {
        return Ok(false);
    }
    let ours = git.merge_bases(&plan.onto, head)?;
    let theirs = git.merge_bases(&upstream, head)?;
    if ours != [plan.onto] || theirs != [plan.onto] {
        return Ok(false);
    }
    let mut at = *head;
    while at != plan.onto {
        match git.repo.commit(&at)?.parents.as_slice() {
            [only] => at = *only,
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn switch_to(git: &GitHome, branch_ref: Option<&str>, id: &ObjectId, plan: &Plan) -> Result<()> {
    let opened = git.open_index()?;
    let from = match git.repo.head_commit()? {
        Some(h) => Some(git.repo.commit(&h)?.tree),
        None => None,
    };
    let to = git.repo.commit(id)?.tree;
    if let Err(blocked) = move_tree(git, &opened, from.as_ref(), Some(&to), Mode::Safe)? {
        blocked.report("checkout", "switch branches", 1);
    }
    let name = branch_ref.map_or_else(|| id.to_hex(), str::to_string);
    git.set_head(
        branch_ref,
        id,
        &format!("{} (start): checkout {name}", plan.action),
    )
}

/// Checks the new base out onto a detached `HEAD`; a local change in the way stops with the autostash put back.
fn detach_at_onto(git: &GitHome, state: &State, plan: &Plan, target: &ObjectId) -> Result<()> {
    let opened = git.open_index()?;
    let from = match git.repo.head_commit()? {
        Some(h) => Some(git.repo.commit(&h)?.tree),
        None => None,
    };
    let to = git.repo.commit(target)?.tree;
    if let Err(blocked) = move_tree(git, &opened, from.as_ref(), Some(&to), Mode::Safe)? {
        if let Some(stash) = &state.autostash {
            restore_autostash(git, stash);
        }
        blocked.report("checkout", "switch branches", 1);
    }
    git.set_head(
        None,
        target,
        &format!("{} (start): checkout {}", plan.action, plan.onto_name),
    )
}

/// The commits after `base` (and `exclude`) that lead to `head`, oldest first, with the ones whose change the base
/// side already has marked.
fn range_of(
    git: &GitHome,
    base: &ObjectId,
    exclude: Option<&ObjectId>,
    head: &ObjectId,
) -> Result<Range> {
    let theirs = git.reachable(base)?;
    let mut skipped = HashSet::new();
    if let Some(e) = exclude {
        skipped = git.reachable(e)?;
    }
    let mine = git.reachable(head)?;
    let mut commits: HashMap<ObjectId, Commit> = HashMap::new();
    for id in mine
        .iter()
        .filter(|id| !theirs.contains(id) && !skipped.contains(id))
    {
        commits.insert(*id, git.repo.commit(id)?);
    }
    let single = |c: &Commit| c.parents.len() <= 1;
    let mut upstream_changes: HashSet<u64> = HashSet::new();
    if commits.values().any(single) {
        for id in theirs.iter().filter(|id| !mine.contains(id)) {
            let c = git.repo.commit(id)?;
            if single(&c) {
                upstream_changes.insert(patch_id(git, &c)?);
            }
        }
    }
    let mut same_patch = HashSet::new();
    if !upstream_changes.is_empty() {
        for (id, c) in &commits {
            if single(c) && upstream_changes.contains(&patch_id(git, c)?) {
                same_patch.insert(*id);
            }
        }
    }
    let order = topo_oldest_first(head, &commits);
    Ok(Range {
        commits,
        order,
        same_patch,
    })
}

/// The commits to replay, oldest first: those after the base, without merges and without the ones whose change
/// the base already has.
fn picks(
    git: &GitHome,
    base: &ObjectId,
    exclude: Option<&ObjectId>,
    head: &ObjectId,
) -> Result<Vec<ObjectId>> {
    let range = range_of(git, base, exclude, head)?;
    let mut kept = Vec::new();
    for id in &range.order {
        let applied = range.same_patch.contains(id) && !todo::Commits::is_empty(git, id)?;
        if range.commits[id].parents.len() <= 1 && !applied {
            kept.push(*id);
        }
    }
    Ok(kept)
}

/// The sheet of `--rebase-merges`: every commit of the range with its merges, and `onto` for the first merge base.
fn merge_items(git: &GitHome, plan: &Plan, head: &ObjectId, cousins: bool) -> Result<Vec<Item>> {
    let base = plan.range_base();
    let range = range_of(git, &base, plan.restrict.as_ref(), head)?;
    let mut bases = git.merge_bases(&base, head)?;
    bases.sort_by_key(|id| std::cmp::Reverse(git.repo.commit(id).map_or(0, |c| c.committer.time)));
    let bottom = bases.first().copied();
    let mode = MergeMode {
        cousins,
        root_with_onto: plan.root && plan.squash_onto.is_none(),
    };
    merges::make_script(git, &range, bottom, &mode)
}

/// Git's `--topo-order --reverse`: a stack walk from the tip that emits a commit once all its children are out, so
/// each line of history stays together; the first parent is walked last.
fn topo_oldest_first(tip: &ObjectId, commits: &HashMap<ObjectId, Commit>) -> Vec<ObjectId> {
    let mut children: HashMap<ObjectId, usize> = HashMap::new();
    for c in commits.values() {
        for p in c.parents.iter().filter(|p| commits.contains_key(p)) {
            *children.entry(*p).or_default() += 1;
        }
    }
    let mut out = Vec::new();
    let mut stack: Vec<ObjectId> = commits
        .contains_key(tip)
        .then_some(*tip)
        .into_iter()
        .collect();
    while let Some(id) = stack.pop() {
        out.push(id);
        for p in commits[&id]
            .parents
            .iter()
            .filter(|p| commits.contains_key(p))
        {
            if let Some(n) = children.get_mut(p) {
                *n -= 1;
                if *n == 0 {
                    stack.push(*p);
                }
            }
        }
    }
    out.reverse();
    out
}

/// A hash of what a commit changes, ignoring where in the file and the whitespace, so equal changes made on
/// different bases collide.
fn patch_id(git: &GitHome, c: &Commit) -> Result<u64> {
    let parent_tree = match c.parents.first() {
        Some(p) => Some(git.repo.commit(p)?.tree),
        None => None,
    };
    let changes = git
        .repo
        .diff_trees(parent_tree.as_ref(), Some(&c.tree), &Pathspec::default())?;
    let mut h = DefaultHasher::new();
    for ch in &changes {
        ch.path.hash(&mut h);
        ch.old.map(|e| raw_mode(e.mode)).hash(&mut h);
        ch.new.map(|e| raw_mode(e.mode)).hash(&mut h);
        let blob = |e: &Option<pyn_gitwork::Entry>| {
            e.and_then(|e| git.repo.blob(&e.id).ok())
                .unwrap_or_default()
        };
        let (old, new) = (blob(&ch.old), blob(&ch.new));
        if pyn_diff::is_binary(&old) || pyn_diff::is_binary(&new) {
            old.hash(&mut h);
            new.hash(&mut h);
            continue;
        }
        let (a, b) = (pyn_diff::split_lines(&old), pyn_diff::split_lines(&new));
        let (mut i, mut j) = (0, 0);
        for op in pyn_diff::diff(&a, &b) {
            match op {
                pyn_diff::Op::Equal => {
                    i += 1;
                    j += 1;
                }
                pyn_diff::Op::Delete => {
                    (b'-', squeeze(a[i])).hash(&mut h);
                    i += 1;
                }
                pyn_diff::Op::Insert => {
                    (b'+', squeeze(b[j])).hash(&mut h);
                    j += 1;
                }
            }
        }
    }
    Ok(h.finish())
}

fn squeeze(line: &[u8]) -> Vec<u8> {
    line.iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect()
}

fn run(git: &GitHome, mut state: State, mut unrun: usize) -> Result<()> {
    require_valid(&state);
    while !state.todo.is_empty() {
        let item = state.todo.remove(0);
        state.done.push(item.clone());
        state.save_progress(git)?;
        if !item.counts() {
            continue;
        }
        if !state.quiet {
            let counted = |items: &[Item]| items.iter().filter(|i| i.counts()).count();
            eprint!(
                "Rebasing ({}/{})\r",
                counted(&state.done),
                counted(&state.done) + counted(&state.todo)
            );
        }
        if unrun > 0 {
            unrun -= 1;
            continue;
        }
        clear_item_state(git);
        if let Err(e) = step(git, &state, &item) {
            if e.is::<Rescheduled>() {
                state.todo.insert(0, item);
                state.put(
                    git,
                    "git-rebase-todo",
                    &todo::sheet(git, &state.todo, false)?,
                )?;
                std::process::exit(1);
            }
            state.done.pop();
            state.todo.insert(0, item);
            state.save_progress(git)?;
            return Err(e);
        }
        match item.id {
            Some(id) if !matches!(item.cmd, Cmd::Drop | Cmd::Edit | Cmd::Merge) => {
                if let Some(head) = git.repo.head_commit()? {
                    rewrite::record(&dir_of(git), &id, &head, more_melds(&state))?;
                }
            }
            _ => {}
        }
    }
    finish(git, &state)
}

/// A command that failed and stays on the sheet, as git reschedules a `label`, `reset` or `merge` that could not run.
#[derive(Debug)]
struct Rescheduled(String);

impl std::fmt::Display for Rescheduled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rescheduled {}

/// What git removes before each command: the previous stop's files and the merge state.
fn clear_item_state(git: &GitHome) {
    let dir = dir_of(git);
    for name in ["message", "author-script", "stopped-sha", "amend"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    for name in ["MERGE_HEAD", "AUTO_MERGE", HEAD_FILE] {
        let _ = std::fs::remove_file(git_dir(git).join(name));
    }
}

fn reschedule(git: &GitHome, item: &Item, why: &anyhow::Error) -> anyhow::Error {
    eprintln!("error: {why}");
    eprintln!("hint: Could not execute the todo command");
    eprintln!("hint: ");
    eprintln!(
        "hint:     {}",
        item.line(git, false).unwrap_or_default().trim_end()
    );
    eprintln!("hint: ");
    eprintln!("hint: It has been rescheduled; To edit the command before continuing, please");
    eprintln!("hint: edit the todo list first:");
    eprintln!("hint: ");
    eprintln!("hint:     pyn rebase --edit-todo");
    eprintln!("hint:     pyn rebase --continue");
    anyhow::Error::new(Rescheduled(why.to_string()))
}

fn step(git: &GitHome, state: &State, item: &Item) -> Result<()> {
    match item.cmd {
        Cmd::Noop | Cmd::Drop | Cmd::Comment => Ok(()),
        Cmd::Break => {
            let head = git
                .repo
                .head_commit()?
                .ok_or_else(|| anyhow!("the rebase has no HEAD"))?;
            let subject = subject_of(&git.repo.commit(&head)?.message);
            eprintln!("Stopped at {} ({subject})", short(&head));
            std::process::exit(0);
        }
        Cmd::Exec => exec(git, &item.arg),
        Cmd::Label => {
            merges::do_label(git, &dir_of(git), &item.arg).map_err(|e| reschedule(git, item, &e))
        }
        Cmd::Reset => do_reset(git, state, &item.arg).map_err(|e| reschedule(git, item, &e)),
        Cmd::UpdateRef => merges::do_update_ref(git, &dir_of(git), &item.arg)
            .map_err(|e| reschedule(git, item, &e)),
        Cmd::Merge => {
            *state.reflog_env.borrow_mut() = format!("{} (merge)", state.action);
            do_merge(git, state, item)
        }
        _ => {
            *state.reflog_env.borrow_mut() = format!("{} ({})", state.action, item.cmd.word());
            replay(git, state, item)
        }
    }
}

/// Runs an `exec` line in the work tree root; a failure, or a dirty tree afterwards, stops the rebase.
fn exec(git: &GitHome, line: &str) -> Result<()> {
    eprintln!("Executing: {line}");
    let ran = std::process::Command::new("sh")
        .arg("-c")
        .arg(line)
        .current_dir(&git.ws.root)
        .status();
    if !ran.is_ok_and(|s| s.success()) {
        eprintln!("warning: execution failed: {line}");
        eprintln!("You can fix the problem, and then run\n\n  pyn rebase --continue\n");
        std::process::exit(1);
    }
    if unclean(git)?.is_some() {
        eprintln!(
            "error: execution succeeded: {line}\nbut left changes to the index and/or the working tree."
        );
        eprintln!("Commit or stash your changes, and run\n\n  pyn rebase --continue\n");
        std::process::exit(1);
    }
    Ok(())
}

/// Replays one commit: a fast-forward, a new commit, nothing when its change is already there, or a stop.
fn replay(git: &GitHome, state: &State, item: &Item) -> Result<()> {
    let id = &item
        .id
        .ok_or_else(|| anyhow!("a command without a commit"))?;
    let Merged {
        picked,
        ours_tree,
        applied,
        ..
    } = merge_pick(git, Verb::Pick, id, None)?;
    let subject = subject_of(&picked.message);
    let conflicted = &applied.conflicted;
    if item.cmd.melds() {
        return meld_in(
            git,
            state,
            item,
            &picked,
            ours_tree,
            &applied.index,
            conflicted,
        );
    }
    if !conflicted.is_empty() {
        stop(git, state, id, &picked, conflicted, &subject, None)?;
    }
    let tree = write_tree(git, &applied.index)?;
    let head = git.repo.head_commit()?;
    let parent_tree = match picked.parents.first() {
        Some(p) => Some(git.repo.commit(p)?.tree),
        None => None,
    };
    let was_empty = parent_tree == Some(picked.tree);
    if tree == ours_tree && !was_empty {
        if state.stop_empty {
            stop_with(git, state, id, &picked, &[], &subject, None, Why::Empty)?;
        }
        eprintln!("dropping {id} {subject} -- patch contents already upstream");
        return Ok(());
    }
    let reflog = format!("{} ({})", state.action, item.cmd.word());
    let at_new_root = head.is_some() && head == state.squash_onto.get();
    let fast = !state.no_ff && lands_on_head(&picked, head, at_new_root);
    let message = if item.cmd == Cmd::Reword {
        match reworded(git, state, &picked.message) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("error: {e}");
                if fast {
                    move_forward(git, state, id)?;
                    stop_to_edit(git, id, &picked, true)?;
                }
                commit_here(
                    state,
                    git,
                    &Pending {
                        tree,
                        ours_tree,
                        author: picked.author.clone(),
                        message: picked.message.clone(),
                        reflog,
                    },
                )?;
                stop_with(git, state, id, &picked, &[], &subject, None, Why::Message)?;
                unreachable!("stop exits")
            }
        }
    } else {
        picked.message.clone()
    };
    if fast && item.cmd != Cmd::Reword {
        move_forward(git, state, id)?;
    } else {
        commit_here(
            state,
            git,
            &Pending {
                tree,
                ours_tree,
                author: picked.author.clone(),
                message,
                reflog,
            },
        )?;
    }
    if item.cmd == Cmd::Edit {
        stop_to_edit(git, id, &picked, false)?;
    }
    Ok(())
}

/// `reset <label>`: `HEAD` and the work tree go to the commit the label names; `[new root]` is an empty commit.
fn do_reset(git: &GitHome, state: &State, arg: &str) -> Result<()> {
    let name = arg.split_whitespace().next().unwrap_or(arg);
    let target = if arg.starts_with("[new root]") {
        match state.squash_onto.get() {
            Some(empty) => empty,
            None => {
                let empty = make_squash_onto(git)?;
                state.squash_onto.set(Some(empty));
                state.put(git, "squash-onto", &format!("{empty}\n"))?;
                empty
            }
        }
    } else {
        merges::lookup_label(git, name)?.0
    };
    let shown = if arg.starts_with("[new root]") {
        arg
    } else {
        name
    };
    let message = format!("{} (reset): '{shown}'", state.action);
    *state.reflog_env.borrow_mut() = message.clone();
    move_to(git, &target, &message, Mode::Force)
}

/// Checks `target` out onto the detached `HEAD`, logging the move under `message`.
fn move_to(git: &GitHome, target: &ObjectId, message: &str, mode: Mode) -> Result<()> {
    let head = git.repo.head_commit()?;
    let opened = git.open_index()?;
    let from = match head {
        Some(h) => Some(git.repo.commit(&h)?.tree),
        None => None,
    };
    let to = git.repo.commit(target)?.tree;
    if let Err(blocked) = move_tree(git, &opened, from.as_ref(), Some(&to), mode)? {
        blocked.report("checkout", "switch branches", 1);
    }
    if head != Some(*target) {
        git.move_head(target, message)?;
    }
    Ok(())
}

fn fast_forward(git: &GitHome, state: &State, to: &ObjectId) -> Result<()> {
    move_to(
        git,
        to,
        &format!("{}: fast-forward", state.action),
        Mode::Safe,
    )
}

/// The labels of a `merge` line, the text up to the last of them, and the oneline after a `#`.
fn split_merge_arg(arg: &str) -> (Vec<&str>, &str, Option<&str>) {
    let mut labels = Vec::new();
    let mut end = 0;
    let mut at = 0;
    let mut oneline = None;
    while at < arg.len() {
        at += arg[at..].len() - arg[at..].trim_start().len();
        let rest = &arg[at..];
        if rest.is_empty() {
            break;
        }
        let word = rest.split_whitespace().next().unwrap_or(rest);
        if word == "#" {
            oneline = Some(rest[1..].trim_start());
            break;
        }
        labels.push(word);
        at += word.len();
        end = at;
    }
    (labels, &arg[..end], oneline)
}

/// `merge [-C|-c <commit>] <label>... [# <oneline>]`: recreates the merge on `HEAD`, or fast-forwards to the original
/// when nothing changed. A conflict stops the rebase with the merge in progress.
fn do_merge(git: &GitHome, state: &State, item: &Item) -> Result<()> {
    let fail = |why: String| {
        if let Some(id) = item.id {
            let _ = write_state(git, HEAD_FILE, &format!("{id}\n"));
        }
        reschedule(git, item, &anyhow!(why))
    };
    let Some(head_id) = git.repo.head_commit()? else {
        return Err(fail("cannot merge without a current revision".into()));
    };
    let (labels, merge_arg, oneline) = split_merge_arg(&item.arg);
    let mut heads: Vec<(ObjectId, String)> = Vec::new();
    for label in &labels {
        match merges::lookup_label(git, label) {
            Ok(found) => heads.push(found),
            Err(_) => return Err(fail(format!("unable to parse '{label}'"))),
        }
    }
    if heads.is_empty() {
        return Err(fail(format!("nothing to merge: '{}'", item.arg)));
    }
    let original = match item.id {
        Some(id) => Some((id, git.repo.commit(&id)?)),
        None => None,
    };
    let record = |git: &GitHome| -> Result<()> {
        if let (Some((id, _)), Some(head)) = (&original, git.repo.head_commit()?) {
            rewrite::record(&dir_of(git), id, &head, false)?;
        }
        Ok(())
    };
    if state.squash_onto.get() == Some(head_id) {
        if heads.len() > 1 {
            return Err(fail(
                "octopus merge cannot be executed on top of a [new root]".into(),
            ));
        }
        fast_forward(git, state, &heads[0].0)?;
        return record(git);
    }
    let can_ff = !state.no_ff
        && original.as_ref().is_some_and(|(_, c)| {
            c.parents.first() == Some(&head_id)
                && heads.iter().zip(&c.parents[1..]).all(|(h, p)| h.0 == *p)
        });
    if can_ff {
        let (id, commit) = original.as_ref().expect("a fast-forward has an original");
        fast_forward(git, state, id)?;
        if item.flag == Flag::Edit {
            amend_merge(git, state, item, commit)?;
        }
        return record(git);
    }
    if heads.len() > 1 {
        return Err(fail(
            "octopus merges are not built; merge the branches one at a time".into(),
        ));
    }
    let author = match &original {
        Some((_, c)) => c.author.clone(),
        None => super::stage::identity(git, "author")?,
    };
    state.put(git, "author-script", &author_script(&author))?;
    let message = match (&original, oneline) {
        (Some((_, c)), _) => c.message.clone(),
        (None, Some(line)) => line.as_bytes().to_vec(),
        (None, None) => format!("Merge branch '{merge_arg}'").into_bytes(),
    };
    write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&message))?;
    let (theirs, theirs_name) = &heads[0];
    let bases = git.merge_bases(&head_id, theirs)?;
    if bases.first() == Some(theirs) {
        return record(git);
    }
    write_state(git, "MERGE_HEAD", &theirs.to_hex())?;
    write_state(git, "MERGE_MODE", "no-ff")?;
    if bases.len() > 1 {
        return Err(fail(
            "the histories have more than one merge base (a criss-cross merge); pyn cannot merge them yet".into(),
        ));
    }
    let tree_of = |c: &ObjectId| -> Result<ObjectId> { Ok(git.repo.commit(c)?.tree) };
    let base_tree = match bases.first() {
        Some(b) => Some(tree_of(b)?),
        None => None,
    };
    let opened = git.open_index()?;
    let ours_tree = write_tree(git, &opened.index)?;
    let theirs_tree = tree_of(theirs)?;
    let incoming =
        git.repo
            .diff_trees(base_tree.as_ref(), Some(&theirs_tree), &Pathspec::default())?;
    super::convert::refuse_conversions(
        git,
        incoming
            .iter()
            .filter_map(|c| c.new.map(|e| (c.path.as_slice(), e.mode))),
    )?;
    let base_label = bases.first().map_or_else(String::new, short);
    let merged = merge_trees(
        git,
        base_tree.as_ref(),
        Some(&ours_tree),
        Some(&theirs_tree),
        &Labels {
            ours: "HEAD",
            theirs: theirs_name,
            base: &base_label,
        },
        &Settings::default(),
    )?;
    let landing = Landing {
        theirs: theirs_name,
        orig_head: None,
        strict_index: false,
    };
    let applied = match apply_merge(git, &opened, Some(&ours_tree), &merged, &landing)? {
        Ok(applied) => applied,
        Err(refusal) => {
            refusal.blocked.print("merge", "merge");
            return Err(fail("Aborting".into()));
        }
    };
    if !applied.conflicted.is_empty() {
        write_state(
            git,
            "AUTO_MERGE",
            &format!("{}\n", result_tree(git, &merged)?),
        )?;
        record(git)?;
        return stop_merge(git, state, item, &message, &applied.conflicted);
    }
    let tree = write_tree(git, &applied.index)?;
    let id = make_commit_on(
        git,
        &Pending {
            tree,
            ours_tree,
            author,
            message: message.clone(),
            reflog: state.reflog_env.borrow().clone(),
        },
        vec![head_id, *theirs],
    )?;
    clear_merge_files(git);
    if item.flag == Flag::Edit {
        amend_merge(git, state, item, &git.repo.commit(&id)?)?;
    }
    record(git)
}

/// Lets the user reword the merge commit `HEAD` is on; an empty message stops the rebase there.
fn amend_merge(git: &GitHome, state: &State, item: &Item, commit: &Commit) -> Result<()> {
    let comment = rebase_comment(git, state);
    let Some(message) = editor::edit_message(git, &commit.message, &comment)? else {
        eprintln!("Aborting commit due to empty commit message.");
        return stop_merge(git, state, item, &commit.message, &[]);
    };
    if message == commit.message {
        return Ok(());
    }
    make_commit_on(
        git,
        &Pending {
            tree: commit.tree,
            ours_tree: commit.tree,
            author: commit.author.clone(),
            message,
            reflog: state.reflog_env.borrow().clone(),
        },
        commit.parents.clone(),
    )?;
    Ok(())
}

fn clear_merge_files(git: &GitHome) {
    for name in ["MERGE_HEAD", "MERGE_MSG", "MERGE_MODE", "AUTO_MERGE"] {
        let _ = std::fs::remove_file(git_dir(git).join(name));
    }
}

/// Leaves the state git expects for a merge that stopped on conflicts (or on a message that could not be made) and
/// ends the command with status 1.
fn stop_merge(
    git: &GitHome,
    state: &State,
    item: &Item,
    message: &[u8],
    conflicted: &[Vec<u8>],
) -> Result<()> {
    let mut text = message.to_vec();
    if !conflicted.is_empty() {
        if !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        text.extend_from_slice(b"\n# Conflicts:\n");
        for p in conflicted {
            text.extend_from_slice(format!("#\t{}\n", path_string(p)).as_bytes());
        }
    }
    write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&text))?;
    let mut copy = text.clone();
    copy.push(b'\n');
    state.put(git, "message", &String::from_utf8_lossy(&copy))?;
    if let Some(id) = item.id {
        state.put(git, "stopped-sha", &format!("{id}\n"))?;
        state.put(git, "patch", "")?;
        write_state(git, HEAD_FILE, &format!("{id}\n"))?;
        let subject = &item.arg;
        eprintln!("Could not apply {}... {subject}", short(&id));
    } else {
        eprintln!("error: could not apply {}", item.arg);
    }
    std::process::exit(1);
}

/// Fast-forwards `HEAD` to a replayed commit; a `HEAD` that is already there is not logged.
fn move_forward(git: &GitHome, state: &State, id: &ObjectId) -> Result<()> {
    if git.repo.head_commit()? == Some(*id) {
        return Ok(());
    }
    git.move_head(id, &format!("{}: fast-forward", state.action))
}

/// Whether the commit already sits on `HEAD` (or is a root and `HEAD` is the empty commit of a new root).
fn lands_on_head(picked: &Commit, head: Option<ObjectId>, at_new_root: bool) -> bool {
    match picked.parents.as_slice() {
        [parent] => head == Some(*parent),
        [] => at_new_root,
        _ => false,
    }
}

/// Commits on top of `HEAD`; on the empty commit of a new root the commit has no parent.
fn commit_here(state: &State, git: &GitHome, pending: &Pending) -> Result<ObjectId> {
    let parents = match git.repo.head_commit()? {
        Some(h) if state.squash_onto.get() != Some(h) => vec![h],
        _ => Vec::new(),
    };
    make_commit_on(git, pending, parents)
}

/// The message the user gives a reworded commit.
fn reworded(git: &GitHome, state: &State, message: &[u8]) -> Result<Vec<u8>> {
    editor::edit_message(git, message, &rebase_comment(git, state))?
        .ok_or_else(|| anyhow!("Aborting commit due to empty commit message."))
}

/// The `#` lines under a message in the editor: where the rebase is, as git shows it.
fn rebase_comment(git: &GitHome, state: &State) -> String {
    let line = |item: &Item| item.line(git, true).unwrap_or_default();
    let list =
        |items: &[Item]| -> String { items.iter().map(|i| format!("#    {}", line(i))).collect() };
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut text = format!(
        "#\n# interactive rebase in progress; onto {}\n",
        &state.onto.to_hex()[..7]
    );
    let done = state.done.len();
    let from = done.saturating_sub(2);
    text.push_str(&format!(
        "# Last command{} done ({done} command{} done):\n{}",
        plural(done),
        plural(done),
        list(&state.done[from..])
    ));
    let rest = state.todo.len();
    if rest == 0 {
        text.push_str("# No commands remaining.\n");
    } else {
        text.push_str(&format!(
            "# Next command{} to do ({rest} remaining command{}):\n{}",
            plural(rest),
            plural(rest),
            list(&state.todo[..rest.min(2)])
        ));
    }
    let branch = state
        .head_name
        .strip_prefix("refs/heads/")
        .unwrap_or("HEAD");
    text.push_str(&format!(
        "# You are currently rebasing branch '{branch}' on '{}'.\n#\n",
        &state.onto.to_hex()[..7]
    ));
    text
}

/// A `squash` or `fixup`: the commit's change goes into the commit `HEAD` is on, the messages are collected, and
/// the last of a chain makes the commit.
fn meld_in(
    git: &GitHome,
    state: &State,
    item: &Item,
    picked: &Commit,
    ours_tree: ObjectId,
    index: &pyn_git::Index,
    conflicted: &[Vec<u8>],
) -> Result<()> {
    let id = &item
        .id
        .ok_or_else(|| anyhow!("a command without a commit"))?;
    let head_id = git
        .repo
        .head_commit()?
        .ok_or_else(|| anyhow!("the rebase has no HEAD"))?;
    let head = git.repo.commit(&head_id)?;
    let text = meld::add(
        &dir_of(git),
        &String::from_utf8_lossy(&head.message),
        &String::from_utf8_lossy(&picked.message),
        item.cmd,
        item.flag,
        id,
    )?;
    let subject = subject_of(&picked.message);
    if !conflicted.is_empty() {
        let kept = Some((text.clone(), head_id));
        stop(git, state, id, picked, conflicted, &subject, kept)?;
    }
    let tree = write_tree(git, index)?;
    let message = match combined_message(git, state, &text, false) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {e}");
            stop(git, state, id, picked, &[], &subject, Some((text, head_id)))?;
            unreachable!("stop exits")
        }
    };
    commit_over(git, state, &head, tree, ours_tree, message, item.cmd.word())?;
    if !more_melds(state) {
        meld::clear(&dir_of(git));
    }
    Ok(())
}

fn more_melds(state: &State) -> bool {
    state.todo.first().is_some_and(|i| i.cmd.melds())
}

/// The message of the commit a chain makes now: the collected text while more members follow, then the first
/// commit's own message when only `fixup` joined it, else what the user makes of the collected text. `resumed`
/// (after a stop) always opens the editor, as git does.
fn combined_message(git: &GitHome, state: &State, text: &str, resumed: bool) -> Result<Vec<u8>> {
    if more_melds(state) {
        return Ok(text.as_bytes().to_vec());
    }
    if !resumed && let Ok(only) = std::fs::read(dir_of(git).join(meld::FIXUP)) {
        return Ok(only);
    }
    editor::edit_message(git, text.as_bytes(), &rebase_comment(git, state))?
        .ok_or_else(|| anyhow!("Aborting commit due to empty commit message."))
}

/// Replaces the commit `HEAD` is on by one with `tree` and `message`, keeping its parents and author.
fn commit_over(
    git: &GitHome,
    state: &State,
    head: &Commit,
    tree: ObjectId,
    ours_tree: ObjectId,
    message: Vec<u8>,
    word: &str,
) -> Result<()> {
    make_commit_on(
        git,
        &Pending {
            tree,
            ours_tree,
            author: head.author.clone(),
            message,
            reflog: format!("{} ({word})", state.action),
        },
        head.parents.clone(),
    )?;
    Ok(())
}

/// Leaves the state git expects for an `edit` (status 0), or for a reworded commit whose message could not be made
/// (status 1), with the commit already on `HEAD`.
fn stop_to_edit(git: &GitHome, id: &ObjectId, picked: &Commit, failed: bool) -> Result<()> {
    let Some(head) = git.repo.head_commit()? else {
        bail!("the rebase has no HEAD");
    };
    let put = |name: &str, text: &str| write_state(git, &format!("{DIR}/{name}"), text);
    put("stopped-sha", &format!("{id}\n"))?;
    put("amend", &format!("{head}\n"))?;
    put(
        "message",
        &format!("{}\n", String::from_utf8_lossy(&picked.message)),
    )?;
    put("author-script", &author_script(&picked.author))?;
    put("patch", &patch_file::of_commit(git, id)?)?;
    write_state(git, HEAD_FILE, &format!("{id}\n"))?;
    if !failed {
        eprintln!(
            "Stopped at {id7}...  {}",
            subject_of(&picked.message),
            id7 = short(id)
        );
    }
    eprintln!(
        "You can amend the commit now, with\n\n  pyn commit --amend\n\nOnce you are satisfied with your changes, run\n\n  pyn rebase --continue"
    );
    std::process::exit(i32::from(failed));
}

/// Leaves the state git expects for a pick that stopped (on conflicts, on a commit that became empty, or on a
/// message that could not be made) and ends the command with status 1. `meld` is the collected message and the
/// commit being replaced.
fn stop(
    git: &GitHome,
    state: &State,
    id: &ObjectId,
    picked: &Commit,
    conflicted: &[Vec<u8>],
    subject: &str,
    meld: Option<(String, ObjectId)>,
) -> Result<()> {
    stop_with(
        git,
        state,
        id,
        picked,
        conflicted,
        subject,
        meld,
        Why::Conflict,
    )
}

#[derive(PartialEq, Eq)]
enum Why {
    Conflict,
    Empty,
    Message,
}

#[allow(clippy::too_many_arguments)]
fn stop_with(
    git: &GitHome,
    state: &State,
    id: &ObjectId,
    picked: &Commit,
    conflicted: &[Vec<u8>],
    subject: &str,
    meld: Option<(String, ObjectId)>,
    why: Why,
) -> Result<()> {
    state.put(git, "stopped-sha", &format!("{id}\n"))?;
    let message = match &meld {
        Some((text, _)) => text.clone().into_bytes(),
        None => {
            let mut m = picked.message.clone();
            m.push(b'\n');
            m
        }
    };
    state.put(git, "message", &String::from_utf8_lossy(&message))?;
    state.put(git, "author-script", &author_script(&picked.author))?;
    state.put(git, "patch", &patch_file::of_commit(git, id)?)?;
    write_state(git, HEAD_FILE, &format!("{id}\n"))?;
    let mut text = match &meld {
        Some((text, head)) => {
            state.put(git, "amend", &format!("{head}\n"))?;
            if !dir_of(git).join(meld::FIXUP).exists() {
                write_state(git, "SQUASH_MSG", text)?;
            }
            text.clone().into_bytes()
        }
        None => picked.message.clone(),
    };
    if meld.is_none() && !conflicted.is_empty() {
        if !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        text.extend_from_slice(b"\n# Conflicts:\n");
        for p in conflicted {
            text.extend_from_slice(format!("#\t{}\n", path_string(p)).as_bytes());
        }
    }
    if why != Why::Message || meld.is_some() {
        write_state(git, "MERGE_MSG", &String::from_utf8_lossy(&text))?;
    }
    if why == Why::Empty {
        eprintln!("The previous cherry-pick is now empty, possibly due to conflict resolution.");
        eprintln!("If you wish to commit it anyway, use:\n\n    pyn commit --allow-empty\n");
        eprintln!("Otherwise, please use 'pyn rebase --skip'");
        std::process::exit(1);
    }
    if conflicted.is_empty() {
        eprintln!("hint: Fix the message problem, then run \"pyn rebase --continue\".");
        std::process::exit(1);
    }
    let (id7, cmd) = (short(id), "pyn rebase");
    eprintln!("error: could not apply {id7}... {subject}");
    eprintln!("hint: Resolve all conflicts manually, mark them as resolved with");
    eprintln!("hint: \"pyn add/rm <conflicted_files>\", then run \"{cmd} --continue\".");
    eprintln!("hint: You can instead skip this commit: run \"{cmd} --skip\".");
    eprintln!("hint: To abort and get back to the state before \"{cmd}\", run \"{cmd} --abort\".");
    eprintln!("Could not apply {id7}... {subject}");
    std::process::exit(1);
}

fn quoted(text: &[u8]) -> String {
    format!("'{}'", String::from_utf8_lossy(text).replace('\'', "'\\''"))
}

fn author_script(who: &Signature) -> String {
    let (sign, minutes) = if who.offset_minutes < 0 {
        ('-', -who.offset_minutes)
    } else {
        ('+', who.offset_minutes)
    };
    format!(
        "GIT_AUTHOR_NAME={}\nGIT_AUTHOR_EMAIL={}\nGIT_AUTHOR_DATE='@{} {sign}{:02}{:02}'\n",
        quoted(&who.name),
        quoted(&who.email),
        who.time,
        minutes / 60,
        minutes % 60
    )
}

fn parse_author_script(text: &str) -> Option<Signature> {
    let (mut name, mut email, mut date) = (None, None, None);
    for line in text.lines() {
        let (key, value) = line.split_once('=')?;
        let value = value
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
            .unwrap_or(value)
            .replace("'\\''", "'");
        match key {
            "GIT_AUTHOR_NAME" => name = Some(value),
            "GIT_AUTHOR_EMAIL" => email = Some(value),
            "GIT_AUTHOR_DATE" => date = Some(value),
            _ => {}
        }
    }
    let (time, offset_minutes) = parse_date(&date?).ok()?;
    Some(Signature {
        name: name?.into_bytes(),
        email: email?.into_bytes(),
        time,
        offset_minutes,
    })
}

/// Moves the branch to where the replay ended and reattaches `HEAD`.
fn finish(git: &GitHome, state: &State) -> Result<()> {
    let Some(end) = git.repo.head_commit()? else {
        bail!("the rebase has no HEAD");
    };
    let action = state.action;
    if state.head_name.starts_with("refs/heads/") {
        let old = git.repo.refs().resolve(&state.head_name)?;
        git.move_ref(
            &state.head_name,
            old.as_ref(),
            &end,
            &format!("{action} (finish): {} onto {}", state.head_name, state.onto),
        )?;
        git.set_head(
            Some(&state.head_name),
            &end,
            &format!("{action} (finish): returning to {}", state.head_name),
        )?;
    }
    write_orig_head_as(git, &state.orig_head, Some("rebase"))?;
    let _ = rewrite::flush(&dir_of(git), &end);
    let ref_report = merges::update_refs(git, &dir_of(git), state.quiet);
    let _ = rewrite::copy_notes(git, &dir_of(git));
    clear_state(git);
    if !state.quiet {
        let what = if state.head_name.starts_with("refs/") {
            state.head_name.as_str()
        } else {
            "detached HEAD"
        };
        eprintln!("Successfully rebased and updated {what}.");
    }
    for line in &ref_report {
        eprintln!("{line}");
    }
    if let Some(stash) = &state.autostash {
        restore_autostash(git, stash);
    }
    Ok(())
}

/// Applies the autostash; when that does not go cleanly the changes are kept on `refs/stash`.
fn restore_autostash(git: &GitHome, stash: &ObjectId) {
    match apply_commit(git, stash) {
        Ok(Applying::Clean) => eprintln!("Applied autostash."),
        _ => {
            if store_stash(git, stash, "autostash").is_err() {
                eprintln!("error: could not store the autostash {stash}");
                return;
            }
            eprintln!("Applying autostash resulted in conflicts.");
            eprintln!("Your changes are safe in the stash.");
            eprintln!("You can run \"pyn stash pop\" or \"pyn stash drop\" at any time.");
        }
    }
}

fn cont(git: &GitHome) -> Result<()> {
    let state = load(git)?;
    require_valid(&state);
    if state.done.is_empty() {
        state.put(git, "end", &format!("{}\n", state.todo.len()))?;
        refuse_if_dropped(git, &state)?;
    }
    let opened = git.open_index()?;
    let unfinished = opened.index.entries.iter().any(|e| e.stage > 0)
        || git
            .repo
            .unstaged_changes(&opened.index)?
            .values()
            .any(|k| matches!(k, Kind::Modified | Kind::Deleted | Kind::TypeChange));
    if unfinished {
        println!("You must edit all merge conflicts and then\nmark them as resolved using pyn add");
        std::process::exit(1);
    }
    let dir = dir_of(git);
    let Some(head_id) = git.repo.head_commit()? else {
        bail!("the rebase has no HEAD");
    };
    let head = git.repo.commit(&head_id)?;
    let tree = write_tree(git, &opened.index)?;
    let stopped = read_id(git, &dir.join("stopped-sha"))?;
    let merging = !merge_heads(git)?.is_empty();
    let amending = dir.join("amend").exists();
    let chain = dir.join(meld::SQUASH).exists();
    if amending && chain {
        if tree != head.tree || !more_melds(&state) {
            let text = read_text(&dir.join(meld::SQUASH)).unwrap_or_default();
            let message = combined_message(git, &state, &text, true)?;
            commit_over(git, &state, &head, tree, head.tree, message, "continue")?;
        }
        if !more_melds(&state) {
            meld::clear(&dir);
        }
    } else if amending {
        if tree != head.tree {
            let comment = rebase_comment(git, &state);
            let Some(message) = editor::edit_message(git, &head.message, &comment)? else {
                bail!("Aborting commit due to empty commit message.");
            };
            commit_over(git, &state, &head, tree, head.tree, message, "continue")?;
        }
    } else if stopped.is_some() || merging {
        if tree != head.tree {
            let picked = match stopped {
                Some(id) => Some(git.repo.commit(&id)?),
                None => None,
            };
            let mut message = match (read_text(&dir.join("message")), &picked) {
                (Some(m), _) => {
                    let kept: Vec<&str> = m.lines().filter(|l| !l.starts_with('#')).collect();
                    clean_message(&[kept.join("\n")])
                }
                (None, Some(p)) => p.message.clone(),
                (None, None) => Vec::new(),
            };
            let comment = rebase_comment(git, &state);
            message = editor::edit_message(git, &message, &comment)?
                .ok_or_else(|| anyhow!("Aborting commit due to empty commit message."))?;
            let author =
                match read_text(&dir.join("author-script")).and_then(|t| parse_author_script(&t)) {
                    Some(a) => a,
                    None => match &picked {
                        Some(p) => p.author.clone(),
                        None => super::stage::identity(git, "author")?,
                    },
                };
            let parents = if merging {
                let mut all = vec![head_id];
                all.extend(merge_heads(git)?);
                all
            } else if state.squash_onto.get() == Some(head_id) {
                Vec::new()
            } else {
                vec![head_id]
            };
            make_commit_on(
                git,
                &Pending {
                    tree,
                    ours_tree: head.tree,
                    author,
                    message,
                    reflog: "rebase (continue)".into(),
                },
                parents,
            )?;
            if merging {
                let _ = std::fs::remove_file(git_dir(git).join("MERGE_MODE"));
            }
        }
    } else if tree != head.tree {
        refuse(
            &[
                "error: you have staged changes in your working tree",
                "hint: commit them with 'pyn commit', or discard them, then run 'pyn rebase --continue' again.",
            ],
            1,
        );
    }
    if let Some(stopped) = stopped
        && let Some(end) = git.repo.head_commit()?
    {
        rewrite::record(&dir, &stopped, &end, more_melds(&state))?;
    }
    clear_stop(git);
    run(git, state, 0)
}

/// Removes what a stopped command left behind.
fn clear_stop(git: &GitHome) {
    let dir = dir_of(git);
    for name in ["stopped-sha", "message", "author-script", "amend", "patch"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    for name in [
        HEAD_FILE,
        "MERGE_MSG",
        "SQUASH_MSG",
        "MERGE_HEAD",
        "AUTO_MERGE",
    ] {
        let _ = std::fs::remove_file(git_dir(git).join(name));
    }
}

fn skip(git: &GitHome) -> Result<()> {
    let state = load(git)?;
    require_valid(&state);
    hard_reset(git, None)?;
    write_orig_head_as(git, &state.orig_head, Some("rebase"))?;
    let dir = dir_of(git);
    let stopped = read_id(git, &dir.join("stopped-sha"))?;
    let skipped_meld =
        dir.join("amend").exists() && state.done.last().is_some_and(|i| i.cmd.melds());
    if skipped_meld {
        meld::drop_last(&dir);
        if dir.join(meld::SQUASH).exists() && !more_melds(&state) {
            let head_id = git
                .repo
                .head_commit()?
                .ok_or_else(|| anyhow!("the rebase has no HEAD"))?;
            let head = git.repo.commit(&head_id)?;
            let text = read_text(&dir.join(meld::SQUASH)).unwrap_or_default();
            let message = combined_message(git, &state, &text, false)?;
            commit_over(
                git, &state, &head, head.tree, head.tree, message, "continue",
            )?;
            meld::clear(&dir);
        }
    }
    if let Some(stopped) = stopped
        && let Some(end) = git.repo.head_commit()?
    {
        rewrite::record(&dir, &stopped, &end, more_melds(&state))?;
    }
    clear_stop(git);
    run(git, state, 0)
}

fn hard_reset(git: &GitHome, rev: Option<&ObjectId>) -> Result<()> {
    let rev = rev.map(ObjectId::to_hex);
    reset::reset(git, &git.ws.root, &ResetArgs::quiet_hard(rev.as_deref()))
}

fn silent_hard_reset(git: &GitHome, rev: &ObjectId) -> Result<()> {
    let rev = rev.to_hex();
    reset::reset_silently(git, &git.ws.root, &ResetArgs::quiet_hard(Some(&rev)))
}

fn abort(git: &GitHome) -> Result<()> {
    let state = load(git)?;
    let before = git.repo.head_commit()?;
    silent_hard_reset(git, &state.orig_head)?;
    write_orig_head_as(git, &state.orig_head, Some("rebase"))?;
    if state.head_name.starts_with("refs/heads/") {
        let old = git.repo.refs().resolve(&state.head_name)?;
        if old != Some(state.orig_head) {
            git.move_ref(
                &state.head_name,
                old.as_ref(),
                &state.orig_head,
                &format!("{} (abort): {}", state.action, state.head_name),
            )?;
        }
        git.set_head_from(
            before.as_ref(),
            Some(&state.head_name),
            &state.orig_head,
            &format!("{} (abort): returning to {}", state.action, state.head_name),
        )?;
    }
    clear_state(git);
    if let Some(stash) = &state.autostash {
        restore_autostash(git, stash);
    }
    Ok(())
}

fn quit(git: &GitHome) -> Result<()> {
    let state = load(git)?;
    merges::delete_rewritten(git, &dir_of(git));
    std::fs::remove_dir_all(dir_of(git))?;
    if let Some(stash) = &state.autostash {
        store_stash(git, stash, "autostash")?;
        eprintln!("Autostash exists; creating a new stash entry.");
        eprintln!("Your changes are safe in the stash.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_line_splits_into_labels_and_oneline() {
        assert_eq!(
            split_merge_arg("topic  other # Merge x"),
            (vec!["topic", "other"], "topic  other", Some("Merge x"))
        );
        assert_eq!(split_merge_arg("a"), (vec!["a"], "a", None));
        assert_eq!(
            split_merge_arg("# only"),
            (Vec::<&str>::new(), "", Some("only"))
        );
    }

    #[test]
    fn the_author_script_round_trips() {
        let who = Signature {
            name: b"O'Neil".to_vec(),
            email: b"o@example.com".to_vec(),
            time: 1_700_000_000,
            offset_minutes: -270,
        };
        let back = parse_author_script(&author_script(&who)).unwrap();
        assert_eq!(back.name, who.name);
        assert_eq!(back.email, who.email);
        assert_eq!(back.time, who.time);
        assert_eq!(back.offset_minutes, who.offset_minutes);
    }
}

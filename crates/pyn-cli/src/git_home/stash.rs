//! `pyn stash` on a git repository: a stash is a commit on `refs/stash` (the work tree, with the index commit and
//! an optional untracked-files commit as further parents), and `logs/refs/stash` is the list, as git keeps them.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use pyn_git::{Expect, Index, IndexEntry, ReflogEntry};
use pyn_gitwork::{Entry, Head, Kind, Pathspec, fs_path, hash_worktree_file};
use pyn_object::{Commit, FileMode, Kind as ObjectKind, ObjectId};

use super::patch_mode::{self, StashPatch};
use super::stage::{
    Entries, OpenIndex, Specs, entries_of, identity, index_of, put_entry, raw_mode,
    refuse_in_progress, remove_entry, resolve_specs, stage_file, stat_fields, under,
    untracked_files, write_tree,
};
use super::strategy::Settings;
use super::switch::{subject, switch_to_new};
use super::treemerge::{Labels, Merge, Resolved, Worktree, merge_trees};
use super::worktree::{
    Blocked, Mode, check_writes, in_the_way, move_tree, remove_entry_file, write_bytes, write_entry,
};
use super::{GitHome, Refused, convert, path_string};

const STASH: &str = "refs/stash";
const ABBREV: usize = 7;
const INDEX_CONFLICT: &str = "conflicts in index. Try without --index.";

/// Save, list and restore local changes.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct StashArgs {
    #[command(subcommand)]
    action: Option<StashAction>,
    #[command(flatten)]
    push: PushArgs,
}

#[derive(Debug, Args, Default)]
pub(super) struct PushArgs {
    /// Stash untracked files too (they are removed from the work tree).
    #[arg(short = 'u', long)]
    pub(super) include_untracked: bool,
    /// The stash's message.
    #[arg(short = 'm', long)]
    pub(super) message: Option<String>,
    /// Keep the staged changes in the index and work tree.
    #[arg(short = 'k', long)]
    pub(super) keep_index: bool,
    /// Take the staged changes out of the index too (with `--patch` they stay unless this is given).
    #[arg(long, conflicts_with = "keep_index")]
    pub(super) no_keep_index: bool,
    /// Choose the hunks to stash, one at a time.
    #[arg(short = 'p', long)]
    pub(super) patch: bool,
    /// Stash only these paths (literal, not patterns).
    #[arg(last = true)]
    pub(super) paths: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum StashAction {
    /// Save the local changes (the default).
    Push(PushArgs),
    /// List the stashes, newest first.
    List,
    /// Apply a stash and drop it (the newest if none is named).
    Pop {
        /// Restore the staged state too.
        #[arg(long)]
        index: bool,
        stash: Option<String>,
    },
    /// Apply a stash and keep it.
    Apply {
        /// Restore the staged state too.
        #[arg(long)]
        index: bool,
        stash: Option<String>,
    },
    /// Remove a stash.
    Drop { stash: Option<String> },
    /// Remove every stash.
    Clear,
    /// Show what a stash changed.
    Show(super::stash_show::ShowArgs),
    /// Create a branch at the commit a stash was made on, apply the stash there and drop it.
    Branch { name: String, stash: Option<String> },
    /// Make a stash commit without storing it or touching the work tree; prints its id.
    Create {
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        message: Vec<String>,
    },
    /// Put a commit made by `stash create` on the stash list.
    Store {
        #[arg(short, long)]
        message: Option<String>,
        #[arg(short, long)]
        quiet: bool,
        #[arg(allow_hyphen_values = true)]
        commit: Vec<String>,
    },
}

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

pub fn stash(git: &GitHome, cwd: &Path, args: StashArgs) -> Result<()> {
    match args.action {
        None => push(git, cwd, args.push),
        Some(StashAction::Push(p)) => push(git, cwd, p),
        Some(StashAction::List) => list(git),
        Some(StashAction::Pop { index, stash }) => apply(git, stash.as_deref(), true, index),
        Some(StashAction::Apply { index, stash }) => apply(git, stash.as_deref(), false, index),
        Some(StashAction::Drop { stash }) => drop_entry(git, stash.as_deref()),
        Some(StashAction::Clear) => clear(git),
        Some(StashAction::Show(a)) => super::stash_show::show(git, a),
        Some(StashAction::Branch { name, stash }) => branch(git, &name, stash.as_deref()),
        Some(StashAction::Create { message }) => create_only(git, &message),
        Some(StashAction::Store {
            message,
            quiet: _,
            commit,
        }) => store(git, message.as_deref(), &commit),
    }
}

/// The stashes, newest first, with the commit each names.
fn entries(git: &GitHome) -> Result<Vec<ReflogEntry>> {
    let mut log = git.repo.refs().read_reflog(STASH)?;
    log.reverse();
    Ok(log)
}

fn list(git: &GitHome) -> Result<()> {
    for (n, e) in entries(git)?.iter().enumerate() {
        println!("stash@{{{n}}}: {}", e.message);
    }
    Ok(())
}

/// A stash a command line names: its position when it is on the list, and the typed name.
pub(super) struct Located {
    pub(super) n: Option<usize>,
    pub(super) id: ObjectId,
    pub(super) name: String,
}

/// Resolves `stash@{N}`, `N`, the newest (none given) or any stash-like commit.
pub(super) fn locate(git: &GitHome, name: Option<&str>) -> Result<Located> {
    let all = entries(git)?;
    let (n, label, bare) = match name {
        None if all.is_empty() => {
            eprintln!("No stash entries found.");
            std::process::exit(1);
        }
        None => (0, "refs/stash@{0}".to_string(), false),
        Some(s) => {
            let braced = s
                .strip_prefix("stash@{")
                .and_then(|r| r.strip_suffix('}'))
                .and_then(|n| n.parse::<usize>().ok());
            match (braced, s.parse::<usize>()) {
                (Some(n), _) => (n, s.to_string(), false),
                (None, Ok(n)) => (n, format!("refs/stash@{{{n}}}"), true),
                _ => return stash_like(git, s),
            }
        }
    };
    match all.get(n) {
        Some(e) => Ok(Located {
            n: Some(n),
            id: e.new,
            name: label,
        }),
        None if all.is_empty() => Err(Refused(format!("{label} is not a valid reference")).into()),
        None => bail!(
            "log for '{}' only has {} entries",
            if bare { "refs/stash" } else { "stash" },
            all.len()
        ),
    }
}

fn stash_like(git: &GitHome, spec: &str) -> Result<Located> {
    let Ok(id) = git.repo.resolve(spec) else {
        return Err(Refused(format!("{spec} is not a valid reference")).into());
    };
    match git.repo.commit(&id) {
        Ok(c) if c.parents.len() >= 2 => Ok(Located {
            n: None,
            id,
            name: spec.to_string(),
        }),
        _ => bail!("'{spec}' is not a stash-like commit"),
    }
}

/// The position and commit a stash argument names, which must be on the list.
fn find(git: &GitHome, name: Option<&str>) -> Result<Located> {
    let found = locate(git, name)?;
    match found.n {
        Some(_) => Ok(found),
        None => Err(Refused(format!("'{}' is not a stash reference", found.name)).into()),
    }
}

fn branch_label(git: &GitHome) -> Result<String> {
    Ok(match git.repo.head()? {
        Head::Branch { name, .. } => name,
        Head::Detached(_) => "(no branch)".into(),
    })
}

fn commit_with(
    git: &GitHome,
    tree: ObjectId,
    parents: Vec<ObjectId>,
    message: String,
) -> Result<ObjectId> {
    let record = Commit {
        tree,
        parents,
        author: identity(git, "author")?,
        committer: identity(git, "committer")?,
        extra_headers: Vec::new(),
        message: message.into_bytes(),
    };
    Ok(git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode(git.algorithm())?)?)
}

fn tree_of_entries(git: &GitHome, entries: Entries, version: u32) -> Result<ObjectId> {
    write_tree(git, &index_of(version, entries))
}

fn push(git: &GitHome, cwd: &Path, args: PushArgs) -> Result<()> {
    if args.patch && args.include_untracked {
        eprintln!("Can't use --patch and --include-untracked or --all at the same time");
        std::process::exit(1);
    }
    let scope = Scope::of(git, cwd, &args.paths)?;
    if let Some((_, message)) = save_in(git, &args, &scope, true, false)? {
        println!("Saved working directory and index state {message}");
    }
    Ok(())
}

/// The paths a stash is limited to; empty `named` means the whole work tree.
pub(super) struct Scope {
    spec: Pathspec,
    named: Vec<(String, String)>,
    cwd_prefix: String,
}

impl Scope {
    pub(super) fn all() -> Self {
        Self {
            spec: Pathspec::default(),
            named: Vec::new(),
            cwd_prefix: String::new(),
        }
    }

    fn of(git: &GitHome, cwd: &Path, paths: &[String]) -> Result<Self> {
        if paths.iter().any(String::is_empty) {
            bail!(
                "empty string is not a valid pathspec. please use . instead if you meant to match all paths"
            );
        }
        for p in paths {
            if git.ws.repo_prefix(cwd, p).is_err() {
                bail!(
                    "{p}: '{p}' is outside repository at '{}'",
                    git.repo.root()?.display()
                );
            }
        }
        let Specs { spec, named } = resolve_specs(git, cwd, paths)?;
        let cwd_prefix = if paths.is_empty() {
            String::new()
        } else {
            git.ws.repo_prefix(cwd, ".")?
        };
        Ok(Self {
            spec,
            named,
            cwd_prefix,
        })
    }

    fn limited(&self) -> bool {
        !self.named.is_empty()
    }

    /// The paths named that neither the index nor `extra` knows, as git words them.
    fn unknown(&self, index: &Index, extra: &[Vec<u8>]) -> Vec<String> {
        let mut missing = Vec::new();
        for (arg, prefix) in &self.named {
            let p = prefix.as_bytes();
            if prefix.is_empty()
                || index.entries.iter().any(|e| under(&e.path, p))
                || extra.iter().any(|e| under(e, p))
            {
                continue;
            }
            let here = &self.cwd_prefix;
            let n = if here.is_empty() || !under(p, here.as_bytes()) {
                0
            } else {
                here.len() + 1
            };
            let slash = if arg.ends_with('/') { "/" } else { "" };
            missing.push(format!(":(prefix:{n}){prefix}{slash}"));
        }
        missing
    }
}

/// Prints git's complaint about unmatched paths and exits.
fn refuse_unknown(missing: &[String], hint: bool) {
    if missing.is_empty() {
        return;
    }
    for spec in missing {
        eprintln!("error: pathspec '{spec}' did not match any file(s) known to git");
    }
    if hint {
        eprintln!("Did you forget to 'git add'?");
    }
    std::process::exit(1);
}

/// Stashes the local changes: the commit and its message, or `None` when there is nothing to save. With `store` the
/// commit is also put on `refs/stash`; `quiet` leaves out the notices.
pub(super) fn save(
    git: &GitHome,
    args: &PushArgs,
    store: bool,
    quiet: bool,
) -> Result<Option<(ObjectId, String)>> {
    save_in(git, args, &Scope::all(), store, quiet)
}

/// What the work tree and index hold relative to `HEAD`, read once for a stash.
struct Local {
    committed: BTreeMap<Vec<u8>, Entry>,
    leaves: BTreeMap<Vec<u8>, Entry>,
    unstaged: BTreeMap<Vec<u8>, Kind>,
}

impl Local {
    /// True when neither the index nor the work tree differs from `HEAD` at `path`.
    fn clean(&self, path: &[u8]) -> bool {
        self.leaves.get(path) == self.committed.get(path) && !self.unstaged.contains_key(path)
    }

    fn paths(&self) -> BTreeSet<&Vec<u8>> {
        self.committed.keys().chain(self.leaves.keys()).collect()
    }
}

struct Created {
    patched: Option<StashPatch>,
    commit: ObjectId,
    message: String,
    head_tree: ObjectId,
    index_tree: ObjectId,
    untracked: Vec<Vec<u8>>,
    every_untracked: Vec<Vec<u8>>,
    local: Local,
}

/// Builds the stash commit for the changes under `scope`; nothing but objects is written.
fn create(
    git: &GitHome,
    opened: &OpenIndex,
    args: &PushArgs,
    scope: &Scope,
) -> Result<Option<Created>> {
    refuse_in_progress(git).map_err(|e| Refused(e.to_string()))?;
    let Some(head) = git.repo.head_commit()? else {
        bail!("You do not have the initial commit yet");
    };
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        return Err(Refused(
            "Cannot save the current index state: the index has unmerged paths".into(),
        )
        .into());
    }
    let head_tree = git.repo.commit(&head)?.tree;
    let local = Local {
        committed: git.repo.flatten(Some(&head_tree), &Pathspec::default())?,
        leaves: git.repo.index_leaves(&opened.index),
        unstaged: git.repo.unstaged_changes(&opened.index)?,
    };
    let version = opened.index.version;
    let mut untracked = if args.include_untracked {
        untracked_files(git, &opened.index)?
    } else {
        Vec::new()
    };
    if !args.include_untracked {
        refuse_unknown(&scope.unknown(&opened.index, &[]), true);
    }
    let every_untracked = untracked.clone();
    untracked.retain(|p| scope.spec.matches(p));

    let index_tree = tree_of_entries(git, entries_of(&opened.index), version)?;
    let mut work = entries_of(&opened.index);
    for path in local.paths().into_iter().filter(|p| scope.spec.matches(p)) {
        if local.clean(path) {
            continue;
        }
        let file = fs_path(git.repo.root()?, path);
        if std::fs::symlink_metadata(&file).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
        {
            let existing = work.get(&(path.clone(), 0)).cloned();
            work.insert((path.clone(), 0), stage_file(git, path, existing.as_ref())?);
        } else if local.committed.contains_key(path) {
            work.remove(&(path.clone(), 0));
        }
    }
    let work_tree = tree_of_entries(git, work, version)?;
    check_writes(&untracked)?;
    let changed = if scope.limited() {
        local
            .paths()
            .into_iter()
            .any(|p| scope.spec.matches(p) && !local.clean(p))
    } else {
        index_tree != head_tree || work_tree != head_tree
    };
    if !changed && untracked.is_empty() {
        return Ok(None);
    }
    let patched = if args.patch {
        let Some(chosen) = patch_mode::stash(git, opened, head, &scope.spec)? else {
            eprintln!("No changes selected");
            std::process::exit(1);
        };
        Some(chosen)
    } else {
        None
    };
    let work_tree = match &patched {
        Some(p) => tree_of_entries(git, p.work.clone(), version)?,
        None => work_tree,
    };
    let branch = branch_label(git)?;
    let base = format!("{branch}: {} {}", short(&head), subject(git, &head)?);
    let index_commit = commit_with(git, index_tree, vec![head], format!("index on {base}\n"))?;
    let mut parents = vec![head, index_commit];
    if !untracked.is_empty() {
        let mut entries = Entries::new();
        for path in &untracked {
            entries.insert((path.clone(), 0), stage_file(git, path, None)?);
        }
        let tree = tree_of_entries(git, entries, version)?;
        parents.push(commit_with(
            git,
            tree,
            Vec::new(),
            format!("untracked files on {base}\n"),
        )?);
    }
    let message = match &args.message {
        Some(m) => format!("On {branch}: {m}"),
        None => format!("WIP on {base}"),
    };
    let commit = commit_with(git, work_tree, parents, message.clone())?;
    Ok(Some(Created {
        patched,
        commit,
        message,
        head_tree,
        index_tree,
        untracked,
        every_untracked,
        local,
    }))
}

fn save_in(
    git: &GitHome,
    args: &PushArgs,
    scope: &Scope,
    store: bool,
    quiet: bool,
) -> Result<Option<(ObjectId, String)>> {
    let opened = git.open_index()?;
    let Some(made) = create(git, &opened, args, scope)? else {
        if !quiet {
            println!("No local changes to save");
        }
        return Ok(None);
    };
    if store {
        store_stash(git, &made.commit, &made.message)?;
    }
    if let Some(patch) = &made.patched {
        if let Some(why) = &patch.failure {
            println!("Saved working directory and index state {}", made.message);
            why.lines().for_each(|l| eprintln!("error: {l}"));
            eprintln!("Cannot remove worktree changes");
            std::process::exit(1);
        }
        finish_patch(git, patch, args, scope, &made.local)?;
        return Ok(Some((made.commit, made.message)));
    }
    if scope.limited() && args.include_untracked {
        // git's clean step complains only once the stash is saved, and before changing anything
        let missing = scope.unknown(&opened.index, &made.every_untracked);
        if !missing.is_empty() {
            if !quiet {
                println!("Saved working directory and index state {}", made.message);
            }
            eprintln!("fatal: pathspec '{}' did not match any files", missing[0]);
            std::process::exit(1);
        }
    }
    if scope.limited() {
        reset_scope(git, &opened, &made.local, scope)?;
    } else {
        reset_all(git, &opened, &made, args.keep_index)?;
    }
    for path in &made.untracked {
        remove_entry_file(git, path)?;
    }
    if scope.limited() && args.keep_index {
        // git checks the paths again once the untracked files are gone, after the stash is already saved
        let missing = scope.unknown(&git.open_index()?.index, &[]);
        if !missing.is_empty() && !quiet {
            println!("Saved working directory and index state {}", made.message);
        }
        refuse_unknown(&missing, false);
        restore_staged(git, &made.local, scope)?;
    }
    Ok(Some((made.commit, made.message)))
}

/// Takes the stashed hunks out of the work tree, and with `--no-keep-index` the staged changes out of the index.
fn finish_patch(
    git: &GitHome,
    patch: &StashPatch,
    args: &PushArgs,
    scope: &Scope,
    local: &Local,
) -> Result<()> {
    for (path, state) in &patch.reverts {
        match state {
            Some((mode, bytes)) => {
                write_bytes(git, path, *mode, bytes)?;
            }
            None => remove_entry_file(git, path)?,
        }
    }
    if !args.no_keep_index {
        return Ok(());
    }
    let opened = git.open_index()?;
    let mut entries = entries_of(&opened.index);
    for path in local.paths().into_iter().filter(|p| scope.spec.matches(p)) {
        match local.committed.get(path) {
            Some(e) => put_entry(
                &mut entries,
                IndexEntry::new(path.clone(), raw_mode(e.mode), e.id),
            ),
            None => remove_entry(&mut entries, path),
        }
    }
    git.save_index(&opened, &index_of(opened.index.version, entries))
}

/// Resets the whole index and work tree to `HEAD` (`reset --hard`), then back to the index with `keep_index`.
fn reset_all(git: &GitHome, opened: &OpenIndex, made: &Created, keep_index: bool) -> Result<()> {
    let head = git
        .repo
        .head_commit()?
        .expect("a stash needs an initial commit");
    let tree = Some(&made.head_tree);
    if let Err(blocked) = move_tree(git, opened, tree, tree, Mode::Force)? {
        blocked.report("stash", "stash", 1);
    }
    super::merge::write_orig_head_as(git, &head, Some("reset"))?;
    git.log_ref("HEAD", Some(&head), &head, "reset: moving to HEAD", false)?;
    if keep_index {
        let reopened = git.open_index()?;
        let kept = Some(&made.index_tree);
        if let Err(blocked) = move_tree(git, &reopened, tree, kept, Mode::Force)? {
            blocked.report("stash", "stash", 1);
        }
    }
    Ok(())
}

/// Puts the named paths back to `HEAD` in the index and work tree.
fn reset_scope(git: &GitHome, opened: &OpenIndex, local: &Local, scope: &Scope) -> Result<()> {
    let mut entries = entries_of(&opened.index);
    for path in local.paths().into_iter().filter(|p| scope.spec.matches(p)) {
        if local.clean(path) {
            continue;
        }
        match local.committed.get(path) {
            Some(entry) => put_entry(&mut entries, write_entry(git, path, entry)?),
            None => {
                remove_entry_file(git, path)?;
                remove_entry(&mut entries, path);
            }
        }
    }
    git.save_index(opened, &index_of(opened.index.version, entries))
}

/// Brings the staged versions of the named paths back into the index and work tree (`--keep-index`).
fn restore_staged(git: &GitHome, local: &Local, scope: &Scope) -> Result<()> {
    let opened = git.open_index()?;
    let mut entries = entries_of(&opened.index);
    for path in local.paths().into_iter().filter(|p| scope.spec.matches(p)) {
        match (local.leaves.get(path), local.committed.get(path)) {
            (Some(staged), committed) if committed != Some(staged) => {
                put_entry(&mut entries, write_entry(git, path, staged)?);
            }
            (None, Some(_)) => {
                remove_entry_file(git, path)?;
                remove_entry(&mut entries, path);
            }
            _ => {}
        }
    }
    git.save_index(&opened, &index_of(opened.index.version, entries))
}

/// Puts a stash commit on `refs/stash` as the newest entry.
pub(super) fn store_stash(git: &GitHome, stash: &ObjectId, message: &str) -> Result<()> {
    let old = git.repo.refs().resolve(STASH)?;
    git.repo
        .refs()
        .update_checked(STASH, old.map_or(Expect::Absent, Expect::Id), stash)?;
    git.log_ref(STASH, old.as_ref(), stash, message, true)
}

pub(super) fn commit_tree(git: &GitHome, id: &ObjectId) -> Result<ObjectId> {
    Ok(git.repo.commit(id)?.tree)
}

fn apply(git: &GitHome, name: Option<&str>, pop: bool, index: bool) -> Result<()> {
    let found = if pop {
        find(git, name)?
    } else {
        locate(git, name)?
    };
    apply_and_report(git, &found.id, index, pop)?;
    match (pop, found.n) {
        (true, Some(n)) => drop_at(git, n, &found.name),
        _ => Ok(()),
    }
}

/// Applies a stash and prints the status; a conflict or a blocked apply ends the command.
fn apply_and_report(git: &GitHome, id: &ObjectId, index: bool, pop: bool) -> Result<()> {
    let applied = apply_stash(git, id, index);
    if pop
        && let Err(e) = &applied
        && e.downcast_ref::<Refused>()
            .is_some_and(|r| r.0 == INDEX_CONFLICT)
    {
        println!("The stash entry is kept in case you need it again.");
    }
    match applied? {
        Applying::Blocked(blocked) => blocked.report("merge", "merge", 1),
        Applying::Conflicted => {
            if pop {
                println!("The stash entry is kept in case you need it again.");
            }
            std::process::exit(1);
        }
        Applying::Clean => {}
    }
    match git
        .repo
        .validate_diff_ignore_submodules()
        .and_then(|()| git.repo.status())
    {
        Ok(status) => super::commands::status_table(&status),
        Err(error) => {
            // Git completes a clean apply even when the status report fails.
            let error = anyhow::Error::from(error);
            eprintln!(
                "fatal: {}",
                crate::sanitize::text(&crate::fatal_message(&error))
            );
            Ok(())
        }
    }
}

fn branch(git: &GitHome, name: &str, stash: Option<&str>) -> Result<()> {
    let found = locate(git, stash)?;
    let base = git.repo.commit(&found.id)?.parents[0];
    if let Err(e) = switch_to_new(git, name, &base) {
        match e.downcast_ref::<Refused>() {
            Some(r) => eprintln!("error: {r}"),
            None => eprintln!("fatal: {e:#}"),
        }
        std::process::exit(1);
    }
    apply_and_report(git, &found.id, true, false)?;
    match found.n {
        Some(n) => drop_at(git, n, &found.name),
        None => Ok(()),
    }
}

fn create_only(git: &GitHome, message: &[String]) -> Result<()> {
    let args = PushArgs {
        message: (!message.is_empty()).then(|| message.join(" ")),
        ..PushArgs::default()
    };
    let opened = git.open_index()?;
    if let Some(made) = create(git, &opened, &args, &Scope::all())? {
        println!("{}", made.commit);
    }
    Ok(())
}

fn store(git: &GitHome, message: Option<&str>, commit: &[String]) -> Result<()> {
    let [spec] = commit else {
        eprintln!("\"git stash store\" requires one <commit> argument");
        std::process::exit(1);
    };
    let Ok(id) = git.repo.resolve(spec) else {
        eprintln!("Cannot update {STASH} with {spec}");
        std::process::exit(1);
    };
    if let Ok(object) = git.repo.objects().read(&id)
        && object.kind != ObjectKind::Commit
    {
        eprintln!(
            "error: object {id} is a {}, not a commit",
            object.kind.name()
        );
    }
    if !git.repo.commit(&id).is_ok_and(|c| c.parents.len() >= 2) {
        bail!("'{id}' is not a stash-like commit");
    }
    if git.repo.refs().resolve(STASH)? == Some(id) {
        return Ok(());
    }
    store_stash(
        git,
        &id,
        message.unwrap_or("Created via \"git stash store\"."),
    )
}

pub(super) enum Applying {
    Clean,
    Conflicted,
    /// Local changes or untracked files in the way; nothing was changed.
    Blocked(Blocked),
}

/// Merges the changes a stash commit holds onto the work tree and index.
pub(super) fn apply_commit(git: &GitHome, id: &ObjectId) -> Result<Applying> {
    apply_stash(git, id, false)
}

/// Like [`apply_commit`]; with `index` the staged state the stash recorded is restored too.
fn apply_stash(git: &GitHome, id: &ObjectId, index: bool) -> Result<Applying> {
    refuse_in_progress(git).map_err(|e| Refused(e.to_string()))?;
    let stash = git.repo.commit(id)?;
    if stash.parents.len() < 2 {
        bail!("{} is not a stash commit", id);
    }
    if git.repo.head_commit()?.is_none() {
        bail!("You do not have the initial commit yet");
    }
    let opened = git.open_index()?;
    let unmerged: BTreeSet<&Vec<u8>> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.stage > 0)
        .map(|e| &e.path)
        .collect();
    if !unmerged.is_empty() {
        for path in unmerged {
            eprintln!("{}: needs merge", path_string(path));
        }
        return Ok(Applying::Conflicted);
    }
    let version = opened.index.version;
    let base_tree = commit_tree(git, &stash.parents[0])?;
    let current_tree = tree_of_entries(git, entries_of(&opened.index), version)?;
    let staged = if index {
        staged_state(git, &stash, &base_tree, &current_tree)?
    } else {
        None
    };
    let restore_untracked: BTreeMap<Vec<u8>, Entry> = match stash.parents.get(2) {
        Some(u) => {
            let tree = commit_tree(git, u)?;
            git.repo.flatten(Some(&tree), &Pathspec::default())?
        }
        None => BTreeMap::new(),
    };
    let incoming =
        git.repo
            .diff_trees(Some(&base_tree), Some(&stash.tree), &Pathspec::default())?;
    convert::refuse_conversions(
        git,
        incoming
            .iter()
            .filter_map(|c| c.new.map(|e| (c.path.as_slice(), e.mode)))
            .chain(
                restore_untracked
                    .iter()
                    .map(|(p, e)| (p.as_slice(), e.mode)),
            ),
    )?;
    let merged = merge_trees(
        git,
        Some(&base_tree),
        Some(&current_tree),
        Some(&stash.tree),
        &Labels {
            ours: "Updated upstream",
            theirs: "Stashed changes",
            base: "Stash base",
        },
        &Settings::default(),
    )?;
    check_writes(merged.paths.keys().chain(restore_untracked.keys()))?;
    let ours = git.repo.index_leaves(&opened.index);
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let ignores = super::stage::Ignores {
        ignore: git.repo.ignore()?,
    };
    let none = HashSet::new();
    let mut blocked = Blocked::default();
    for (path, resolved) in &merged.paths {
        let here = ours.get(path);
        let changes = match resolved {
            Resolved::Take(e) => e.as_ref() != here,
            Resolved::Conflict(c) => !matches!(c.worktree, Worktree::Keep),
        };
        if !changes {
            continue;
        }
        match here {
            Some(_) if matches!(dirty.get(path), Some(Kind::Modified | Kind::TypeChange)) => {
                blocked.local.push(path.clone());
            }
            None if in_the_way(git, &ignores, path, &none)? => blocked.untracked.push(path.clone()),
            _ => {}
        }
    }
    if !blocked.local.is_empty() || !blocked.untracked.is_empty() {
        return Ok(Applying::Blocked(blocked));
    }
    let in_the_way: Vec<&Vec<u8>> = restore_untracked
        .keys()
        .filter(|p| {
            std::fs::symlink_metadata(fs_path(git.repo.root().expect("a work tree"), p)).is_ok()
        })
        .collect();

    let mut entries = entries_of(&opened.index);
    let mut added: Vec<IndexEntry> = Vec::new();
    let mut conflicted = false;
    for (path, resolved) in &merged.paths {
        if merged.line_merged.contains(path) {
            println!("Auto-merging {}", path_string(path));
        }
        match resolved {
            Resolved::Take(Some(entry)) => {
                if ours.get(path) != Some(entry) {
                    let written = write_entry(git, path, entry)?;
                    if !ours.contains_key(path) {
                        added.push(written.clone());
                    }
                    put_entry(&mut entries, written);
                }
            }
            Resolved::Take(None) => {
                if ours.contains_key(path) {
                    remove_entry_file(git, path)?;
                    super::stage::remove_entry(&mut entries, path);
                }
            }
            Resolved::Conflict(c) => {
                conflicted = true;
                println!(
                    "CONFLICT (content): Merge conflict in {}",
                    path_string(path)
                );
                if let Worktree::Write { mode, bytes } = &c.worktree {
                    write_bytes(git, path, *mode, bytes)?;
                }
                super::stage::remove_entry(&mut entries, path);
                for (stage, side) in [(1u8, &c.base), (2, &c.ours), (3, &c.theirs)] {
                    if let Some(e) = side {
                        let mut entry = IndexEntry::new(path.clone(), raw_mode(e.mode), e.id);
                        entry.stage = stage;
                        entries.insert((path.clone(), stage), entry);
                    }
                }
            }
        }
    }
    for (path, entry) in &restore_untracked {
        if !in_the_way.contains(&path) {
            write_entry(git, path, entry)?;
        }
    }
    let index: Index = if conflicted {
        if staged.is_some() {
            eprintln!("Index was not unstashed.");
        }
        index_of(version, entries)
    } else if let Some(merged) = &staged {
        staged_index(git, version, merged)?
    } else {
        let mut keep = entries_of(&opened.index);
        for entry in added {
            put_entry(&mut keep, entry);
        }
        index_of(version, keep)
    };
    git.save_index(&opened, &index)?;
    if staged.is_some() {
        let head = git.repo.head_commit()?.expect("checked above");
        super::merge::write_orig_head_as(git, &head, Some("reset"))?;
        git.log_ref("HEAD", Some(&head), &head, "reset: moving to HEAD", false)?;
    }
    if !in_the_way.is_empty() {
        for path in &in_the_way {
            eprintln!("{} already exists, no checkout", path_string(path));
        }
        return Err(Refused("could not restore untracked files from stash".into()).into());
    }
    Ok(if conflicted {
        Applying::Conflicted
    } else {
        Applying::Clean
    })
}

/// The index the stash recorded merged onto the current one; `None` when there is nothing to restore.
fn staged_state(
    git: &GitHome,
    stash: &Commit,
    base: &ObjectId,
    current: &ObjectId,
) -> Result<Option<Merge>> {
    let saved = commit_tree(git, &stash.parents[1])?;
    if saved == *base || saved == *current {
        return Ok(None);
    }
    let labels = Labels {
        ours: "Updated upstream",
        theirs: "Stashed changes",
        base: "Stash base",
    };
    let merged = merge_trees(
        git,
        Some(base),
        Some(current),
        Some(&saved),
        &labels,
        &Settings::default(),
    )?;
    if merged
        .paths
        .values()
        .any(|r| matches!(r, Resolved::Conflict(_)))
    {
        return Err(Refused(INDEX_CONFLICT.into()).into());
    }
    Ok(Some(merged))
}

/// An index of the merged entries, with the file's stat data where the work tree already holds that content.
fn staged_index(git: &GitHome, version: u32, merged: &Merge) -> Result<Index> {
    let mut entries = Entries::new();
    for (path, resolved) in &merged.paths {
        let Resolved::Take(Some(e)) = resolved else {
            continue;
        };
        let mut entry = IndexEntry::new(path.clone(), raw_mode(e.mode), e.id);
        let file = fs_path(git.repo.root()?, path);
        if let Ok(meta) = std::fs::symlink_metadata(&file)
            && e.mode != FileMode::Gitlink
            && (meta.is_file() || meta.file_type().is_symlink())
            && hash_worktree_file(git.repo.algorithm(), &file)? == (e.mode, e.id)
        {
            stat_fields(&mut entry, &meta);
        }
        entries.insert((path.clone(), 0), entry);
    }
    Ok(index_of(version, entries))
}

/// Removes entry `n` (0 is the newest) and rewrites the reflog chain, as `git stash drop` does.
fn drop_at(git: &GitHome, n: usize, label: &str) -> Result<()> {
    let mut log = git.repo.refs().read_reflog(STASH)?;
    let at = log.len() - 1 - n;
    let gone = log.remove(at);
    let mut previous = git.zero_id();
    for e in &mut log {
        e.old = previous;
        previous = e.new;
    }
    match log.last() {
        Some(top) => {
            let top = top.new;
            git.repo.refs().update_checked(STASH, Expect::Any, &top)?;
            git.repo.refs().write_reflog(STASH, &log)?;
        }
        None => {
            git.repo.refs().delete(STASH, Expect::Any)?;
            git.repo.refs().write_reflog(STASH, &[])?;
        }
    }
    println!("Dropped {label} ({})", gone.new);
    Ok(())
}

fn drop_entry(git: &GitHome, name: Option<&str>) -> Result<()> {
    let found = find(git, name)?;
    drop_at(git, found.n.expect("a listed stash"), &found.name)
}

fn clear(git: &GitHome) -> Result<()> {
    if git.repo.refs().resolve(STASH)?.is_some() {
        git.repo.refs().delete(STASH, Expect::Any)?;
    }
    git.repo.refs().write_reflog(STASH, &[])?;
    Ok(())
}

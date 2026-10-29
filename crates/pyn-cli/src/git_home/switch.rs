//! `switch`, `checkout` and the work tree forms of `restore` on a git repository: they move `HEAD`, the index and
//! the work tree as git does, and never overwrite local changes unless `-f` (or the command's purpose) says to.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, bail};
use pyn_git::IndexEntry;
use pyn_gitwork::{Entry, Head, Kind, Pathspec};
use pyn_object::ObjectId;

use super::refwrite::BRANCHES;
use super::stage::{
    Entries, entries_of, index_of, put_entry, raw_mode, refuse_in_progress, remove_entry,
    resolve_specs, under,
};
use super::track::{self, Plan, Want};
use super::worktree::{Mode, check_writes, move_tree, remove_entry_file, write_bytes, write_entry};
use super::{GitHome, Refused, convert, path_string};
use crate::branch::{CheckoutArgs, SwitchArgs};

const CHECKOUT_HOOKS: [&str; 1] = ["post-checkout"];

pub(super) fn subject(git: &GitHome, id: &ObjectId) -> Result<String> {
    let commit = git.repo.commit(id)?;
    let text = String::from_utf8_lossy(&commit.message).into_owned();
    Ok(text.lines().next().unwrap_or_default().to_string())
}

/// Where a switch lands.
enum Landing {
    Branch(String),
    NewBranch {
        name: String,
        start: Option<String>,
    },
    /// `-C`: the branch is created, or reset to the start point when it exists.
    ResetBranch {
        name: String,
        start: Option<String>,
    },
    /// `--orphan`: an unborn branch with an empty index and work tree.
    Orphan(String),
    /// `explicit` is `--detach`, which leaves out the advice.
    Detach {
        spec: String,
        explicit: bool,
    },
}

struct Go {
    landing: Landing,
    force: bool,
    quiet: bool,
}

/// What `-` and `@{-N}` stand for: the branch (or commit) checked out N moves ago, read from the `HEAD` reflog.
pub(super) fn previous_branch(git: &GitHome, spec: &str) -> Result<String> {
    let n = match spec {
        "-" => Some(1),
        _ => spec
            .strip_prefix("@{-")
            .and_then(|r| r.strip_suffix('}'))
            .and_then(|n| n.parse::<usize>().ok())
            .filter(|n| *n > 0),
    };
    let Some(n) = n else {
        bail!("invalid reference: {spec}");
    };
    match git.repo.previous_checkout(n)? {
        Some(name) => Ok(name),
        None => bail!(
            "invalid reference: {}",
            if spec == "-" { "@{-1}" } else { spec }
        ),
    }
}

/// `name` with `-` and `@{-N}` replaced by the branch they stand for.
pub(super) fn expand_branch_name(git: &GitHome, name: String) -> Result<String> {
    if name == "-" || name.starts_with("@{-") {
        return previous_branch(git, &name);
    }
    Ok(name)
}

/// `M`, `A`, `D` or `T` for each tracked path that differs from `HEAD` in the index or the work tree.
fn local_changes(git: &GitHome) -> Result<Vec<(char, Vec<u8>)>> {
    let head = git.repo.head()?;
    let index = git.repo.read_index()?;
    let staged = git.repo.staged_changes(&head, &index)?;
    let unstaged = git.repo.unstaged_changes(&index)?;
    let mut paths: Vec<&Vec<u8>> = staged.keys().chain(unstaged.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut out = Vec::new();
    for path in paths {
        let (s, u) = (staged.get(path).copied(), unstaged.get(path).copied());
        if u == Some(Kind::Untracked) {
            continue;
        }
        let letter = match (s, u) {
            (Some(Kind::Added), Some(Kind::Deleted)) => continue,
            (_, Some(Kind::Deleted)) | (Some(Kind::Deleted), None) => 'D',
            (Some(Kind::Added), _) => 'A',
            (Some(Kind::TypeChange), _) | (None, Some(Kind::TypeChange)) => 'T',
            _ => 'M',
        };
        out.push((letter, path.clone()));
    }
    Ok(out)
}

fn current_label(git: &GitHome) -> Result<String> {
    Ok(match git.repo.head()? {
        Head::Branch { name, .. } => name,
        Head::Detached(id) => id.to_hex(),
    })
}

fn tree_of(git: &GitHome, commit: Option<&ObjectId>) -> Result<Option<ObjectId>> {
    Ok(match commit {
        Some(c) => Some(git.repo.commit(c)?.tree),
        None => None,
    })
}

fn advice_on(git: &GitHome, name: &str) -> bool {
    git.repo.config().get(name).is_none_or(|v| v != "false")
}

pub(super) fn detach_advice(git: &GitHome, label: &str, id: &ObjectId) -> Result<()> {
    detach_note(git, label);
    eprintln!(
        "HEAD is now at {} {}",
        git.repo.abbreviate(id)?,
        subject(git, id)?
    );
    Ok(())
}

fn detach_note(git: &GitHome, label: &str) {
    if advice_on(git, "advice.detachedhead") {
        eprintln!(
            "Note: switching to '{label}'.\n\n\
             You are in 'detached HEAD' state. You can look around, make experimental\n\
             changes and commit them, and you can discard any commits you make in this\n\
             state without impacting any branches by switching back to a branch.\n\n\
             If you want to create a new branch to retain commits you create, you may\n\
             do so (now or later) by using -c with the switch command. Example:\n\n  \
             pyn switch -c <new-branch-name>\n\n\
             Or undo this operation with:\n\n  pyn switch -\n\n\
             Turn off this advice by setting config variable advice.detachedHead to false\n"
        );
    }
}

/// Commits reachable from `old` that no ref, `HEAD` included, reaches.
fn left_behind(git: &GitHome, old: &ObjectId) -> Result<Vec<ObjectId>> {
    let mut covered = HashSet::new();
    let mut tips: Vec<ObjectId> = git.repo.head_commit()?.into_iter().collect();
    for (name, _) in git.repo.refs().list("refs/")? {
        if let Ok(id) = git.repo.resolve(&name) {
            tips.push(id);
        }
    }
    for tip in tips {
        if !covered.contains(&tip) {
            covered.extend(git.reachable(&tip)?);
        }
    }
    let mut log = git.repo.log_hiding(&[*old], covered, Pathspec::default())?;
    let mut out = Vec::new();
    while let Some(entry) = log.next_entry()? {
        out.push(entry.id);
    }
    Ok(out)
}

const ORPHAN_LISTED: usize = 4;

/// What git says on leaving a detached `HEAD`: the commits no branch keeps, or where `HEAD` was.
fn leaving_detached(git: &GitHome, old: &ObjectId) -> Result<()> {
    let lost = left_behind(git, old)?;
    if lost.is_empty() {
        eprintln!(
            "Previous HEAD position was {} {}",
            git.repo.abbreviate(old)?,
            subject(git, old)?
        );
        return Ok(());
    }
    let n = lost.len();
    eprintln!(
        "Warning: you are leaving {n} commit{} behind, not connected to\nany of your branches:\n",
        if n == 1 { "" } else { "s" }
    );
    for id in lost.iter().take(ORPHAN_LISTED) {
        eprintln!("  {} {}", git.repo.abbreviate(id)?, subject(git, id)?);
    }
    if n > ORPHAN_LISTED {
        eprintln!(" ... and {} more.", n - ORPHAN_LISTED);
    }
    eprintln!();
    if advice_on(git, "advice.detachedhead") {
        eprintln!(
            "If you want to keep {} by creating a new branch, this may be a good time\n\
             to do so with:\n\n git branch <new-branch-name> {}\n",
            if n == 1 { "it" } else { "them" },
            git.repo.abbreviate(&lost[0])?
        );
    }
    Ok(())
}

fn start_commit(git: &GitHome, spec: &str) -> Result<ObjectId> {
    git.repo
        .resolve(spec)
        .map_err(|_| anyhow::anyhow!("invalid reference: {spec}"))
}

fn go(git: &GitHome, go: Go) -> Result<()> {
    let via = Via {
        quiet: go.quiet,
        ..Via::default()
    };
    go_quietly(git, go, via)
}

/// How a caller drives a checkout: without chatter, onto a branch another work tree has, with a refusal returned.
#[derive(Clone, Copy, Default)]
pub(super) struct Via {
    pub quiet: bool,
    pub any_worktree: bool,
    pub returned: bool,
    pub force: bool,
}

/// Checks out a branch, else the commit `target` names (detached), as `git checkout [-q] <target> --`.
pub(super) fn checkout_revision(git: &GitHome, target: &str, via: Via) -> Result<()> {
    let landing = if git.branch_commit(target)?.is_some() {
        Landing::Branch(target.to_string())
    } else if git.repo.resolve(target).is_ok() {
        Landing::Detach {
            spec: target.to_string(),
            explicit: false,
        }
    } else {
        bail!("invalid reference: {target}");
    };
    go_quietly(
        git,
        Go {
            landing,
            force: via.force,
            quiet: via.quiet,
        },
        via,
    )
}

fn go_quietly(git: &GitHome, go: Go, via: Via) -> Result<()> {
    let Via {
        quiet,
        any_worktree: ignore_other_worktrees,
        returned,
        force: _,
    } = via;
    refuse_in_progress(git)?;
    let head = git.repo.head()?;
    let from = current_label(git)?;
    let old_commit = head.id();
    let was_detached = matches!(head, Head::Detached(_));
    let (target, branch_ref, label, created): (Option<ObjectId>, Option<String>, String, bool) =
        match &go.landing {
            Landing::Branch(name) => {
                let Some(id) = git.branch_commit(name)? else {
                    bail!("invalid reference: {name}");
                };
                (
                    Some(id),
                    Some(GitHome::branch_ref(name)),
                    name.clone(),
                    false,
                )
            }
            Landing::NewBranch { name, start } => {
                GitHome::check_branch_name(name)?;
                if git.branch_commit(name)?.is_some() {
                    bail!("a branch named '{name}' already exists");
                }
                git.check_ref_room(&GitHome::branch_ref(name))?;
                let id = match start {
                    Some(s) => Some(start_commit(git, s)?),
                    None => old_commit,
                };
                (id, Some(GitHome::branch_ref(name)), name.clone(), true)
            }
            Landing::ResetBranch { name, start } => {
                GitHome::check_branch_name(name)?;
                let full = GitHome::branch_ref(name);
                let exists = git.branch_commit(name)?.is_some();
                if exists {
                    if let Some(tree) = git.used_elsewhere(name)? {
                        bail!(
                            "cannot force update the branch '{name}' used by worktree at '{}'",
                            tree.display()
                        );
                    }
                } else {
                    git.check_ref_room(&full)?;
                }
                let id = match start {
                    Some(s) => start_commit(git, s)?,
                    None => match old_commit {
                        Some(id) => id,
                        None => bail!("invalid reference: HEAD"),
                    },
                };
                (Some(id), Some(full), name.clone(), !exists)
            }
            Landing::Orphan(name) => {
                GitHome::check_branch_name(name)?;
                if git.branch_commit(name)?.is_some() {
                    bail!("a branch named '{name}' already exists");
                }
                git.check_ref_room(&GitHome::branch_ref(name))?;
                (None, Some(GitHome::branch_ref(name)), name.clone(), false)
            }
            Landing::Detach { spec, .. } => {
                (Some(git.repo.resolve(spec)?), None, spec.clone(), false)
            }
        };
    let same_branch = matches!(&head, Head::Branch { name, .. } if Some(format!("{BRANCHES}{name}")) == branch_ref);
    if let Landing::Branch(name) = &go.landing
        && !same_branch
        && !ignore_other_worktrees
        && let Some(tree) = git.used_elsewhere(name)?
    {
        bail!(
            "'{name}' is already used by worktree at '{}'",
            tree.display()
        );
    }
    let opened = git.open_index()?;
    let (old_tree, new_tree) = (
        tree_of(git, old_commit.as_ref())?,
        tree_of(git, target.as_ref())?,
    );
    let mode = if go.force { Mode::Force } else { Mode::Safe };
    if let Err(blocked) = move_tree(git, &opened, old_tree.as_ref(), new_tree.as_ref(), mode)? {
        if !returned {
            blocked.report("checkout", "switch branches", 1);
        }
        blocked.print("checkout", "switch branches");
        eprintln!("Aborting");
        bail!("");
    }
    let message = format!("checkout: moving from {from} to {label}");
    match (&branch_ref, &target) {
        (Some(r), Some(id)) => {
            let start_label = match &go.landing {
                Landing::NewBranch { start: Some(s), .. }
                | Landing::ResetBranch { start: Some(s), .. } => s.clone(),
                _ => "HEAD".into(),
            };
            if created {
                git.create_ref(r, id, &format!("branch: Created from {start_label}"))?;
            } else if matches!(go.landing, Landing::ResetBranch { .. }) {
                let old = git.branch_commit(&label)?;
                let reset = format!("branch: Reset to {start_label}");
                if old != Some(*id) {
                    git.reset_ref(r, old.as_ref(), id, &reset)?;
                } else if same_branch {
                    git.log_ref("HEAD", Some(id), id, &reset, false)?;
                }
            }
            git.set_head(Some(r), id, &message)?;
        }
        (Some(r), None) => git.repo.refs().set_symbolic("HEAD", r)?,
        (None, Some(id)) if was_detached && old_commit == Some(*id) => {}
        (None, Some(id)) => git.set_head(None, id, &message)?,
        (None, None) => unreachable!("a detach has a commit"),
    }
    if !go.force && !quiet {
        for (letter, path) in local_changes(git)? {
            println!("{letter}\t{}", path_string(&path));
        }
    }
    git.warn_if_hooks_off(&CHECKOUT_HOOKS);
    let hook_new = target.or(old_commit);
    let hook_status = git.post_checkout(old_commit.as_ref(), hook_new.as_ref(), true)?;
    let done = || match hook_status {
        0 => Ok(()),
        _ => std::process::exit(1),
    };
    if quiet {
        return done();
    }
    if let (
        true,
        Landing::Branch(_)
        | Landing::NewBranch { .. }
        | Landing::ResetBranch { .. }
        | Landing::Orphan(_),
        Some(old),
    ) = (was_detached, &go.landing, old_commit)
        && target != Some(old)
    {
        leaving_detached(git, &old)?;
    }
    match &go.landing {
        Landing::NewBranch { name, .. } | Landing::Orphan(name) => {
            eprintln!("Switched to a new branch '{name}'");
        }
        Landing::ResetBranch { name, .. } if created => {
            eprintln!("Switched to a new branch '{name}'");
        }
        Landing::ResetBranch { name, .. } if same_branch => eprintln!("Reset branch '{name}'"),
        Landing::ResetBranch { name, .. } => eprintln!("Switched to and reset branch '{name}'"),
        Landing::Branch(name) if same_branch => eprintln!("Already on '{name}'"),
        Landing::Branch(name) => eprintln!("Switched to branch '{name}'"),
        Landing::Detach { spec, explicit } => {
            let id = target.expect("a detach has a commit");
            if !explicit && !was_detached {
                detach_note(git, spec);
            }
            if was_detached && old_commit != Some(id) {
                leaving_detached(git, &old_commit.expect("a detached head has a commit"))?;
            }
            eprintln!(
                "HEAD is now at {} {}",
                git.repo.abbreviate(&id)?,
                subject(git, &id)?
            );
        }
    }
    done()
}

pub fn switch(git: &GitHome, args: SwitchArgs) -> Result<()> {
    let recurse = super::submodule::wants_recursion(
        git,
        (args.recurse_submodules, args.no_recurse_submodules),
    );
    switch_to(git, args)?;
    if recurse {
        super::submodule::follow_head(git, false)?;
    }
    Ok(())
}

/// The branch `checkout --track <start>` creates: the part of a remote-tracking name after the remote.
fn tracked_name(start: &str) -> Result<String> {
    let rest = start.strip_prefix("refs/").unwrap_or(start);
    let rest = rest.strip_prefix("remotes/").unwrap_or(rest);
    match rest.split_once('/') {
        Some((_, name)) if !name.is_empty() => Ok(name.to_owned()),
        _ => bail!("missing branch name; try -b"),
    }
}

/// Creates the branch with `landing` and then sets up the tracking `plan` says.
fn go_tracking(git: &GitHome, go_to: Go, plan: Option<(String, Plan)>) -> Result<()> {
    let quiet = go_to.quiet;
    go(git, go_to)?;
    match plan {
        Some((name, p)) => track::apply(git, &name, &p, quiet),
        None => Ok(()),
    }
}

fn switch_to(git: &GitHome, mut args: SwitchArgs) -> Result<()> {
    let want = Want::of(&args.track);
    if want != Want::Config
        && args.create.is_none()
        && args.force_create.is_none()
        && args.orphan.is_none()
    {
        if args.detach {
            bail!("'--detach' cannot be used with '-t/--track'");
        }
        let Some(spec) = args.start.take() else {
            bail!("--track needs a branch name");
        };
        args.create = Some(tracked_name(&spec)?);
        args.start = Some(spec);
    }
    let made = args.create.clone().or_else(|| args.force_create.clone());
    let plan = match &made {
        Some(name) if args.orphan.is_none() => {
            track::plan(git, want, args.start.as_deref(), name)?.map(|p| (name.clone(), p))
        }
        _ => None,
    };
    let makers = [&args.create, &args.force_create, &args.orphan];
    if makers.iter().filter(|m| m.is_some()).count() > 1 {
        bail!("options '-c', '-C', and '--orphan' cannot be used together");
    }
    if args.detach && makers.iter().any(|m| m.is_some()) {
        bail!("'--detach' cannot be used with '-b/-B/--orphan'");
    }
    if args.orphan.is_some() && args.start.is_some() {
        bail!("'--orphan' cannot take <start-point>");
    }
    let landing = if let Some(name) = args.orphan {
        Landing::Orphan(name)
    } else if let Some(name) = args.force_create {
        Landing::ResetBranch {
            name,
            start: args.start,
        }
    } else {
        match (args.create, args.start, args.detach) {
            (Some(name), start, false) => Landing::NewBranch { name, start },
            (Some(_), _, true) => unreachable!("refused above"),
            (None, Some(spec), true) => Landing::Detach {
                spec,
                explicit: true,
            },
            (None, None, true) => Landing::Detach {
                spec: "HEAD".into(),
                explicit: true,
            },
            (None, Some(name), false) => {
                let name = expand_branch_name(git, name)?;
                if git.branch_commit(&name)?.is_none() {
                    if git.repo.resolve(&name).is_err()
                        && super::dwim::enabled(git, crate::branch::flag(args.guess, args.no_guess))
                        && let Some(g) = super::dwim::guess(git, &name)?
                    {
                        return create_guessed(git, name, &g, args.force, args.quiet);
                    }
                    if git.repo.resolve(&name).is_ok() {
                        bail!(
                            "a branch is expected, got commit '{name}'; use `pyn switch --detach {name}` to check it out"
                        );
                    }
                    bail!("invalid reference: {name}");
                }
                Landing::Branch(name)
            }
            (None, None, false) => bail!("missing branch or commit argument"),
        }
    };
    go_tracking(
        git,
        Go {
            landing,
            force: args.force,
            quiet: args.quiet,
        },
        plan,
    )
}

/// Creates the branch `name` from a guessed remote-tracking branch and makes it track that branch.
fn create_guessed(
    git: &GitHome,
    name: String,
    g: &super::dwim::Guess,
    force: bool,
    quiet: bool,
) -> Result<()> {
    let plan = track::plan(git, Want::Config, Some(&g.tracking), &name)?.map(|p| (name.clone(), p));
    let landing = Landing::NewBranch {
        name,
        start: Some(g.tracking.clone()),
    };
    go_tracking(
        git,
        Go {
            landing,
            force,
            quiet,
        },
        plan,
    )
}

/// Creates the branch `name` at the commit `start` and switches to it, as `checkout -b name start`.
pub(super) fn switch_to_new(git: &GitHome, name: &str, start: &ObjectId) -> Result<()> {
    go(
        git,
        Go {
            landing: Landing::NewBranch {
                name: name.to_string(),
                start: Some(start.to_hex()),
            },
            force: false,
            quiet: false,
        },
    )
}

enum Source {
    Index,
    Tree(ObjectId),
}

struct Restore {
    source: Source,
    to_index: bool,
    to_worktree: bool,
    /// Tracked paths the source lacks are removed too.
    prune: bool,
    /// Take this stage of an unmerged path: 2 for ours, 3 for theirs.
    stage: Option<u8>,
    /// Recreate the conflicted merge of unmerged paths from stages 1 to 3.
    merge: bool,
    /// Reach skip-worktree entries too (`--ignore-skip-worktree-bits`).
    ignore_skip: bool,
}

fn restore_paths(git: &GitHome, cwd: &Path, paths: &[String], r: Restore) -> Result<usize> {
    if paths.is_empty() {
        bail!("you must specify path(s) to restore");
    }
    let opened = git.open_index()?;
    let patterns = super::sparse::load(git)?;
    let skipped: HashMap<Vec<u8>, (u32, ObjectId)> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.skip_worktree && e.stage == 0)
        .map(|e| (e.path.clone(), (e.mode, e.id)))
        .collect();
    if r.ignore_skip
        && matches!(r.source, Source::Tree(_))
        && (patterns.is_some() || !skipped.is_empty())
    {
        bail!(
            "--ignore-skip-worktree-bits with a tree to restore from is not supported in a sparse checkout"
        );
    }
    let specs = resolve_specs(git, cwd, paths)?;
    let mut entries: Entries = entries_of(&opened.index);
    let from_tree = match &r.source {
        Source::Tree(commit) => {
            let tree = git.repo.commit(commit)?.tree;
            git.repo.flatten(Some(&tree), &specs.spec)?
        }
        Source::Index => BTreeMap::new(),
    };
    if let Some(path) = skipped
        .iter()
        .find(|(p, (mode, id))| {
            from_tree
                .get(*p)
                .is_some_and(|t| t.id != *id || raw_mode(t.mode) != *mode)
        })
        .map(|(p, _)| p)
    {
        bail!(
            "'{}' is outside the sparse-checkout patterns and differs in the tree to restore from; restoring it is not supported",
            path_string(path)
        );
    }
    let hidden: HashSet<Vec<u8>> = if r.ignore_skip {
        HashSet::new()
    } else {
        skipped.keys().cloned().collect()
    };
    let indexed: Vec<Vec<u8>> = entries
        .keys()
        .map(|(p, _)| p.clone())
        .filter(|p| !hidden.contains(p))
        .collect();
    let mut from_tree = from_tree;
    from_tree.retain(|p, _| !hidden.contains(p));
    if let Some(p) = &patterns
        && let Some(path) = from_tree
            .keys()
            .find(|k| !p.includes(k) && !entries.keys().any(|(e, _)| e == *k))
    {
        bail!(
            "'{}' is outside the sparse-checkout patterns and not in the index; restoring it from a tree is not supported",
            path_string(path)
        );
    }
    let picked: HashSet<Vec<u8>> = {
        let mut tracker = specs.spec.tracker();
        let candidates: BTreeSet<&Vec<u8>> = from_tree.keys().chain(indexed.iter()).collect();
        candidates
            .into_iter()
            .filter(|p| tracker.picks(p))
            .cloned()
            .collect()
    };
    let in_spec = |p: &[u8]| picked.contains(p);
    let mut remerged: BTreeMap<Vec<u8>, (Option<Entry>, Entry, Entry)> = BTreeMap::new();
    let mut all_versions: Vec<String> = Vec::new();
    let mut lacking: Vec<Vec<u8>> = Vec::new();
    let sources: BTreeMap<Vec<u8>, Entry> = match &r.source {
        Source::Tree(_) => from_tree,
        Source::Index => {
            let mut found = BTreeMap::new();
            let paths: BTreeSet<&Vec<u8>> = entries
                .keys()
                .map(|(p, _)| p)
                .filter(|p| in_spec(p) && !hidden.contains(*p))
                .collect();
            for path in paths {
                let at = |stage: u8| entries.get(&(path.clone(), stage));
                let chosen = match (at(0), r.stage) {
                    (Some(e), _) => Some(e),
                    (None, Some(wanted)) => at(wanted),
                    (None, None) if r.merge => {
                        let (Some(ours), Some(theirs)) = (at(2), at(3)) else {
                            all_versions.push(path_string(path));
                            continue;
                        };
                        let leaf = |e: &IndexEntry| Entry {
                            mode: super::stage::leaf_mode(e.mode),
                            id: e.id,
                        };
                        remerged.insert(path.clone(), (at(1).map(leaf), leaf(ours), leaf(theirs)));
                        continue;
                    }
                    (None, None) => {
                        return Err(
                            Refused(format!("path '{}' is unmerged", path_string(path))).into()
                        );
                    }
                };
                match chosen {
                    Some(e) => {
                        found.insert(
                            path.clone(),
                            Entry {
                                mode: super::stage::leaf_mode(e.mode),
                                id: e.id,
                            },
                        );
                    }
                    None => lacking.push(path.clone()),
                }
            }
            found
        }
    };
    if let Some(arg) = specs
        .spec
        .first_unmatched(sources.keys().chain(indexed.iter()).map(Vec::as_slice))
    {
        return Err(Refused(format!(
            "pathspec '{}' did not match any file(s) known to git",
            String::from_utf8_lossy(arg)
        ))
        .into());
    }
    let mut errors: Vec<String> = all_versions
        .iter()
        .map(|p| format!("path '{p}' does not have all necessary versions"))
        .collect();
    if !r.prune {
        let side = if r.stage == Some(2) { "our" } else { "their" };
        errors.extend(
            lacking
                .iter()
                .map(|p| format!("path '{}' does not have {side} version", path_string(p))),
        );
    }
    if let Some((last, rest)) = errors.split_last() {
        rest.iter().for_each(|e| eprintln!("error: {e}"));
        return Err(Refused(last.clone()).into());
    }
    let doomed: Vec<Vec<u8>> = if r.prune {
        indexed
            .iter()
            .filter(|p| in_spec(p) && !sources.contains_key(*p) && !remerged.contains_key(*p))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    if r.to_worktree {
        check_writes(
            sources
                .keys()
                .filter(|p| in_spec(p))
                .chain(remerged.keys())
                .chain(doomed.iter()),
        )?;
        convert::refuse_unsupported(
            git,
            sources
                .iter()
                .filter(|(p, _)| in_spec(p))
                .map(|(p, e)| (p.as_slice(), e.mode))
                .chain(remerged.iter().map(|(p, (_, o, _))| (p.as_slice(), o.mode))),
        )?;
        for (path, (base, ours, theirs)) in &remerged {
            let (mode, bytes) = super::remerge::recreate(git, path, base.as_ref(), ours, theirs)?;
            write_bytes(git, path, mode, &bytes)?;
        }
    }
    let mut cleared = false;
    for (path, entry) in &sources {
        let written = if r.to_worktree && in_spec(path) {
            Some(write_entry(git, path, entry)?)
        } else {
            None
        };
        let was_skipped = r.ignore_skip && skipped.contains_key(path);
        cleared |= was_skipped && written.is_some();
        if r.to_index || (was_skipped && written.is_some()) {
            let e = written
                .unwrap_or_else(|| IndexEntry::new(path.clone(), raw_mode(entry.mode), entry.id));
            put_entry(&mut entries, e);
        }
    }
    for path in &doomed {
        if r.to_worktree {
            remove_entry_file(git, path)?;
        }
        if r.to_index {
            remove_entry(&mut entries, path);
        }
    }
    if r.to_index || cleared {
        git.save_index(&opened, &index_of(opened.index.version, entries))?;
    }
    post_checkout_files(git)?;
    Ok(sources.len() + remerged.len() + doomed.len())
}

/// Runs `post-checkout` for a checkout of paths; a failing hook ends the command with status 1.
pub(super) fn post_checkout_files(git: &GitHome) -> Result<()> {
    git.warn_if_hooks_off(&CHECKOUT_HOOKS);
    let head = git.repo.head()?.id();
    if git.post_checkout(head.as_ref(), head.as_ref(), false)? != 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn commit_of(git: &GitHome, spec: &str) -> Result<ObjectId> {
    Ok(git.repo.resolve(spec)?)
}

/// `pyn restore` with the work tree as a target.
pub fn restore(
    git: &GitHome,
    cwd: &Path,
    paths: &[String],
    source: Option<&str>,
    staged: bool,
    worktree: bool,
    ignore_skip: bool,
) -> Result<()> {
    let to_worktree = worktree || !staged;
    let source = match source {
        Some(s) => Source::Tree(commit_of(git, s)?),
        None if staged => match git.repo.head_commit()? {
            Some(head) => Source::Tree(head),
            None => bail!("could not resolve HEAD"),
        },
        None => Source::Index,
    };
    let prune = matches!(source, Source::Tree(_));
    restore_paths(
        git,
        cwd,
        paths,
        Restore {
            source,
            to_index: staged,
            to_worktree,
            prune,
            stage: None,
            merge: false,
            ignore_skip,
        },
    )?;
    Ok(())
}

pub(super) struct SideFlags<'a> {
    pub source: Option<&'a str>,
    pub staged: bool,
    /// 2 for `--ours`, 3 for `--theirs`.
    pub stage: Option<u8>,
    pub merge: bool,
    pub ignore_skip: bool,
}

/// `pyn restore --ours|--theirs|--merge`: takes a side of unmerged paths, or recreates their conflicted merge.
pub(super) fn restore_sides(
    git: &GitHome,
    cwd: &Path,
    paths: &[String],
    f: SideFlags,
) -> Result<()> {
    if f.merge && f.stage.is_some() {
        bail!(
            "git checkout: --ours/--theirs, --force and --merge are incompatible when\nchecking out of the index."
        );
    }
    if f.staged {
        if f.stage.is_some() {
            bail!("'--ours' or '--theirs' cannot be used with --staged");
        }
        bail!("'--merge' or '--conflict' cannot be used with --staged");
    }
    if let Some(source) = f.source {
        commit_of(git, source)?;
        bail!("'--merge', '--ours', or '--theirs' cannot be used when checking out of a tree");
    }
    restore_paths(
        git,
        cwd,
        paths,
        Restore {
            source: Source::Index,
            to_index: false,
            to_worktree: true,
            prune: true,
            stage: f.stage,
            merge: f.merge,
            ignore_skip: f.ignore_skip,
        },
    )?;
    Ok(())
}

fn is_path(git: &GitHome, cwd: &Path, name: &str) -> Result<bool> {
    let Ok(prefix) = git.ws.repo_prefix(cwd, name) else {
        return Ok(false);
    };
    let index = git.repo.read_index()?;
    Ok(index
        .entries
        .iter()
        .any(|e| under(&e.path, prefix.as_bytes())))
}

/// The revision (if the first name is one) and the paths of `checkout -p`.
fn patch_target(
    git: &GitHome,
    cwd: &Path,
    names: &[String],
    paths: Vec<String>,
) -> Result<(Option<String>, Vec<String>)> {
    let dashes = !paths.is_empty() || std::env::args_os().any(|a| a == "--");
    let Some(first) = names.first() else {
        return Ok((None, paths));
    };
    if dashes {
        return Ok((Some(first.clone()), paths));
    }
    if git.repo.resolve(first).is_ok() {
        if is_path(git, cwd, first)? {
            bail!(
                "ambiguous argument '{first}': both revision and filename\n\
                 Use '--' to separate paths from revisions, like this:\n\
                 'pyn <command> [<revision>...] -- [<file>...]'"
            );
        }
        return Ok((Some(first.clone()), names[1..].to_vec()));
    }
    Ok((None, names.to_vec()))
}

pub fn checkout(git: &GitHome, cwd: &Path, args: CheckoutArgs) -> Result<()> {
    let recurse = super::submodule::wants_recursion(
        git,
        (args.recurse_submodules, args.no_recurse_submodules),
    );
    let switching =
        args.paths.is_empty() && !args.patch && !args.ours && !args.theirs && args.args.len() <= 1;
    checkout_to(git, cwd, args)?;
    if recurse && switching {
        super::submodule::follow_head(git, false)?;
    }
    Ok(())
}

/// `checkout -b` and `-B` word a start point that names no commit differently from `switch`.
fn check_start(git: &GitHome, name: &str, names: usize, start: Option<&str>) -> Result<()> {
    if names > 1 {
        bail!("Cannot update paths and switch to branch '{name}' at the same time.");
    }
    if let Some(s) = start
        && git.repo.resolve(s).is_err()
    {
        bail!("'{s}' is not a commit and a branch '{name}' cannot be created from it");
    }
    Ok(())
}

fn checkout_to(git: &GitHome, cwd: &Path, args: CheckoutArgs) -> Result<()> {
    let CheckoutArgs {
        args: names,
        paths,
        create,
        reset,
        force,
        detach,
        ours,
        theirs,
        yes: _,
        ignore_skip_worktree_bits: ignore_skip,
        patch,
        recurse_submodules: _,
        no_recurse_submodules: _,
        track: track_args,
        guess,
        no_guess,
        quiet,
        pathspec_file: _,
    } = args;
    if patch {
        if create.is_some() && reset.is_some() {
            bail!("options '-b', '-B', and '--orphan' cannot be used together");
        }
        if force {
            bail!("'--force' cannot be used with updating paths");
        }
        if detach {
            bail!("'--detach' cannot be used with updating paths");
        }
        if let Some(name) = create.as_ref().or(reset.as_ref()) {
            bail!("Cannot update paths and switch to branch '{name}' at the same time.");
        }
        let (rev, paths) = patch_target(git, cwd, &names, paths)?;
        if (ours || theirs) && rev.is_some() {
            bail!("'--merge', '--ours', or '--theirs' cannot be used when checking out of a tree");
        }
        return super::patch_mode::checkout(git, cwd, rev.as_deref(), &paths);
    }
    if create.is_some() && reset.is_some() {
        bail!("options '-b', '-B', and '--orphan' cannot be used together");
    }
    let stage = match (ours, theirs) {
        (true, _) => Some(2),
        (_, true) => Some(3),
        _ => None,
    };
    let dashes_paths = !paths.is_empty();
    if (dashes_paths || stage.is_some()) && Want::of(&track_args) != Want::Config {
        bail!("'--track' cannot be used with updating paths");
    }
    if dashes_paths || stage.is_some() {
        if create.is_some() || reset.is_some() || detach {
            bail!("'pyn checkout -- <paths>' cannot be used with switching branches");
        }
        let (tree_ish, paths) = match (names.as_slice(), dashes_paths) {
            ([], true) => (None, paths),
            ([one], true) => (Some(one.clone()), paths),
            _ => bail!("use `pyn checkout --ours` or `--theirs` with `-- <paths>`"),
        };
        let r = match tree_ish {
            Some(t) => Restore {
                source: Source::Tree(commit_of(git, &t)?),
                to_index: true,
                to_worktree: true,
                prune: false,
                stage,
                merge: false,
                ignore_skip,
            },
            None => Restore {
                source: Source::Index,
                to_index: false,
                to_worktree: true,
                prune: false,
                stage,
                merge: false,
                ignore_skip,
            },
        };
        let label = match &r.source {
            Source::Tree(c) => git.repo.abbreviate(c)?,
            Source::Index => "the index".into(),
        };
        let n = restore_paths(git, cwd, &paths, r)?;
        if !quiet {
            eprintln!(
                "Updated {n} path{} from {label}",
                if n == 1 { "" } else { "s" }
            );
        }
        return Ok(());
    }
    let want = Want::of(&track_args);
    let create = if want != Want::Config && create.is_none() && reset.is_none() {
        if detach {
            bail!("'--detach' cannot be used with '-t/--track'");
        }
        let [spec] = names.as_slice() else {
            bail!("--track needs a branch name");
        };
        Some(tracked_name(spec)?)
    } else {
        create
    };
    if let Some(name) = create {
        if detach {
            bail!("options '-b' and '--detach' cannot be used together");
        }
        let start = names.first().cloned();
        check_start(git, &name, names.len(), start.as_deref())?;
        let plan = track::plan(git, want, start.as_deref(), &name)?.map(|p| (name.clone(), p));
        return go_tracking(
            git,
            Go {
                landing: Landing::NewBranch { name, start },
                force,
                quiet,
            },
            plan,
        );
    }
    if let Some(name) = reset {
        if detach {
            bail!("'--detach' cannot be used with '-b/-B/--orphan'");
        }
        let start = names.first().cloned();
        check_start(git, &name, names.len(), start.as_deref())?;
        let plan = track::plan(git, want, start.as_deref(), &name)?.map(|p| (name.clone(), p));
        return go_tracking(
            git,
            Go {
                landing: Landing::ResetBranch { name, start },
                force,
                quiet,
            },
            plan,
        );
    }
    if detach && names.is_empty() {
        return go(
            git,
            Go {
                landing: Landing::Detach {
                    spec: "HEAD".into(),
                    explicit: true,
                },
                force,
                quiet,
            },
        );
    }
    let Some(first) = names.first() else {
        bail!("no branch or commit given; name one, or use `pyn status` to see where you are");
    };
    if names.len() > 1 {
        let (tree_ish, rest) = (first.clone(), &names[1..]);
        let source = Source::Tree(commit_of(git, &tree_ish)?);
        let label = match &source {
            Source::Tree(c) => git.repo.abbreviate(c)?,
            Source::Index => unreachable!("a commit was named"),
        };
        let n = restore_paths(
            git,
            cwd,
            rest,
            Restore {
                source,
                to_index: true,
                to_worktree: true,
                prune: false,
                stage: None,
                merge: false,
                ignore_skip,
            },
        )?;
        if !quiet {
            eprintln!(
                "Updated {n} path{} from {label}",
                if n == 1 { "" } else { "s" }
            );
        }
        return Ok(());
    }
    let name = expand_branch_name(git, first.clone()).unwrap_or_else(|_| first.clone());
    if !detach && git.branch_commit(&name)?.is_some() {
        return go(
            git,
            Go {
                landing: Landing::Branch(name),
                force,
                quiet,
            },
        );
    }
    if git.repo.resolve(&name).is_ok() {
        return go(
            git,
            Go {
                landing: Landing::Detach {
                    spec: name,
                    explicit: detach,
                },
                force,
                quiet,
            },
        );
    }
    let dashes = std::env::args_os().any(|a| a == "--");
    if !detach && super::dwim::enabled(git, crate::branch::flag(guess, no_guess)) {
        let file = !dashes && is_path(git, cwd, &name)?;
        match super::dwim::guess(git, &name) {
            Ok(Some(_)) if file => {
                return Err(Refused(format!(
                    "'{name}' could be both a local file and a tracking branch; use -- (and optionally --no-guess) to disambiguate"
                ))
                .into());
            }
            Ok(Some(g)) => return create_guessed(git, name, &g, force, quiet),
            Ok(None) => {}
            Err(_) if file => {}
            Err(e) => return Err(e),
        }
    }
    if dashes {
        bail!("invalid reference: {name}");
    }
    if !detach && is_path(git, cwd, &name)? {
        let n = restore_paths(
            git,
            cwd,
            std::slice::from_ref(&name),
            Restore {
                source: Source::Index,
                to_index: false,
                to_worktree: true,
                prune: false,
                stage: None,
                merge: false,
                ignore_skip,
            },
        )?;
        if !quiet {
            eprintln!(
                "Updated {n} path{} from the index",
                if n == 1 { "" } else { "s" }
            );
        }
        return Ok(());
    }
    Err(Refused(format!(
        "pathspec '{name}' did not match any file(s) known to git"
    ))
    .into())
}

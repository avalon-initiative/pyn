//! `pyn branch`, `pyn switch` and `pyn checkout` on a pyn server, and the workspace's current branch.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::transport::Method;
use anyhow::{Context, Result, bail};
use clap::Args;
use pyn_proto as api;

use crate::backend::Backend;
use crate::client::Api;
use crate::local_store::LocalStore;
use crate::localfile;
use crate::snapshot::{self, short};
use crate::staging::Index;
use crate::sync::{self, mode_name};
use crate::workspace::{FileState, Workspace, set_read_only};

/// The server's default branch, which a workspace is on until it switches.
pub const DEFAULT_BRANCH: &str = "main";

fn state_path(ws: &Workspace) -> PathBuf {
    ws.dir.join("local_only").join("state").join("branch")
}

/// The branch the workspace is on, if it has switched; `None` means the default branch.
pub fn current(ws: Option<&Workspace>) -> Option<String> {
    let text = std::fs::read_to_string(state_path(ws?)).ok()?;
    Some(text.trim().to_string()).filter(|b| !b.is_empty())
}

/// The name to show for the workspace's branch.
pub fn name(ws: Option<&Workspace>) -> String {
    current(ws).unwrap_or_else(|| DEFAULT_BRANCH.to_string())
}

fn set_current(ws: &Workspace, branch: &str) -> Result<()> {
    let path = state_path(ws);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, format!("{branch}\n"))
        .with_context(|| format!("writing {}", path.display()))
}

/// Prints which branch a workspace command is about, on standard output.
pub fn print_header(ws: Option<&Workspace>) {
    if ws.is_some() {
        println!("On branch {}", name(ws));
    }
}

/// Like `print_header`, on standard error, for output that scripts read.
pub fn note_branch(ws: Option<&Workspace>) {
    if ws.is_some() {
        eprintln!("On branch {}", name(ws));
    }
}

/// List, create, rename or delete branches.
#[derive(Debug, Args)]
pub struct BranchArgs {
    /// `<name> [<start>]` creates a branch; with -d or -D the branches to delete; with -m `[<old>] <new>`.
    pub(crate) names: Vec<String>,
    /// Delete a branch whose commits are all on the default branch.
    #[arg(short = 'd', long)]
    pub(crate) delete: bool,
    /// Delete a branch even when it has commits the default branch lacks.
    #[arg(short = 'D')]
    pub(crate) force_delete: bool,
    /// Rename a branch (the current one if only the new name is given).
    #[arg(short = 'm', long = "move")]
    pub(crate) rename: bool,
    /// Rename a branch even if the new name exists (a git repository only).
    #[arg(short = 'M')]
    pub(crate) force_rename: bool,
    /// Copy a branch, its reflog and its `branch.<name>.*` settings (a git repository only).
    #[arg(short = 'c', long)]
    pub(crate) copy: bool,
    /// Copy a branch even if the new name exists (a git repository only).
    #[arg(short = 'C')]
    pub(crate) force_copy: bool,
    /// Create or reset a branch even if it exists (a git repository only).
    #[arg(short = 'f', long)]
    pub(crate) force: bool,
    /// Make the branch track an upstream branch (a git repository only).
    #[arg(short = 'u', long = "set-upstream-to", value_name = "UPSTREAM")]
    pub(crate) set_upstream_to: Option<String>,
    /// Remove the branch's upstream information (a git repository only).
    #[arg(long)]
    pub(crate) unset_upstream: bool,
    /// List branches (the default with no arguments).
    #[arg(short = 'l', long)]
    pub(crate) list: bool,
    /// With the list, show each branch's commit and message.
    #[arg(short = 'v', long)]
    pub(crate) verbose: bool,
    /// Print the current branch's name.
    #[arg(long)]
    pub(crate) show_current: bool,
    /// List local and remote-tracking branches (a pyn server only).
    #[arg(short = 'a', long)]
    pub(crate) all: bool,
    /// List remote-tracking branches (a pyn server only).
    #[arg(short = 'r', long)]
    pub(crate) remotes: bool,
}

/// Switch the workspace to another branch.
#[derive(Debug, Args)]
pub struct SwitchArgs {
    /// The branch to switch to; with -c, where the new branch starts.
    pub(crate) start: Option<String>,
    /// Create the branch and switch to it.
    #[arg(short = 'c', long)]
    pub(crate) create: Option<String>,
    /// Create the branch, or reset it if it exists, and switch to it (a git repository only).
    #[arg(short = 'C', long = "force-create")]
    pub(crate) force_create: Option<String>,
    /// Switch to a new branch with no commits (a git repository only).
    #[arg(long)]
    pub(crate) orphan: Option<String>,
    /// Discard local changes that the switch would overwrite.
    #[arg(short = 'f', long, alias = "discard-changes")]
    pub(crate) force: bool,
    /// Check out a commit with a detached HEAD (a git repository only).
    #[arg(short = 'd', long)]
    pub(crate) detach: bool,
}

/// Switch branches, or restore files from the workspace's revisions or from another commit.
#[derive(Debug, Args)]
pub struct CheckoutArgs {
    /// A branch to switch to, or with `--` the branch or commit to take the files from; with -b, where the new
    /// branch starts.
    pub(crate) args: Vec<String>,
    /// Files to restore: to the revision the workspace has, or to the one the named commit has.
    #[arg(last = true)]
    pub(crate) paths: Vec<String>,
    /// Create the branch and switch to it.
    #[arg(short = 'b')]
    pub(crate) create: Option<String>,
    /// Discard local changes that the switch would overwrite.
    #[arg(short = 'f', long)]
    pub(crate) force: bool,
    /// Check out a commit with a detached HEAD (a git repository only).
    #[arg(long)]
    pub(crate) detach: bool,
    /// During a merge, take the current branch's version of the conflicted files.
    #[arg(long, conflicts_with = "theirs")]
    pub(crate) ours: bool,
    /// During a merge, take the merged-in version of the conflicted files.
    #[arg(long)]
    pub(crate) theirs: bool,
    /// Skip the typed confirmation before discarding an edit to an exclusive file.
    #[arg(long)]
    pub(crate) yes: bool,
}

fn branches(api: &Api) -> Result<Vec<api::Branch>> {
    api.send(api.get(&api.repo_route("/branches")?))?.json()
}

fn branch_route(api: &Api, name: &str) -> Result<String> {
    api.repo_route(&format!("/branches/{name}"))
}

/// A branch by name; `None` when the server has no such branch.
fn find(api: &Api, name: &str) -> Result<Option<api::Branch>> {
    match api.send(api.get(&branch_route(api, name)?)) {
        Ok(resp) => Ok(Some(resp.json()?)),
        Err(e) => match e.downcast_ref::<crate::client::ApiError>() {
            Some(err) if err.code == "branch_not_found" => Ok(None),
            _ => Err(e),
        },
    }
}

/// What the server takes as a start point: a branch name as it is, a commit reference as the commit's full id.
pub(crate) fn start_point(backend: &dyn Backend, api: &Api, spec: &str) -> Result<String> {
    if find(api, spec)?.is_some() {
        return Ok(spec.to_string());
    }
    match snapshot::parse_commit(spec) {
        Some(_) => snapshot::resolve(backend, spec),
        None => Ok(spec.to_string()),
    }
}

fn create(
    backend: &dyn Backend,
    api: &Api,
    ws: Option<&Workspace>,
    new: &str,
    start: Option<&str>,
) -> Result<api::Branch> {
    let start = match (start, current(ws)) {
        (Some(s), _) => Some(start_point(backend, api, s)?),
        (None, here) => here,
    };
    let body = api::CreateBranchRequest {
        name: new.to_string(),
        start,
    };
    api.send(
        api.request(Method::POST, &api.repo_route("/branches")?)
            .json(&body),
    )?
    .json()
}

pub fn branch_command(
    backend: &dyn Backend,
    ws: Option<&Workspace>,
    args: BranchArgs,
) -> Result<()> {
    let api = backend.pyn_server("branch")?;
    let BranchArgs {
        names,
        delete,
        force_delete,
        rename,
        list,
        verbose,
        show_current,
        all,
        remotes,
        force_rename,
        copy,
        force_copy,
        force,
        set_upstream_to,
        unset_upstream,
    } = args;
    if force_rename || copy || force_copy || force || set_upstream_to.is_some() || unset_upstream {
        crate::usage_error(
            "`pyn branch` -M, -c, -C, -f, -u and --unset-upstream need a git repository",
        );
    }
    if (all || remotes) && (delete || force_delete || rename || show_current || !names.is_empty()) {
        bail!("`pyn branch -a` and `-r` only list");
    }
    let modes = [delete || force_delete, rename, list || show_current].map(usize::from);
    if modes.iter().sum::<usize>() > 1 {
        bail!("choose one of --delete, --move and --list");
    }
    if show_current {
        println!("{}", name(ws));
        return Ok(());
    }
    if delete || force_delete {
        if names.is_empty() {
            bail!("name the branches to delete");
        }
        return delete_branches(api, ws, &names, force_delete);
    }
    if rename {
        let (old, new) = match names.as_slice() {
            [new] => (name(ws), new.clone()),
            [old, new] => (old.clone(), new.clone()),
            _ => bail!("`pyn branch -m` takes `[<old>] <new>`"),
        };
        let body = api::RenameBranchRequest { name: new.clone() };
        api.send(
            api.request(Method::PATCH, &branch_route(api, &old)?)
                .json(&body),
        )?;
        if let Some(w) = ws
            && name(ws) == old
        {
            set_current(w, &new)?;
        }
        return Ok(());
    }
    match names.as_slice() {
        [] => list_branches(backend, api, ws, verbose, Scope::of(all, remotes)),
        _ if list => bail!("`pyn branch --list` takes no branch names"),
        [new] => create(backend, api, ws, new, None).map(drop),
        [new, start] => create(backend, api, ws, new, Some(start)).map(drop),
        _ => crate::usage_error("`pyn branch` takes `<name> [<start>]`"),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Scope {
    Local,
    Remote,
    All,
}

impl Scope {
    fn of(all: bool, remotes: bool) -> Self {
        match (all, remotes) {
            (true, _) => Scope::All,
            (_, true) => Scope::Remote,
            _ => Scope::Local,
        }
    }
}

/// The server is the only remote, `origin`, and its branches are the same ones the workspace follows.
fn list_branches(
    backend: &dyn Backend,
    api: &Api,
    ws: Option<&Workspace>,
    verbose: bool,
    scope: Scope,
) -> Result<()> {
    let here = name(ws);
    let mut all = branches(api)?;
    all.sort_by(|a, b| a.name.cmp(&b.name));
    let mut rows: Vec<(char, String, &api::Branch)> = Vec::new();
    if scope != Scope::Remote {
        rows.extend(all.iter().map(|b| {
            let mark = if b.name == here { '*' } else { ' ' };
            (mark, b.name.clone(), b)
        }));
    }
    if scope != Scope::Local {
        let prefix = if scope == Scope::All {
            "remotes/origin"
        } else {
            "origin"
        };
        rows.extend(all.iter().map(|b| (' ', format!("{prefix}/{}", b.name), b)));
    }
    let width = rows
        .iter()
        .map(|(_, n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    for (mark, shown, b) in rows {
        if !verbose {
            println!("{mark} {shown}");
            continue;
        }
        let local = match ws {
            Some(w) if mark == '*' => local_tip(w, &here)?,
            _ => None,
        };
        let (id, subject) = match (&local, &b.commit) {
            (Some(l), _) => l.clone(),
            (None, Some(c)) => {
                let message = backend.commit(c)?.message;
                (
                    short(c).to_string(),
                    message.lines().next().unwrap_or_default().to_string(),
                )
            }
            (None, None) => ("-".to_string(), "(no commits yet)".to_string()),
        };
        println!("{mark} {shown:<width$}  {id}  {subject}");
    }
    Ok(())
}

/// The workspace branch's tip and subject when it has local commits the server lacks.
fn local_tip(ws: &Workspace, branch: &str) -> Result<Option<(String, String)>> {
    let Some(store) = LocalStore::open(ws) else {
        return Ok(None);
    };
    let ahead = store.unpublished(branch)?.ahead;
    let Some(tip) = store.tip(branch)?.filter(|_| ahead > 0) else {
        return Ok(None);
    };
    let message = String::from_utf8_lossy(&store.repo().commit(&tip)?.message).into_owned();
    let subject = message.lines().next().unwrap_or_default();
    Ok(Some((
        short(&tip.to_hex()).to_string(),
        format!("{subject} (local, {ahead} not yet published)"),
    )))
}

fn delete_branches(api: &Api, ws: Option<&Workspace>, names: &[String], force: bool) -> Result<()> {
    let here = name(ws);
    for n in names {
        if ws.is_some() && *n == here {
            bail!(
                "cannot delete branch '{n}': this workspace is on it; switch to another branch first"
            );
        }
        let Some(found) = find(api, n)? else {
            bail!("branch '{n}' not found");
        };
        let mut req = api.request(Method::DELETE, &branch_route(api, n)?);
        if force {
            req = req.query(&[("force", true)]);
        }
        api.send(req).map_err(|e| not_merged_hint(e, n))?;
        let was = found
            .commit
            .map_or_else(String::new, |c| format!(" (was {})", short(&c)));
        println!("Deleted branch {n}{was}.");
    }
    Ok(())
}

fn not_merged_hint(err: anyhow::Error, branch: &str) -> anyhow::Error {
    match err.downcast_ref::<crate::client::ApiError>() {
        Some(e) if e.code == "branch_not_merged" => anyhow::anyhow!(
            "the branch '{branch}' is not fully merged into the default branch; \
             if you are sure you want to delete it, run `pyn branch -D {branch}`"
        ),
        _ => err,
    }
}

pub fn switch_command(
    backend: &dyn Backend,
    ws: Option<&Workspace>,
    args: SwitchArgs,
) -> Result<()> {
    let api = backend.pyn_server("switch")?;
    if args.force_create.is_some() || args.orphan.is_some() {
        crate::usage_error("`pyn switch` -C and --orphan need a git repository");
    }
    if args.detach {
        bail!(
            "`pyn switch --detach` needs a git repository; a pyn server's branches always have a head"
        );
    }
    let ws = ws.context("`pyn switch` works inside a workspace; run `pyn clone` first")?;
    match (args.create, args.start) {
        (Some(new), start) => {
            let made = create(backend, api, Some(ws), &new, start.as_deref())?;
            go(backend, api, ws, &made.name, args.force)
        }
        (None, Some(target)) => {
            if find(api, &target)?.is_none() {
                bail!(
                    "invalid reference: {target}; `pyn switch` takes a branch (see `pyn branch`)"
                );
            }
            go(backend, api, ws, &target, args.force)
        }
        (None, None) => bail!("missing branch name; see `pyn branch`"),
    }
}

pub fn checkout_command(
    backend: &dyn Backend,
    ws: Option<&Workspace>,
    cwd: &Path,
    args: CheckoutArgs,
    rp: &dyn Fn(&str) -> Result<String>,
) -> Result<()> {
    let api = backend.pyn_server("checkout")?;
    if args.detach {
        bail!(
            "`pyn checkout --detach` needs a git repository; a pyn server's branches always have a head"
        );
    }
    let ws = ws.context(
        "`pyn checkout` switches branches and restores files inside a workspace; run `pyn clone` first. \
         To take an exclusive lock use `pyn lock <path>`",
    )?;
    let CheckoutArgs {
        args: before,
        paths,
        create: new,
        force,
        detach: _,
        ours,
        theirs,
        yes,
    } = args;
    if ours || theirs {
        if paths.is_empty() || new.is_some() {
            bail!("`pyn checkout --ours` and `--theirs` take paths after `--`");
        }
        let named = paths.iter().map(|p| rp(p)).collect::<Result<Vec<_>>>()?;
        return crate::merging::take_side(backend, api, ws, theirs, &named);
    }
    if !paths.is_empty() {
        if new.is_some() {
            bail!("`pyn checkout -b` cannot be combined with paths");
        }
        let source = match before.as_slice() {
            [] => None,
            [one] => Some(one.as_str()),
            _ => bail!("`pyn checkout <commit> -- <paths>` takes one commit"),
        };
        return restore_paths(backend, ws, cwd, source, &paths, yes, rp);
    }
    if let Some(new) = new {
        if before.len() > 1 {
            bail!("`pyn checkout -b <name>` takes at most one start point");
        }
        let made = create(
            backend,
            api,
            Some(ws),
            &new,
            before.first().map(String::as_str),
        )?;
        return go(backend, api, ws, &made.name, force);
    }
    match before.as_slice() {
        [] => bail!("missing branch name; see `pyn branch`"),
        [target] if find(api, target)?.is_some() => go(backend, api, ws, target, force),
        [target] if Path::new(target).exists() || ws.root.join(target).exists() => {
            restore_paths(backend, ws, cwd, None, &before, yes, rp)
        }
        [target] if snapshot::parse_commit(target).is_some() => bail!(
            "'{target}' is a commit, not a branch; pyn has no detached HEAD. \
             Create a branch there with `pyn checkout -b <name> {target}`, or bring the files to it with \
             `pyn update --revision {target}`"
        ),
        [target] => bail!(
            "pathspec '{target}' did not match any branch or file; to take an exclusive lock use `pyn lock <path>`"
        ),
        _ => bail!("`pyn checkout` takes one branch; name files after `--`"),
    }
}

/// What each path needs, and the paths whose local changes are in the way with why.
type Plan = (BTreeMap<String, Step>, Vec<(String, &'static str)>);

/// What switching does to one path.
enum Step {
    Fetch(api::SnapshotEntry),
    Adopt(api::SnapshotEntry),
    Remove,
}

fn state_of(entry: &api::SnapshotEntry) -> FileState {
    FileState {
        revision: entry.revision,
        hash: entry.content.clone(),
        legacy: false,
        mode: mode_name(entry.mode).to_string(),
        file_mode: entry.file_mode,
    }
}

fn same_content(known: &FileState, want: &api::SnapshotEntry) -> bool {
    !known.legacy
        && known.hash == want.content
        && !localfile::mode_differs(known.file_mode, want.file_mode)
}

/// What each path needs for the workspace to match `snapshot`, and the paths whose local changes are in the way.
fn plan(
    ws: &Workspace,
    state: &BTreeMap<String, FileState>,
    index: &Index,
    snapshot: &[api::SnapshotEntry],
) -> Result<Plan> {
    let wanted: BTreeMap<&str, &api::SnapshotEntry> =
        snapshot.iter().map(|e| (e.path.as_str(), e)).collect();
    let paths: BTreeSet<&str> = state
        .keys()
        .map(String::as_str)
        .chain(wanted.keys().copied())
        .collect();
    ws.check_paths(&paths)?;
    let (mut steps, mut conflicts) = (BTreeMap::new(), Vec::new());
    for path in paths {
        let (known, want) = (state.get(path), wanted.get(path).copied());
        let target = ws.abs(path);
        let on_disk = localfile::exists(&target);
        let modified = match known {
            Some(k) if on_disk => {
                let (file_mode, bytes) = localfile::read(&target)?;
                k.changed(file_mode, &bytes)
            }
            Some(_) => true,
            None => false,
        };
        match (known, want) {
            (Some(k), Some(w)) if same_content(k, w) => {
                steps.insert(path.to_string(), Step::Adopt(w.clone()));
            }
            (Some(_), Some(w)) => {
                if modified {
                    conflicts.push((path.to_string(), "local changes would be overwritten"));
                }
                steps.insert(path.to_string(), Step::Fetch(w.clone()));
            }
            (Some(_), None) => {
                if modified && on_disk {
                    conflicts.push((path.to_string(), "local changes would be lost"));
                }
                steps.insert(path.to_string(), Step::Remove);
            }
            (None, Some(w)) if on_disk => {
                let (file_mode, bytes) = localfile::read(&target)?;
                let same = crate::workspace::hash_of(&bytes) == w.content
                    && !localfile::mode_differs(w.file_mode, file_mode);
                if same {
                    steps.insert(path.to_string(), Step::Adopt(w.clone()));
                } else {
                    conflicts.push((path.to_string(), "an untracked file would be overwritten"));
                    steps.insert(path.to_string(), Step::Fetch(w.clone()));
                }
            }
            (None, Some(w)) => {
                steps.insert(path.to_string(), Step::Fetch(w.clone()));
            }
            (None, None) => {}
        }
    }
    for path in index.entries.keys() {
        let moves = matches!(steps.get(path), Some(Step::Fetch(_) | Step::Remove));
        if moves && !conflicts.iter().any(|(p, _)| p == path) {
            conflicts.push((path.clone(), "staged changes would be lost"));
        }
    }
    Ok((steps, conflicts))
}

fn remove_empty_parents(ws: &Workspace, path: &str) {
    let mut dir = ws.abs(path);
    while dir.pop() && dir != ws.root && dir.starts_with(&ws.root) {
        if std::fs::remove_dir(&dir).is_err() {
            break;
        }
    }
}

/// Brings the workspace to the head of `target`, refusing first if local changes are in the way.
fn go(backend: &dyn Backend, api: &Api, ws: &Workspace, target: &str, force: bool) -> Result<()> {
    crate::merging::refuse_during_merge(ws, "switch")?;
    let already = name(Some(ws)) == target;
    if !already
        && let Some(store) = LocalStore::open(ws)
        && store.unpublished(&name(Some(ws)))?.ahead > 0
    {
        bail!(
            "branch '{}' has local commits that are not yet published to the server; \
             switching away would leave the workspace without them, so nothing was changed",
            name(Some(ws))
        );
    }
    let found = find(api, target)?.with_context(|| format!("branch '{target}' not found"))?;
    let (_, snap) = match &found.commit {
        Some(head) => backend.snapshot(Some(head))?,
        None => (None, Vec::new()),
    };
    let mut state = ws.load_state()?;
    let mut index = Index::load(ws)?;
    let (steps, conflicts) = plan(ws, &state, &index, &snap)?;
    if !conflicts.is_empty() && !force {
        let mut message = String::from("local changes would be overwritten by the switch:\n");
        for (path, why) in &conflicts {
            message.push_str(&format!("\t{path} ({why})\n"));
        }
        message.push_str(
            "commit them, put them back with `pyn checkout -- <path>`, or discard them with --force; nothing was changed",
        );
        bail!(message);
    }
    let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
    let me = backend.whoami()?;
    carry_staged(&mut index, &steps);
    let applied = apply(backend, ws, &mut state, steps, &locks, &me);
    ws.save_state(&state)?;
    applied?;
    index.save(ws)?;
    set_current(ws, target)?;
    if already {
        eprintln!("Already on '{target}'");
    } else {
        eprintln!("Switched to branch '{target}'");
    }
    Ok(())
}

/// Staged changes to files both branches share stay staged, on the new branch's revision; the rest are dropped, which only
/// happens when the switch is forced.
fn carry_staged(index: &mut Index, steps: &BTreeMap<String, Step>) {
    index.entries.retain(|path, entry| match steps.get(path) {
        Some(Step::Adopt(w)) => {
            entry.base_revision = Some(w.revision);
            true
        }
        Some(Step::Fetch(_) | Step::Remove) => false,
        None => true,
    });
}

fn apply(
    backend: &dyn Backend,
    ws: &Workspace,
    state: &mut BTreeMap<String, FileState>,
    steps: BTreeMap<String, Step>,
    locks: &[api::Lock],
    me: &str,
) -> Result<()> {
    for (path, step) in steps {
        match step {
            Step::Adopt(w) => {
                state.insert(path, state_of(&w));
            }
            Step::Remove => {
                let target = ws.dest(&path)?;
                set_read_only(&target, false)?;
                let _ = std::fs::remove_file(&target);
                remove_empty_parents(ws, &path);
                state.remove(&path);
            }
            Step::Fetch(w) => {
                let entry = file_entry(&w, locks);
                let fetched = sync::fetch(backend, ws, &entry, w.revision, me)?;
                state.insert(path, fetched);
            }
        }
    }
    Ok(())
}

fn file_entry(w: &api::SnapshotEntry, locks: &[api::Lock]) -> api::FileEntry {
    api::FileEntry {
        path: w.path.clone(),
        mode: w.mode,
        revision: Some(w.revision),
        file_mode: w.file_mode,
        lock: locks.iter().find(|l| l.path == w.path).cloned(),
    }
}

/// Puts files back: to the revision the workspace has, or to the one `source` (a branch or commit) has.
fn restore_paths(
    backend: &dyn Backend,
    ws: &Workspace,
    cwd: &Path,
    source: Option<&str>,
    paths: &[String],
    yes: bool,
    rp: &dyn Fn(&str) -> Result<String>,
) -> Result<()> {
    let api = backend.pyn_server("checkout")?;
    let state = ws.load_state()?;
    let snap = match source {
        Some(spec) => {
            let commit = match find(api, spec)? {
                Some(b) => b.commit.context("that branch has no commits yet")?,
                None => snapshot::resolve(backend, spec)?,
            };
            Some(backend.snapshot(Some(&commit))?.1)
        }
        None => None,
    };
    let known: BTreeSet<&str> = match &snap {
        Some(s) => s.iter().map(|e| e.path.as_str()).collect(),
        None => state.keys().map(String::as_str).collect(),
    };
    let mut chosen = BTreeSet::new();
    for arg in paths {
        let prefix = if arg == "." && cwd == ws.root {
            String::new()
        } else {
            rp(arg)?
        };
        let before = chosen.len();
        for p in &known {
            let inside = prefix.is_empty()
                || *p == prefix
                || p.strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
            if inside {
                chosen.insert(p.to_string());
            }
        }
        if chosen.len() == before && !chosen.iter().any(|c| *c == prefix) {
            bail!("pathspec '{arg}' did not match any file known to pyn");
        }
    }
    if snap.is_none() {
        let index = Index::load(ws)?;
        if let Some(staged) = chosen.iter().find(|p| index.entries.contains_key(*p)) {
            bail!(
                "{staged} is staged; unstage it with `pyn restore --staged {staged}` first, then restore the file"
            );
        }
    }
    ws.check_paths(&chosen)?;
    let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
    let me = backend.whoami()?;
    let mut targets = Vec::new();
    for path in &chosen {
        let (entry, revision) = match &snap {
            Some(s) => {
                let w = s.iter().find(|e| &e.path == path).expect("chosen from it");
                (file_entry(w, &locks), w.revision)
            }
            None => {
                let k = &state[path];
                let mode = if k.mode == "exclusive" {
                    api::Mode::Exclusive
                } else {
                    api::Mode::Shared
                };
                let entry = api::FileEntry {
                    path: path.clone(),
                    mode,
                    revision: Some(k.revision),
                    file_mode: k.file_mode,
                    lock: locks.iter().find(|l| &l.path == path).cloned(),
                };
                (entry, k.revision)
            }
        };
        targets.push((entry, revision));
    }
    let edited = edited_exclusive(ws, &state, &targets)?;
    if !edited.is_empty() && !yes {
        confirm_discard(&edited)?;
    }
    for (entry, revision) in &targets {
        sync::fetch(backend, ws, entry, *revision, &me)?;
    }
    Ok(())
}

/// The exclusive paths among `targets` whose local edits a restore would overwrite, locked by the user or not.
fn edited_exclusive(
    ws: &Workspace,
    state: &BTreeMap<String, FileState>,
    targets: &[(api::FileEntry, u64)],
) -> Result<Vec<String>> {
    let mut edited = Vec::new();
    for (entry, _) in targets {
        let target = ws.abs(&entry.path);
        let Some(known) = state.get(&entry.path) else {
            continue;
        };
        if entry.mode != api::Mode::Exclusive || !localfile::exists(&target) {
            continue;
        }
        let (file_mode, bytes) = localfile::read(&target)?;
        if known.changed(file_mode, &bytes) {
            edited.push(entry.path.clone());
        }
    }
    Ok(edited)
}

/// Names the paths that would lose edits and asks for the path (or the count) typed back, as `restore` does.
fn confirm_discard(edited: &[String]) -> Result<()> {
    eprintln!("These exclusive files have local edits that cannot be merged or recovered:");
    for path in edited {
        eprintln!("  {path}");
    }
    let expected = match edited {
        [one] => one.clone(),
        many => many.len().to_string(),
    };
    let what = if edited.len() == 1 {
        "path"
    } else {
        "number of files"
    };
    eprint!("Type the {what} to discard them, or pass --yes: ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim() != expected {
        bail!("not confirmed; nothing was changed");
    }
    Ok(())
}

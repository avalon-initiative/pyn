//! `pyn worktree` on a git repository: linked work trees kept in git's layout (`.git/worktrees/<name>/` with `HEAD`,
//! `gitdir`, `commondir` and `locked`, and a `.git` file in the work tree), so git and pyn can share them.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use pyn_git::{Ref, RefStore};
use pyn_object::ObjectId;

use super::GitHome;
use super::plumbing::{abbrev, default_abbrev, emit_bytes, usage};
use super::reflog::limit;
use super::switch::subject;
use super::worktree::{Mode, move_tree};

const BRANCHES: &str = "refs/heads/";
const GITDIR_MISSING: &str = "gitdir file points to non-existent location";

/// Manage multiple work trees attached to the same repository.
#[derive(Debug, Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Create a work tree at <path> for a branch or commit.
    Add(AddArgs),
    /// List the repository's work trees.
    List(ListArgs),
    /// Lock a work tree so it is not pruned, moved or removed.
    Lock(LockArgs),
    /// Unlock a work tree.
    Unlock(TreeArg),
    /// Move a work tree to a new location.
    Move(MoveArgs),
    /// Remove the administrative files of work trees whose folder is gone.
    Prune(PruneArgs),
    /// Remove a work tree.
    Remove(RemoveArgs),
    /// Fix the links between the repository and its work trees after either was moved by hand.
    Repair(RepairArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Create the work tree even if the branch is checked out elsewhere or the path is registered but missing.
    #[arg(short, long, action = clap::ArgAction::Count)]
    force: u8,
    /// Create a new branch at the commit.
    #[arg(short = 'b', value_name = "new-branch")]
    new_branch: Option<String>,
    /// Create the branch, or reset it when it exists.
    #[arg(short = 'B', value_name = "new-branch")]
    reset_branch: Option<String>,
    /// Detach HEAD at the commit.
    #[arg(short, long)]
    detach: bool,
    /// Do not populate the work tree.
    #[arg(long)]
    no_checkout: bool,
    /// Populate the work tree (the default).
    #[arg(long, overrides_with = "no_checkout")]
    checkout: bool,
    /// Lock the work tree after creating it.
    #[arg(long)]
    lock: bool,
    /// Why the work tree is locked (needs --lock).
    #[arg(long, requires = "lock")]
    reason: Option<String>,
    /// Make an unborn branch with an empty work tree.
    #[arg(long)]
    orphan: bool,
    /// Print nothing.
    #[arg(short, long)]
    quiet: bool,
    path: String,
    commit_ish: Option<String>,
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Machine-readable output.
    #[arg(long)]
    porcelain: bool,
    /// End each porcelain field with a NUL instead of a newline.
    #[arg(short = 'z')]
    nul: bool,
    /// Also show why a work tree is locked or prunable.
    #[arg(short, long)]
    verbose: bool,
    /// Treat a work tree whose folder went missing before this time as prunable.
    #[arg(long)]
    expire: Option<String>,
}

#[derive(Debug, Args)]
struct LockArgs {
    /// Why the work tree is locked.
    #[arg(long)]
    reason: Option<String>,
    worktree: String,
}

#[derive(Debug, Args)]
struct TreeArg {
    worktree: String,
}

#[derive(Debug, Args)]
struct MoveArgs {
    /// Move a locked work tree when given twice.
    #[arg(short, long, action = clap::ArgAction::Count)]
    force: u8,
    worktree: String,
    new_path: String,
}

#[derive(Debug, Args)]
struct PruneArgs {
    /// Show what would be removed.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Report each removal.
    #[arg(short, long)]
    verbose: bool,
    /// Only prune work trees whose folder went missing before this time.
    #[arg(long)]
    expire: Option<String>,
}

#[derive(Debug, Args)]
struct RemoveArgs {
    /// Remove with local changes, and a locked work tree when given twice.
    #[arg(short, long, action = clap::ArgAction::Count)]
    force: u8,
    worktree: String,
}

#[derive(Debug, Args)]
struct RepairArgs {
    paths: Vec<String>,
}

enum HeadState {
    Branch(String, Option<ObjectId>),
    Detached(ObjectId),
}

struct Tree {
    path: PathBuf,
    /// The admin folder under `worktrees/`; `None` for the main work tree.
    name: Option<String>,
    git_dir: PathBuf,
    head: HeadState,
    locked: Option<String>,
    prunable: Option<&'static str>,
    bare: bool,
}

pub(super) fn worktree(git: &GitHome, cwd: &Path, args: WorktreeArgs) -> Result<()> {
    match args.action {
        Action::Add(a) => add(git, cwd, a),
        Action::List(a) => list(git, a),
        Action::Lock(a) => lock(git, cwd, a),
        Action::Unlock(a) => unlock(git, cwd, a),
        Action::Move(a) => move_tree_to(git, cwd, a),
        Action::Prune(a) => prune(git, a),
        Action::Remove(a) => remove(git, cwd, a),
        Action::Repair(a) => repair(git, cwd, a),
    }
}

/// `p` made absolute against `cwd` and lexically normalised, with the folders that exist resolved.
fn absolute(cwd: &Path, p: &str) -> PathBuf {
    let joined = cwd.join(p);
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    match out.file_name().map(ToOwned::to_owned) {
        Some(name) => match out.parent().map(std::fs::canonicalize) {
            Some(Ok(parent)) => parent.join(name),
            _ => out,
        },
        None => out,
    }
}

fn common_dir(git: &GitHome) -> &Path {
    git.repo.git().common_dir()
}

fn refs_of(git: &GitHome, git_dir: &Path) -> RefStore {
    RefStore::new(
        git_dir.to_path_buf(),
        common_dir(git).to_path_buf(),
        git.algorithm(),
    )
}

fn head_of(git: &GitHome, git_dir: &Path) -> Result<HeadState> {
    let refs = refs_of(git, git_dir);
    Ok(match refs.head()? {
        Ref::Direct(id) => HeadState::Detached(id),
        Ref::Symbolic(target) => {
            let id = refs.resolve(&target)?;
            HeadState::Branch(target, id)
        }
    })
}

fn read_trimmed(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim_end().to_string())
}

fn is_bare(git: &GitHome) -> bool {
    git.config().get_bool("core.bare").ok().flatten() == Some(true)
}

fn main_tree(git: &GitHome) -> Result<Tree> {
    let common = common_dir(git);
    let bare = is_bare(git);
    let path = if bare {
        common.to_path_buf()
    } else {
        common.parent().unwrap_or(common).to_path_buf()
    };
    Ok(Tree {
        path,
        name: None,
        git_dir: common.to_path_buf(),
        head: head_of(git, common)?,
        locked: None,
        prunable: None,
        bare,
    })
}

/// Why the admin folder `dir` is stale, if it is: git's reasons, with a gitdir older than `expire` required.
fn stale(dir: &Path, expire: i64) -> Option<&'static str> {
    if dir.join("locked").exists() {
        return None;
    }
    let gitdir = dir.join("gitdir");
    let Some(target) = read_trimmed(&gitdir) else {
        return Some("gitdir file does not exist");
    };
    if target.is_empty() {
        return Some("invalid gitdir file");
    }
    if Path::new(&target).exists() {
        return None;
    }
    let modified = std::fs::metadata(&gitdir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    (modified <= expire).then_some(GITDIR_MISSING)
}

fn admin_dirs(git: &GitHome) -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(common_dir(git).join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    found.sort();
    found
}

/// The main work tree followed by the linked ones that have a readable `gitdir`.
fn trees(git: &GitHome, expire: i64) -> Result<Vec<Tree>> {
    let mut all = vec![main_tree(git)?];
    for (name, dir) in admin_dirs(git) {
        let Some(target) = read_trimmed(&dir.join("gitdir")).filter(|t| !t.is_empty()) else {
            continue;
        };
        let path = Path::new(&target)
            .parent()
            .map_or_else(|| PathBuf::from(&target), Path::to_path_buf);
        all.push(Tree {
            path,
            git_dir: dir.clone(),
            head: head_of(git, &dir)?,
            locked: read_trimmed(&dir.join("locked"))
                .or_else(|| dir.join("locked").exists().then(String::new)),
            prunable: stale(&dir, expire),
            name: Some(name),
            bare: false,
        });
    }
    Ok(all)
}

fn find<'a>(all: &'a [Tree], cwd: &Path, spec: &str) -> Result<&'a Tree> {
    let abs = absolute(cwd, spec);
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let want = canon(&abs);
    if let Some(t) = all.iter().find(|t| canon(&t.path) == want) {
        return Ok(t);
    }
    let suffixed: Vec<&Tree> = all
        .iter()
        .filter(|t| !spec.is_empty() && t.path.ends_with(spec))
        .collect();
    match suffixed.as_slice() {
        [one] => Ok(one),
        _ => bail!("'{spec}' is not a working tree"),
    }
}

/// What a `locked` file holds: the reason and a newline, or nothing.
fn reason_text(reason: Option<&str>) -> String {
    reason.map_or_else(String::new, |r| format!("{r}\n"))
}

fn lock_note(reason: &str) -> String {
    if reason.is_empty() {
        String::new()
    } else {
        format!(", reason: {reason}")
    }
}

fn list(git: &GitHome, args: ListArgs) -> Result<()> {
    if args.nul && !args.porcelain {
        bail!("the option '-z' requires '--porcelain'");
    }
    let expire = match &args.expire {
        Some(t) => limit(t)?,
        None => i64::MAX,
    };
    let all = trees(git, expire)?;
    let zero = git.zero_id();
    let mut out = String::new();
    let end = if args.nul { '\0' } else { '\n' };
    if args.porcelain {
        for t in &all {
            let _ = write!(out, "worktree {}{end}", t.path.display());
            if t.bare {
                let _ = write!(out, "bare{end}");
            } else {
                match &t.head {
                    HeadState::Branch(r, id) => {
                        let _ = write!(out, "HEAD {}{end}", id.unwrap_or(zero));
                        let _ = write!(out, "branch {r}{end}");
                    }
                    HeadState::Detached(id) => {
                        let _ = write!(out, "HEAD {id}{end}detached{end}");
                    }
                }
            }
            if let Some(reason) = &t.locked {
                if reason.is_empty() {
                    let _ = write!(out, "locked{end}");
                } else {
                    let _ = write!(out, "locked {reason}{end}");
                }
            }
            if let Some(reason) = t.prunable {
                let _ = write!(out, "prunable {reason}{end}");
            }
            out.push(end);
        }
        emit_bytes(out.as_bytes());
        return Ok(());
    }
    let width = all
        .iter()
        .map(|t| t.path.as_os_str().len())
        .max()
        .unwrap_or(0);
    let min = default_abbrev(git);
    for t in &all {
        let _ = write!(out, "{:<width$}", t.path.display());
        if t.bare {
            out.push_str(" (bare)");
        } else {
            let (id, label) = match &t.head {
                HeadState::Branch(r, id) => (
                    id.unwrap_or(zero),
                    format!("[{}]", r.strip_prefix(BRANCHES).unwrap_or(r)),
                ),
                HeadState::Detached(id) => (*id, "(detached HEAD)".to_string()),
            };
            let short = if id == zero {
                "0".repeat(min)
            } else {
                abbrev(git, &id, min)
            };
            let _ = write!(out, " {short} {label}");
        }
        let mut notes = String::new();
        for (what, reason) in [("locked", t.locked.as_deref()), ("prunable", t.prunable)] {
            let Some(reason) = reason else { continue };
            if args.verbose && !reason.is_empty() {
                let _ = write!(notes, "\n\t{what}: {reason}");
            } else {
                let _ = write!(out, " {what}");
            }
        }
        out.push_str(&notes);
        out.push('\n');
    }
    emit_bytes(out.as_bytes());
    Ok(())
}

fn add(git: &GitHome, cwd: &Path, args: AddArgs) -> Result<()> {
    let created = [&args.new_branch, &args.reset_branch]
        .iter()
        .filter(|b| b.is_some())
        .count();
    if created + usize::from(args.detach) > 1 {
        usage("options '-b', '-B', and '--detach' cannot be used together");
    }
    if args.orphan {
        if args.detach || args.reset_branch.is_some() {
            usage("options '--orphan' and '--detach' or '-B' cannot be used together");
        }
        if args.commit_ish.is_some() {
            bail!("options '--orphan' and '<commit-ish>' cannot be used together");
        }
    }
    let path = absolute(cwd, &args.path);
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .with_context(|| format!("'{}' is not a usable path", args.path))?;
    if path.exists() && (!path.is_dir() || std::fs::read_dir(&path)?.next().is_some()) {
        bail!("'{}' already exists", path.display());
    }
    let force = args.force > 0;
    let (new_name, is_reset) = match (&args.new_branch, &args.reset_branch) {
        (Some(n), _) => (Some(n.clone()), false),
        (_, Some(n)) => (Some(n.clone()), true),
        _ => (None, false),
    };

    // What HEAD of the new work tree is to be: a branch (existing or new) or a commit.
    let mut branch: Option<String> = None;
    let mut start: Option<ObjectId> = None;
    let note;
    let mut label = args.commit_ish.clone();
    if let Some(name) = &new_name {
        GitHome::check_branch_name(name)?;
        let existing = git.branch_commit(name)?;
        if existing.is_some() && !is_reset {
            bail!("a branch named '{name}' already exists");
        }
        if existing.is_none() {
            git.check_ref_room(&GitHome::branch_ref(name))?;
        }
        if let Some(elsewhere) = git.checked_out_path(name)?
            && is_reset
            && !force
        {
            bail!(
                "'{name}' is already used by worktree at '{}'",
                elsewhere.display()
            );
        }
        let spec = args.commit_ish.as_deref().unwrap_or("HEAD");
        start = if args.orphan {
            None
        } else {
            let id = git
                .repo
                .resolve(spec)
                .map_err(|_| anyhow::anyhow!("invalid reference: {spec}"))?;
            Some(id)
        };
        note = match existing {
            Some(old) => format!("resetting branch '{name}'; was at {}", abbrev(git, &old, 7)),
            None => format!("new branch '{name}'"),
        };
        branch = Some(name.clone());
    } else if args.orphan {
        let name = base.clone();
        GitHome::check_branch_name(&name)?;
        if git.branch_commit(&name)?.is_some() {
            bail!("a branch named '{name}' already exists");
        }
        note = format!("new branch '{name}'");
        branch = Some(name);
    } else if args.detach
        || args
            .commit_ish
            .as_deref()
            .is_some_and(|c| git.branch_commit(c).ok().flatten().is_none())
    {
        let spec = args.commit_ish.as_deref().unwrap_or("HEAD");
        let id = git
            .repo
            .resolve(spec)
            .map_err(|_| anyhow::anyhow!("invalid reference: {spec}"))?;
        start = Some(id);
        note = format!("detached HEAD {}", abbrev(git, &id, 7));
    } else {
        let name = args.commit_ish.clone().unwrap_or_else(|| base.clone());
        match git.branch_commit(&name)? {
            Some(id) => {
                if !force && let Some(elsewhere) = git.checked_out_path(&name)? {
                    bail!(
                        "'{name}' is already used by worktree at '{}'",
                        elsewhere.display()
                    );
                }
                start = Some(id);
                note = format!("checking out '{name}'");
            }
            None => {
                GitHome::check_branch_name(&name)?;
                git.check_ref_room(&GitHome::branch_ref(&name))?;
                let id = git
                    .repo
                    .resolve("HEAD")
                    .map_err(|_| anyhow::anyhow!("invalid reference: HEAD"))?;
                start = Some(id);
                note = format!("new branch '{name}'");
                label = None;
            }
        }
        branch = Some(name);
    }

    let commit = start;
    let tree = match &commit {
        Some(id) => Some(git.repo.commit(id)?.tree),
        None => None,
    };
    if !args.quiet {
        eprintln!("Preparing worktree ({note})");
    }

    let admin_root = common_dir(git).join("worktrees");
    std::fs::create_dir_all(&admin_root)?;
    let mut name = base.clone();
    let mut n = 0;
    while admin_root.join(&name).exists() {
        n += 1;
        name = format!("{base}{n}");
    }
    let admin = admin_root.join(&name);
    let made_path = !path.exists();
    let rollback = |admin: &Path| {
        let _ = std::fs::remove_dir_all(admin);
        if made_path {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(path.join(".git"));
        }
        let _ = std::fs::remove_dir(&admin_root);
    };
    let built = (|| -> Result<()> {
        std::fs::create_dir(&admin)?;
        std::fs::create_dir_all(&path)?;
        std::fs::write(admin.join("commondir"), "../..\n")?;
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", path.join(".git").display()),
        )?;
        std::fs::write(path.join(".git"), format!("gitdir: {}\n", admin.display()))?;
        let head = match (&branch, &commit) {
            (Some(b), _) => format!("ref: {BRANCHES}{b}\n"),
            (None, Some(id)) => format!("{id}\n"),
            (None, None) => unreachable!("a detached work tree has a commit"),
        };
        std::fs::write(admin.join("HEAD"), head)?;
        if !args.no_checkout
            && let (Some(tree), false) = (&tree, args.orphan)
        {
            {
                let linked = GitHome::open_at(&path)?;
                let opened = linked.open_index()?;
                if let Err(blocked) = move_tree(&linked, &opened, None, Some(tree), Mode::Force)? {
                    blocked.print("checkout", "");
                    bail!("could not populate the work tree");
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = built {
        rollback(&admin);
        return Err(e);
    }

    if let (Some(b), Some(id)) = (&branch, &commit) {
        let full = GitHome::branch_ref(b);
        let from = label.unwrap_or_else(|| "HEAD".into());
        let old = git.branch_commit(b)?;
        match (&old, is_reset) {
            (None, _) => git.create_ref(&full, id, &format!("branch: Created from {from}"))?,
            (Some(o), true) if o != id => {
                git.move_ref(&full, Some(o), id, &format!("branch: Reset to {from}"))?
            }
            _ => {}
        }
    }
    if args.lock {
        std::fs::write(admin.join("locked"), reason_text(args.reason.as_deref()))?;
    }
    if !args.quiet
        && !args.no_checkout
        && !args.orphan
        && let Some(id) = &commit
    {
        eprintln!(
            "HEAD is now at {} {}",
            abbrev(git, id, 7),
            subject(git, id)?
        );
    }
    Ok(())
}

fn linked<'a>(all: &'a [Tree], cwd: &Path, spec: &str) -> Result<&'a Tree> {
    let t = find(all, cwd, spec)?;
    if t.name.is_none() {
        bail!("The main working tree cannot be locked or unlocked");
    }
    Ok(t)
}

fn lock(git: &GitHome, cwd: &Path, args: LockArgs) -> Result<()> {
    let all = trees(git, i64::MAX)?;
    let t = linked(&all, cwd, &args.worktree)?;
    if let Some(reason) = &t.locked {
        bail!("'{}' is already locked{}", args.worktree, lock_note(reason));
    }
    std::fs::write(
        t.git_dir.join("locked"),
        reason_text(args.reason.as_deref()),
    )?;
    Ok(())
}

fn unlock(git: &GitHome, cwd: &Path, args: TreeArg) -> Result<()> {
    let all = trees(git, i64::MAX)?;
    let t = linked(&all, cwd, &args.worktree)?;
    if t.locked.is_none() {
        bail!("'{}' is not locked", args.worktree);
    }
    std::fs::remove_file(t.git_dir.join("locked"))?;
    Ok(())
}

/// Refuses a locked work tree unless `force` is at least 2.
fn refuse_locked(t: &Tree, verb: &str, force: u8) -> Result<()> {
    if let Some(reason) = &t.locked
        && force < 2
    {
        let why = if reason.is_empty() {
            ";".to_string()
        } else {
            format!(", lock reason: {reason}")
        };
        bail!(
            "cannot {verb} a locked working tree{why}\nuse '{verb} -f -f' to override or unlock first"
        );
    }
    Ok(())
}

fn refuse_submodules(t: &Tree) -> Result<()> {
    if has_checked_out_submodule(&t.path) {
        bail!("working trees containing submodules cannot be moved or removed");
    }
    Ok(())
}

fn has_checked_out_submodule(root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(root.join(".gitmodules")) else {
        return false;
    };
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("path"))
        .filter_map(|l| l.trim_start().strip_prefix('='))
        .any(|p| root.join(p.trim()).join(".git").exists())
}

fn move_tree_to(git: &GitHome, cwd: &Path, args: MoveArgs) -> Result<()> {
    let all = trees(git, i64::MAX)?;
    let t = find(&all, cwd, &args.worktree)?;
    let Some(name) = &t.name else {
        bail!("'{}' is a main working tree", args.worktree);
    };
    let mut dest = absolute(cwd, &args.new_path);
    if dest.is_dir() {
        let base = t.path.file_name().unwrap_or_default();
        dest = dest.join(base);
    }
    if dest.exists() {
        bail!("'{}' already exists", dest.display());
    }
    refuse_locked(t, "move", args.force)?;
    if !t.path.join(".git").exists() {
        bail!(
            "validation failed, cannot move working tree: '{}' does not exist",
            t.path.join(".git").display()
        );
    }
    refuse_submodules(t)?;
    std::fs::rename(&t.path, &dest).with_context(|| {
        format!(
            "failed to move '{}' to '{}'",
            t.path.display(),
            dest.display()
        )
    })?;
    let admin = common_dir(git).join("worktrees").join(name);
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", dest.join(".git").display()),
    )?;
    Ok(())
}

fn prune(git: &GitHome, args: PruneArgs) -> Result<()> {
    let expire = match &args.expire {
        Some(t) => limit(t)?,
        None => i64::MAX,
    };
    for (name, dir) in admin_dirs(git) {
        let Some(reason) = stale(&dir, expire) else {
            continue;
        };
        if args.verbose || args.dry_run {
            eprintln!("Removing worktrees/{name}: {reason}");
        }
        if !args.dry_run {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    if !args.dry_run {
        let _ = std::fs::remove_dir(common_dir(git).join("worktrees"));
    }
    Ok(())
}

/// Drops the admin folders of work trees that have been gone since before `expire`, as `gc` does.
pub(super) fn prune_expired(git: &GitHome, expire: &str) -> Result<()> {
    prune(
        git,
        PruneArgs {
            dry_run: false,
            verbose: false,
            expire: Some(expire.to_string()),
        },
    )
}

fn remove(git: &GitHome, cwd: &Path, args: RemoveArgs) -> Result<()> {
    let all = trees(git, i64::MAX)?;
    let t = find(&all, cwd, &args.worktree)?;
    if t.name.is_none() {
        bail!("'{}' is a main working tree", args.worktree);
    }
    refuse_locked(t, "remove", args.force)?;
    let present = t.path.join(".git").exists();
    if present {
        if args.force == 0 {
            refuse_submodules(t)?;
            let there = GitHome::open_at(&t.path)?;
            if !there.repo.status()?.entries.is_empty() {
                bail!(
                    "'{}' contains modified or untracked files, use --force to delete it",
                    args.worktree
                );
            }
        }
        std::fs::remove_dir_all(&t.path)
            .with_context(|| format!("failed to delete '{}'", t.path.display()))?;
    } else if args.force == 0 {
        bail!(
            "validation failed, cannot remove working tree: '{}' does not exist",
            t.path.join(".git").display()
        );
    }
    std::fs::remove_dir_all(&t.git_dir)?;
    let _ = std::fs::remove_dir(common_dir(git).join("worktrees"));
    Ok(())
}

fn repair(git: &GitHome, cwd: &Path, args: RepairArgs) -> Result<()> {
    let admin_root = common_dir(git).join("worktrees");
    let fix = |admin: &Path, wt: &Path| -> Result<()> {
        let dot_git = wt.join(".git");
        let gitdir = admin.join("gitdir");
        if read_trimmed(&gitdir).as_deref() != Some(&dot_git.display().to_string()) {
            eprintln!("repair: gitdir incorrect: {}", gitdir.display());
            std::fs::write(&gitdir, format!("{}\n", dot_git.display()))?;
        }
        let want = format!("gitdir: {}", admin.display());
        if read_trimmed(&dot_git).as_deref() != Some(&want) {
            eprintln!("repair: .git file incorrect: {}", dot_git.display());
            std::fs::write(&dot_git, format!("{want}\n"))?;
        }
        Ok(())
    };
    let mut seen = Vec::new();
    for spec in &args.paths {
        let wt = absolute(cwd, spec);
        let dot_git = wt.join(".git");
        let Ok(text) = std::fs::read_to_string(&dot_git) else {
            eprintln!("error: not a valid path: {}", wt.display());
            continue;
        };
        let by_file = text
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("gitdir:"))
            .map(|p| PathBuf::from(p.trim()))
            .filter(|p| p.parent() == Some(admin_root.as_path()) && p.is_dir());
        let admin = by_file.or_else(|| {
            let base = wt.file_name()?;
            let dir = admin_root.join(base);
            dir.is_dir().then_some(dir)
        });
        match admin {
            Some(admin) => {
                fix(&admin, &wt)?;
                seen.push(admin);
            }
            None => eprintln!(
                "error: no worktree administrative folder for: {}",
                wt.display()
            ),
        }
    }
    for (_, dir) in admin_dirs(git) {
        if seen.contains(&dir) {
            continue;
        }
        let Some(target) = read_trimmed(&dir.join("gitdir")).filter(|t| !t.is_empty()) else {
            eprintln!("error: {}: gitdir file does not exist", dir.display());
            continue;
        };
        let dot_git = PathBuf::from(&target);
        if !dot_git.exists() {
            eprintln!("error: {}: .git file does not exist", dir.display());
            continue;
        }
        if let Some(wt) = dot_git.parent() {
            fix(&dir, wt)?;
        }
    }
    Ok(())
}

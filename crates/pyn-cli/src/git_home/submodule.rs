//! `pyn submodule` on a git repository: git's submodule lifecycle in git's layout (`.gitmodules`, `submodule.<name>.*`
//! in `.git/config`, git directories under `.git/modules/<name>` and gitdir files both ways).

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, FromArgMatches, Subcommand};
use pyn_git::IndexEntry;
use pyn_gitwork::{Pathspec, SubmoduleIgnore};
use pyn_object::ObjectId;

use super::GitHome;
use super::describe::submodule_name;
use super::fetch::{self, FetchArgs};
use super::stage::{GITLINK, entries_of, index_of, put_entry, stage_file, stat_fields};
use super::submodule_cfg::{self as cfg, Module};
use super::switch::{Via, checkout_revision};
use super::worktree::{Mode, move_tree};
use super::{Exits, clone};

/// Initialize, update and inspect nested repositories recorded as gitlinks.
#[derive(Debug, Args)]
pub struct SubmoduleArgs {
    /// Print nothing but errors.
    #[arg(short, long, global = true)]
    quiet: bool,
    /// With no command, show the recorded commits rather than the checked-out ones.
    #[arg(long)]
    cached: bool,
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Add a repository as a submodule at <path>.
    Add(AddArgs),
    /// Show the state of each submodule.
    Status(StatusArgs),
    /// Register submodules in the repository configuration.
    Init(PathsArgs),
    /// Unregister submodules and empty their folders.
    Deinit(DeinitArgs),
    /// Check out the commits the superproject records.
    Update(UpdateArgs),
    /// Set or clear the branch `update --remote` follows.
    SetBranch(SetBranchArgs),
    /// Change the url of a submodule in `.gitmodules` and synchronize it.
    SetUrl(SetUrlArgs),
    /// Show the commits between the recorded and the current commit of each submodule.
    Summary(super::submodule_summary::SummaryArgs),
    /// Run a command in each checked-out submodule.
    Foreach(ForeachArgs),
    /// Copy the urls in `.gitmodules` to the configuration and the submodules' remotes.
    Sync(SyncArgs),
    /// Move the git directories of submodules under the superproject's `.git/modules`.
    Absorbgitdirs(PathsArgs),
}

#[derive(Debug, Args)]
struct AddArgs {
    /// Follow this branch of the submodule's repository.
    #[arg(short, long)]
    branch: Option<String>,
    /// Add a path a `.gitignore` hides, and reuse a git directory kept from before.
    #[arg(short, long)]
    force: bool,
    /// Name the submodule this instead of its path.
    #[arg(long)]
    name: Option<String>,
    /// Make a clone with history truncated to this many commits.
    #[arg(long)]
    depth: Option<u32>,
    #[arg(long, hide = true, value_name = "REPO")]
    reference: Option<String>,
    #[arg(long, hide = true)]
    dissociate: bool,
    repository: String,
    path: Option<String>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Show the recorded commits rather than the checked-out ones.
    #[arg(long)]
    cached: bool,
    /// Also show the submodules of submodules.
    #[arg(long)]
    recursive: bool,
    paths: Vec<String>,
}

#[derive(Debug, Args)]
struct PathsArgs {
    paths: Vec<String>,
}

#[derive(Debug, Args)]
struct DeinitArgs {
    /// Discard local changes.
    #[arg(short, long)]
    force: bool,
    /// Deinitialize every submodule.
    #[arg(long)]
    all: bool,
    paths: Vec<String>,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    /// Register submodules that are not yet.
    #[arg(long)]
    init: bool,
    /// Take the commit from the submodule's remote branch instead of the recorded one.
    #[arg(long)]
    remote: bool,
    /// Update the submodules of submodules too.
    #[arg(long)]
    recursive: bool,
    /// Check out the commit, detaching HEAD (the default).
    #[arg(long, conflicts_with_all = ["merge", "rebase"])]
    checkout: bool,
    /// Merge the commit into the submodule's current branch.
    #[arg(long, conflicts_with = "rebase")]
    merge: bool,
    /// Rebase the submodule's current branch onto the commit.
    #[arg(long)]
    rebase: bool,
    /// Discard local changes when checking out.
    #[arg(short, long)]
    force: bool,
    /// Do not fetch new objects from the remote.
    #[arg(short = 'N', long)]
    no_fetch: bool,
    /// Make fresh clones with history truncated to this many commits.
    #[arg(long)]
    depth: Option<u32>,
    #[arg(short, long, hide = true, value_name = "N")]
    jobs: Option<usize>,
    #[arg(long, hide = true)]
    single_branch: bool,
    #[arg(long, hide = true)]
    no_single_branch: bool,
    #[arg(long, hide = true)]
    recommend_shallow: bool,
    #[arg(long, hide = true)]
    no_recommend_shallow: bool,
    paths: Vec<String>,
}

#[derive(Debug, Args)]
struct SetBranchArgs {
    /// The branch to follow.
    #[arg(
        short,
        long,
        conflicts_with = "default",
        required_unless_present = "default"
    )]
    branch: Option<String>,
    /// Follow the remote's `HEAD` again.
    #[arg(short, long)]
    default: bool,
    path: String,
}

#[derive(Debug, Args)]
struct SetUrlArgs {
    path: String,
    url: String,
}

#[derive(Debug, Args)]
struct ForeachArgs {
    /// Also visit the submodules of submodules.
    #[arg(long)]
    recursive: bool,
    /// The shell command to run.
    #[arg(required = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct SyncArgs {
    /// Also synchronize the submodules of submodules.
    #[arg(long)]
    recursive: bool,
    paths: Vec<String>,
}

/// Parses a fixed argument list into an options struct, for commands that are run on a submodule's behalf.
pub(super) fn parsed<T: Args + FromArgMatches>(argv: &[String]) -> T {
    let command = T::augment_args(clap::Command::new("pyn"));
    let matches = command
        .try_get_matches_from(argv)
        .expect("a fixed argument list parses");
    T::from_arg_matches(&matches).expect("a fixed argument list parses")
}

/// `to` written relative to the folder `from`; both are absolute and share their root.
pub(super) fn relative(from: &Path, to: &Path) -> PathBuf {
    let (from, to) = (lexical(from), lexical(to));
    let a: Vec<Component> = from.components().collect();
    let b: Vec<Component> = to.components().collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..a.len() {
        out.push("..");
    }
    for c in &b[common..] {
        out.push(c);
    }
    out
}

/// `p` with `.` and `..` folded away, without touching the file system.
fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// A gitlink in the index.
struct Link {
    path: String,
    id: ObjectId,
    conflicted: bool,
}

/// Where a run is: the repository, and how its submodules' paths are shown.
struct Scope<'a> {
    git: &'a GitHome,
    root: PathBuf,
    /// Shown before every path; the enclosing submodules' path when recursing.
    base: String,
    /// The working folder inside the repository, as a path prefix with its slash; top level only.
    cwd_prefix: String,
    quiet: bool,
}

impl<'a> Scope<'a> {
    fn top(git: &'a GitHome, cwd: &Path, quiet: bool) -> Result<Self> {
        let prefix = git.ws.repo_prefix(cwd, ".")?;
        Ok(Self {
            git,
            root: lexical(git.repo.root()?),
            base: String::new(),
            cwd_prefix: if prefix.is_empty() {
                prefix
            } else {
                format!("{prefix}/")
            },
            quiet,
        })
    }

    fn child<'b>(&self, git: &'b GitHome, path: &str) -> Scope<'b> {
        Scope {
            git,
            root: lexical(git.repo.root().unwrap_or(Path::new("/"))),
            base: format!("{}{}/", self.base, path),
            cwd_prefix: String::new(),
            quiet: self.quiet,
        }
    }

    fn shown(&self, path: &str) -> String {
        let local = if self.cwd_prefix.is_empty() {
            path.to_owned()
        } else {
            relative(Path::new(&self.cwd_prefix), Path::new(path))
                .display()
                .to_string()
        };
        format!("{}{}", self.base, local)
    }

    fn work(&self, path: &str) -> Result<PathBuf> {
        Ok(self.root.join(path))
    }

    fn open(&self, path: &str) -> Result<GitHome> {
        GitHome::open_at(&self.work(path)?)
    }

    fn populated(&self, path: &str) -> Result<bool> {
        Ok(self.work(path)?.join(".git").exists())
    }

    /// The gitlinks that `paths` (given in `cwd`) select, with an error for a path that selects none.
    fn links(&self, cwd: &Path, paths: &[String]) -> Result<Vec<Link>> {
        let index = self.git.open_index()?.index;
        let mut links: Vec<Link> = Vec::new();
        for e in &index.entries {
            if e.mode != GITLINK {
                continue;
            }
            let path = String::from_utf8_lossy(&e.path).into_owned();
            match links.last_mut() {
                Some(l) if l.path == path => l.conflicted |= e.stage > 0,
                _ => links.push(Link {
                    path,
                    id: e.id,
                    conflicted: e.stage > 0,
                }),
            }
        }
        if paths.is_empty() {
            return Ok(links);
        }
        let spec = self.git.pathspec(cwd, paths, false)?;
        if let Some(bad) = spec.first_unmatched(links.iter().map(|l| l.path.as_bytes())) {
            return Err(super::Refused(format!(
                "pathspec '{}' did not match any file(s) known to git",
                String::from_utf8_lossy(bad)
            ))
            .into());
        }
        links.retain(|l| spec.matches(l.path.as_bytes()) || spec.matches_dir(l.path.as_bytes()));
        Ok(links)
    }
}

pub(super) fn submodule(git: &GitHome, cwd: &Path, args: SubmoduleArgs) -> Result<()> {
    let scope = Scope::top(git, cwd, args.quiet)?;
    match args.action {
        None => status(
            &scope,
            cwd,
            &StatusArgs {
                cached: args.cached,
                recursive: false,
                paths: Vec::new(),
            },
        ),
        Some(Action::Add(a)) => add(&scope, cwd, a),
        Some(Action::Status(a)) => status(&scope, cwd, &a),
        Some(Action::Init(a)) => {
            for l in scope.links(cwd, &a.paths)? {
                init_one(&scope, &l, !a.paths.is_empty())?;
            }
            Ok(())
        }
        Some(Action::Deinit(a)) => deinit(&scope, cwd, &a),
        Some(Action::Update(a)) => {
            let opts = Update::of(&a);
            update(&scope, cwd, &opts, &a.paths)
        }
        Some(Action::SetBranch(a)) => set_branch(&scope, cwd, &a),
        Some(Action::SetUrl(a)) => set_url(&scope, cwd, &a),
        Some(Action::Foreach(a)) => foreach(&scope, &a, &a.command.join(" ")),
        Some(Action::Summary(a)) => super::submodule_summary::summary(git, cwd, &a),
        Some(Action::Sync(a)) => sync(&scope, cwd, &a),
        Some(Action::Absorbgitdirs(a)) => absorb(&scope, cwd, &a.paths),
    }
}

fn fatal(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn module_of<'m>(modules: &'m [Module], path: &str) -> Option<&'m Module> {
    modules.iter().find(|m| m.path == path)
}

fn unmapped(scope: &Scope, path: &str) -> anyhow::Error {
    fatal(format!(
        "no submodule mapping found in .gitmodules for path '{}'",
        scope.shown(path)
    ))
}

fn status(scope: &Scope, cwd: &Path, args: &StatusArgs) -> Result<()> {
    let modules = cfg::read(scope.git)?;
    let mut out = String::new();
    for l in scope.links(cwd, &args.paths)? {
        if module_of(&modules, &l.path).is_none() {
            return Err(unmapped(scope, &l.path));
        }
        let shown = scope.shown(&l.path);
        if l.conflicted {
            out.push_str(&format!("U{} {shown}\n", "0".repeat(l.id.to_hex().len())));
            continue;
        }
        if !scope.populated(&l.path)? {
            out.push_str(&format!("-{} {shown}\n", l.id.to_hex()));
            continue;
        }
        let child = scope.open(&l.path)?;
        let head = child.repo.head_commit()?;
        let moved = head.is_some_and(|h| h != l.id);
        let shown_id = match head {
            Some(h) if moved && !args.cached => h,
            _ => l.id,
        };
        let name = submodule_name(&child, &shown_id)?;
        let mark = if moved { '+' } else { ' ' };
        out.push_str(&format!("{mark}{} {shown} ({name})\n", shown_id.to_hex()));
        if args.recursive {
            super::commands::emit(&std::mem::take(&mut out));
            status(
                &scope.child(&child, &l.path),
                cwd,
                &StatusArgs {
                    cached: args.cached,
                    recursive: true,
                    paths: Vec::new(),
                },
            )?;
        }
    }
    super::commands::emit(&out);
    Ok(())
}

fn add(scope: &Scope, cwd: &Path, args: AddArgs) -> Result<()> {
    if args.reference.is_some() || args.dissociate {
        crate::usage_error("`pyn submodule add --reference`: alternates are not supported");
    }
    let git = scope.git;
    let repo_arg = args.repository.trim_end_matches('/').to_owned();
    let path_arg = match &args.path {
        Some(p) => p.trim_end_matches('/').to_owned(),
        None => super::clone::default_dir(&repo_arg, false)
            .context("No directory name could be guessed.")?,
    };
    let path = git.ws.repo_prefix(cwd, &path_arg)?;
    if path.is_empty() {
        bail!("'{path_arg}' is not a path inside the repository");
    }
    let opened = git.open_index()?;
    let bytes = path.as_bytes();
    if opened.index.entries.iter().any(|e| {
        e.path == bytes || (e.path.starts_with(bytes) && e.path.get(bytes.len()) == Some(&b'/'))
    }) {
        bail!("'{path}' already exists in the index");
    }
    let (recorded, clone_url) = if cfg::is_relative(&repo_arg) {
        (repo_arg.clone(), cfg::resolve_url(git, &repo_arg, false)?)
    } else if cwd.join(&repo_arg).is_dir() {
        let abs = cwd.join(&repo_arg).canonicalize()?.display().to_string();
        (abs.clone(), abs)
    } else {
        (repo_arg.clone(), repo_arg.clone())
    };
    let name = args.name.clone().unwrap_or_else(|| path.clone());
    let work = scope.work(&path)?;
    let gitdir = git.repo.git().git_dir().join("modules").join(&name);
    if work.join(".git").exists() {
        println!("Adding existing repo at '{path}' to the index");
    } else {
        if work.exists() && std::fs::read_dir(&work)?.next().is_some() {
            bail!("'{path}' already exists and is not a valid git repo");
        }
        if gitdir.exists() && !args.force {
            bail!(
                "A git directory for '{name}' is found locally with remote(s):\n  origin\t{clone_url}\nIf you want to reuse this local git directory instead of cloning again from\n  {clone_url}\nuse the '--force' option. If the local git directory is not the correct repo\nor you are unsure what this means choose another name with the '--name' option."
            );
        }
        if gitdir.exists() {
            println!("Reactivating local git directory for submodule '{name}'.");
            connect(&work, &gitdir)?;
            let child = GitHome::open_at(&work)?;
            let head = child
                .repo
                .head_commit()?
                .context("the git directory has no commit")?;
            populate(&child, &head)?;
        } else {
            if !scope.quiet {
                eprintln!("Cloning into '{}'...", work.display());
            }
            clone::clone_nested(&clone::Nested {
                url: &clone_url,
                work: &work,
                git_dir: &gitdir,
                branch: args.branch.as_deref(),
                depth: args.depth,
                quiet: scope.quiet,
            })
            .map_err(|e| {
                fatal(format!(
                    "{e:#}\nfatal: clone of '{clone_url}' into submodule path '{}' failed",
                    work.display()
                ))
            })?;
        }
    }
    let child = GitHome::open_at(&work)?;
    let id = child
        .repo
        .head_commit()?
        .context("submodule does not have a commit checked out")?;
    cfg::write_module(
        git,
        &Module {
            name: name.clone(),
            path: path.clone(),
            url: Some(recorded),
            branch: args.branch.clone(),
            ..Module::default()
        },
    )?;
    let mut local = cfg::local(git)?;
    local.set("submodule", Some(&name), "url", &clone_url)?;
    local.set("submodule", Some(&name), "active", "true")?;
    local.write(&cfg::local_path(git))?;
    let opened = git.open_index()?;
    let mut entries = entries_of(&opened.index);
    let meta = std::fs::symlink_metadata(&work)?;
    let mut link = IndexEntry::new(path.as_bytes(), GITLINK, id);
    stat_fields(&mut link, &meta);
    put_entry(&mut entries, link);
    let existing = entries.get(&(b".gitmodules".to_vec(), 0)).cloned();
    let modules = stage_file(git, b".gitmodules", existing.as_ref())?;
    put_entry(&mut entries, modules);
    git.save_index(&opened, &index_of(opened.index.version, entries))
}

/// Writes the gitdir file in `work` and `core.worktree` in `git_dir`, both relative.
fn connect(work: &Path, git_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(work)?;
    std::fs::write(
        work.join(".git"),
        format!("gitdir: {}\n", relative(work, git_dir).display()),
    )?;
    let path = git_dir.join("config");
    let mut file = cfg::read_file(&path)?;
    file.set(
        "core",
        None,
        "worktree",
        &relative(git_dir, work).display().to_string(),
    )?;
    Ok(file.write(&path)?)
}

/// Makes the work tree and index of a repository whose files are gone match `id`, detached.
fn populate(child: &GitHome, id: &ObjectId) -> Result<()> {
    let _ = std::fs::remove_file(child.repo.git().index_path());
    child.set_head(None, id, "checkout: moving to submodule commit")?;
    let tree = child.repo.commit(id)?.tree;
    let opened = child.open_index()?;
    if let Err(blocked) = move_tree(child, &opened, None, Some(&tree), Mode::Force)? {
        blocked.report("checkout", "submodule", 128);
    }
    Ok(())
}

fn init_one(scope: &Scope, l: &Link, named: bool) -> Result<()> {
    let git = scope.git;
    let modules = cfg::read(git)?;
    let Some(m) = module_of(&modules, &l.path) else {
        return Err(unmapped(scope, &l.path));
    };
    let local = cfg::local(git)?;
    if !named && !listed(git, &local, &l.path)? {
        return Ok(());
    }
    if !local.values("submodule", Some(&m.name), "url").is_empty() {
        return Ok(());
    }
    let url = m.url.as_deref().ok_or_else(|| {
        fatal(format!(
            "No url found for submodule path '{}' in .gitmodules",
            scope.shown(&l.path)
        ))
    })?;
    let resolved = cfg::resolve_url(git, url, true)?;
    let mut local = local;
    if local.values("submodule", None, "active").is_empty() {
        local.set("submodule", Some(&m.name), "active", "true")?;
    }
    local.set("submodule", Some(&m.name), "url", &resolved)?;
    if let Some(u) = m.update.as_deref().filter(|u| !u.starts_with('!')) {
        local.set("submodule", Some(&m.name), "update", u)?;
    }
    local.write(&cfg::local_path(git))?;
    if !scope.quiet {
        eprintln!(
            "Submodule '{}' ({resolved}) registered for path '{}'",
            m.name,
            scope.shown(&l.path)
        );
    }
    Ok(())
}

/// Whether `submodule.active` (when set) selects the path; with no such setting every path is selected.
fn listed(_git: &GitHome, local: &pyn_git::ConfigFile, path: &str) -> Result<bool> {
    let specs = local.values("submodule", None, "active");
    if specs.is_empty() {
        return Ok(true);
    }
    Ok(specs.iter().any(|s| {
        s == "."
            || s.is_empty()
            || Pathspec::new([s.as_bytes().to_vec()]).matches_dir(path.as_bytes())
    }))
}

/// Whether the submodule takes part in update, sync and foreach.
fn active(git: &GitHome, m: &Module) -> Result<bool> {
    let local = cfg::local(git)?;
    if let Some(v) = local.values("submodule", Some(&m.name), "active").last() {
        return Ok(matches!(v.as_str(), "true" | "yes" | "on" | "1" | ""));
    }
    if !local.values("submodule", None, "active").is_empty() {
        return listed(git, &local, &m.path);
    }
    Ok(!local.values("submodule", Some(&m.name), "url").is_empty())
}

struct Update {
    init: bool,
    remote: bool,
    recursive: bool,
    mode: Option<&'static str>,
    force: bool,
    no_fetch: bool,
    depth: Option<u32>,
    /// Leave submodules that are not checked out alone instead of cloning them.
    populated_only: bool,
    /// Report each step.
    say: bool,
}

impl Update {
    fn of(a: &UpdateArgs) -> Self {
        let mode = [
            (a.checkout, "checkout"),
            (a.merge, "merge"),
            (a.rebase, "rebase"),
        ]
        .into_iter()
        .find_map(|(on, m)| on.then_some(m));
        Self {
            init: a.init,
            remote: a.remote,
            recursive: a.recursive,
            mode,
            force: a.force,
            no_fetch: a.no_fetch,
            depth: a.depth,
            populated_only: false,
            say: true,
        }
    }
}

fn update(scope: &Scope, cwd: &Path, opts: &Update, paths: &[String]) -> Result<()> {
    let git = scope.git;
    let modules = cfg::read(git)?;
    for l in scope.links(cwd, paths)? {
        let Some(m) = module_of(&modules, &l.path) else {
            return Err(unmapped(scope, &l.path));
        };
        if opts.init {
            init_one(scope, &l, !paths.is_empty())?;
        }
        if l.conflicted {
            eprintln!("Skipping unmerged submodule {}", scope.shown(&l.path));
            continue;
        }
        if !active(git, m)? || (opts.populated_only && !scope.populated(&l.path)?) {
            continue;
        }
        update_one(scope, m, &l, opts)?;
        if opts.recursive && scope.populated(&l.path)? {
            let child = scope.open(&l.path)?;
            update(&scope.child(&child, &l.path), cwd, opts, &[])?;
        }
    }
    Ok(())
}

fn update_one(scope: &Scope, m: &Module, l: &Link, opts: &Update) -> Result<()> {
    let git = scope.git;
    let shown = scope.shown(&l.path);
    let local = cfg::local(git)?;
    let configured = local
        .values("submodule", Some(&m.name), "update")
        .into_iter()
        .next_back();
    let mode = opts
        .mode
        .map(str::to_owned)
        .or(configured)
        .unwrap_or_else(|| "checkout".into());
    if mode == "none" {
        println!("Skipping submodule '{shown}'");
        return Ok(());
    }
    let url = cfg::local_value(git, &m.name, "url")?.ok_or_else(|| {
        fatal(format!(
            "No url found for submodule path '{shown}' in .gitmodules"
        ))
    })?;
    let work = scope.work(&l.path)?;
    let gitdir = git.repo.git().git_dir().join("modules").join(&m.name);
    let depth = opts
        .depth
        .or_else(|| m.shallow.unwrap_or(false).then_some(1));
    let fresh = !work.join(".git").exists();
    if fresh {
        if gitdir.join("HEAD").exists() {
            connect(&work, &gitdir)?;
        } else {
            if !scope.quiet {
                eprintln!("Cloning into '{}'...", work.display());
            }
            clone::clone_nested(&clone::Nested {
                url: &url,
                work: &work,
                git_dir: &gitdir,
                branch: None,
                depth,
                quiet: scope.quiet,
            })
            .map_err(|e| {
                fatal(format!(
                    "{e:#}\nfatal: clone of '{url}' into submodule path '{}' failed",
                    work.display()
                ))
            })?;
        }
    }
    let child = GitHome::open_at(&work)?;
    let mut target = l.id;
    if opts.remote {
        if !opts.no_fetch {
            fetch_quietly(&child, &[])?;
        }
        target = remote_tip(scope, &child, m)?;
    }
    if !has(&child, &target)? {
        if !opts.no_fetch || fresh {
            fetch_quietly(&child, &[])?;
        }
        if !has(&child, &target)? && !opts.no_fetch {
            let _ = fetch_quietly(&child, &["origin".to_owned(), target.to_hex()]);
        }
        if !has(&child, &target)? {
            return Err(Exits(
                128,
                format!(
                    "Fetched in submodule path '{shown}', but it did not contain {}. Direct fetching of that commit failed.",
                    target.to_hex()
                ),
            )
            .into());
        }
    }
    let hex = target.to_hex();
    if fresh {
        populate(&child, &target)?;
        if opts.say {
            println!("Submodule path '{shown}': checked out '{hex}'");
        }
        return Ok(());
    }
    if child.repo.head_commit()? == Some(target) {
        return Ok(());
    }
    if let Some(command) = mode.strip_prefix('!') {
        run_command(&format!("{command} {hex}"), &work)?;
        println!("Submodule path '{shown}': '{command} {hex}'");
        return Ok(());
    }
    match mode.as_str() {
        "checkout" => {
            let via = Via {
                quiet: true,
                force: opts.force,
                ..Via::default()
            };
            inside(&child, || checkout_revision(&child, &hex, via)).map_err(|e| {
                fatal(format!(
                    "{e:#}\nfatal: Unable to checkout '{hex}' in submodule path '{shown}'"
                ))
            })?;
            if opts.say {
                println!("Submodule path '{shown}': checked out '{hex}'");
            }
        }
        "merge" => {
            let args = parsed(&["merge".into(), hex.clone()]);
            inside(&child, || super::merge::merge(&child, args)).map_err(|e| {
                fatal(format!(
                    "{e:#}\nfatal: Unable to merge '{hex}' in submodule path '{shown}'"
                ))
            })?;
            if opts.say {
                println!("Submodule path '{shown}': merged in '{hex}'");
            }
        }
        "rebase" => {
            let args = parsed(&["rebase".into(), hex.clone()]);
            inside(&child, || super::rebase::rebase(&child, &args)).map_err(|e| {
                fatal(format!(
                    "{e:#}\nfatal: Unable to rebase '{hex}' in submodule path '{shown}'"
                ))
            })?;
            if opts.say {
                println!("Submodule path '{shown}': rebased into '{hex}'");
            }
        }
        other => bail!("Invalid update mode '{other}' for submodule path '{shown}'"),
    }
    Ok(())
}

/// Runs `f` with the process in the submodule's work tree, as git runs a command it starts there.
fn inside<T>(child: &GitHome, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let here = std::env::current_dir()?;
    if let Ok(root) = child.repo.root() {
        std::env::set_current_dir(root)?;
    }
    let result = f();
    std::env::set_current_dir(here)?;
    result
}

fn has(git: &GitHome, id: &ObjectId) -> Result<bool> {
    Ok(git.repo.objects().read_opt(id)?.is_some())
}

fn fetch_quietly(child: &GitHome, extra: &[String]) -> Result<()> {
    let mut argv = vec!["fetch".to_owned(), "--quiet".to_owned()];
    argv.extend_from_slice(extra);
    inside(child, || fetch::fetch(child, parsed::<FetchArgs>(&argv)))
}

/// The tip of the branch `update --remote` follows on the submodule's `origin`.
fn remote_tip(scope: &Scope, child: &GitHome, m: &Module) -> Result<ObjectId> {
    let branch = match m.branch.as_deref() {
        Some(".") => match scope.git.repo.head()? {
            pyn_gitwork::Head::Branch { name, .. } => name,
            pyn_gitwork::Head::Detached(_) => bail!(
                "Unable to find current revision in submodule path '{}'",
                scope.shown(&m.path)
            ),
        },
        Some(b) => b.to_owned(),
        None => "HEAD".into(),
    };
    let name = format!("refs/remotes/origin/{branch}");
    child.repo.refs().resolve(&name)?.ok_or_else(|| {
        fatal(format!(
            "Unable to find current origin/{branch} revision in submodule path '{}'",
            scope.shown(&m.path)
        ))
    })
}

fn run_command(command: &str, dir: &Path) -> Result<()> {
    let status = std::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(dir)
        .status()?;
    if !status.success() {
        bail!("run_command returned non-zero status");
    }
    Ok(())
}

fn foreach(scope: &Scope, args: &ForeachArgs, command: &str) -> Result<()> {
    let modules = cfg::read(scope.git)?;
    let root = scope.root.clone();
    for l in scope.links(&root, &[])? {
        if !scope.populated(&l.path)? {
            continue;
        }
        let Some(m) = module_of(&modules, &l.path) else {
            continue;
        };
        let shown = scope.shown(&l.path);
        let dir = scope.work(&l.path)?;
        if !scope.quiet {
            println!("Entering '{shown}'");
        }
        std::io::stdout().flush()?;
        let status = std::process::Command::new("sh")
            .args(["-c", command])
            .current_dir(&dir)
            .env("name", &m.name)
            .env("sm_path", &l.path)
            .env("path", &l.path)
            .env("displaypath", &shown)
            .env("sha1", l.id.to_hex())
            .env("toplevel", &root)
            .status()?;
        if !status.success() {
            bail!("run_command returned non-zero status for {shown}\n.");
        }
        if args.recursive {
            let child = scope.open(&l.path)?;
            foreach(&scope.child(&child, &l.path), args, command)?;
        }
    }
    Ok(())
}

fn sync(scope: &Scope, cwd: &Path, args: &SyncArgs) -> Result<()> {
    let git = scope.git;
    let modules = cfg::read(git)?;
    for l in scope.links(cwd, &args.paths)? {
        let Some(m) = module_of(&modules, &l.path) else {
            continue;
        };
        if !active(git, m)? {
            continue;
        }
        let Some(url) = &m.url else {
            return Err(fatal(format!(
                "No url found for submodule path '{}' in .gitmodules",
                scope.shown(&l.path)
            )));
        };
        let resolved = cfg::resolve_url(git, url, false)?;
        if !scope.quiet {
            println!("Synchronizing submodule url for '{}'", scope.shown(&l.path));
        }
        let mut local = cfg::local(git)?;
        local.set("submodule", Some(&m.name), "url", &resolved)?;
        local.write(&cfg::local_path(git))?;
        if scope.populated(&l.path)? {
            let child = scope.open(&l.path)?;
            let remote = default_remote(&child);
            let path = child.repo.git().common_dir().join("config");
            let mut file = cfg::read_file(&path)?;
            file.set("remote", Some(&remote), "url", &resolved)?;
            file.write(&path)?;
            if args.recursive {
                let child = scope.open(&l.path)?;
                sync(&scope.child(&child, &l.path), cwd, args)?;
            }
        }
    }
    Ok(())
}

fn default_remote(child: &GitHome) -> String {
    match child.repo.head() {
        Ok(pyn_gitwork::Head::Branch { name, .. }) => {
            pyn_git::tracking(child.config(), &name).remote
        }
        _ => None,
    }
    .unwrap_or_else(|| "origin".into())
}

fn deinit(scope: &Scope, cwd: &Path, args: &DeinitArgs) -> Result<()> {
    if args.paths.is_empty() && !args.all {
        bail!("Use '--all' if you really want to deinitialize all submodules");
    }
    let git = scope.git;
    let modules = cfg::read(git)?;
    for l in scope.links(cwd, &args.paths)? {
        let Some(m) = module_of(&modules, &l.path) else {
            continue;
        };
        let shown = scope.shown(&l.path);
        let work = scope.work(&l.path)?;
        let dot_git = work.join(".git");
        if dot_git.is_dir() {
            bail!(
                "Submodule work tree '{shown}' contains a .git directory\n(use 'rm -rf' if you really want to remove it including all of its history)"
            );
        }
        if dot_git.exists() && !args.force {
            let s = git
                .repo
                .submodules()
                .status(l.path.as_bytes(), l.id, SubmoduleIgnore::None)?;
            if s.is_some_and(|s| s.modified || s.untracked) {
                bail!(
                    "Submodule work tree '{shown}' contains local modifications; use '-f' to discard them"
                );
            }
        }
        if work.is_dir() {
            for e in std::fs::read_dir(&work)? {
                let p = e?.path();
                if p.is_dir() && !p.is_symlink() {
                    std::fs::remove_dir_all(&p)?;
                } else {
                    std::fs::remove_file(&p)?;
                }
            }
            if !scope.quiet {
                println!("Cleared directory '{shown}'");
            }
        }
        let path = cfg::local_path(git);
        let mut local = cfg::local(git)?;
        if local.remove_section("submodule", Some(&m.name)) {
            local.write(&path)?;
            if !scope.quiet {
                let url = m.url.clone().unwrap_or_default();
                println!(
                    "Submodule '{}' ({url}) unregistered for path '{shown}'",
                    m.name
                );
            }
        }
    }
    Ok(())
}

fn set_branch(scope: &Scope, cwd: &Path, args: &SetBranchArgs) -> Result<()> {
    let m = named(scope, cwd, &args.path)?;
    cfg::set_key(scope.git, &m.name, "branch", args.branch.as_deref())
}

fn set_url(scope: &Scope, cwd: &Path, args: &SetUrlArgs) -> Result<()> {
    let m = named(scope, cwd, &args.path)?;
    cfg::set_key(scope.git, &m.name, "url", Some(&args.url))?;
    sync(
        scope,
        cwd,
        &SyncArgs {
            recursive: false,
            paths: vec![args.path.clone()],
        },
    )
}

fn named(scope: &Scope, cwd: &Path, path: &str) -> Result<Module> {
    let rel = scope.git.ws.repo_prefix(cwd, path)?;
    cfg::read(scope.git)?
        .into_iter()
        .find(|m| m.path == rel)
        .ok_or_else(|| {
            fatal(format!(
                "no submodule mapping found in .gitmodules for path '{path}'"
            ))
        })
}

fn absorb(scope: &Scope, cwd: &Path, paths: &[String]) -> Result<()> {
    let git = scope.git;
    let modules = cfg::read(git)?;
    for l in scope.links(cwd, paths)? {
        let Some(m) = module_of(&modules, &l.path) else {
            continue;
        };
        if !scope.populated(&l.path)? {
            continue;
        }
        let work = scope.work(&l.path)?;
        let dot_git = work.join(".git");
        if dot_git.is_dir() {
            let dest = git.repo.git().git_dir().join("modules").join(&m.name);
            if dest.exists() {
                bail!(
                    "could not rename '{}' to '{}': destination exists",
                    dot_git.display(),
                    dest.display()
                );
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            println!(
                "Migrating git directory of '{}' from\n'{}' to\n'{}'",
                scope.shown(&l.path),
                dot_git.display(),
                dest.display()
            );
            std::fs::rename(&dot_git, &dest)?;
            connect(&work, &dest)?;
        }
        let child = scope.open(&l.path)?;
        absorb(&scope.child(&child, &l.path), &work, &[])?;
    }
    Ok(())
}

/// After a clone: registers and checks out the submodules `submodule.active` selects, and theirs.
pub(super) fn after_clone(work: &Path, shallow: bool, quiet: bool) -> Result<()> {
    let git = GitHome::open_at(work)?;
    let scope = Scope::top(&git, work, quiet)?;
    let opts = Update {
        init: true,
        remote: false,
        recursive: true,
        mode: None,
        force: false,
        no_fetch: false,
        depth: shallow.then_some(1),
        populated_only: false,
        say: true,
    };
    update(&scope, work, &opts, &[])
}

/// Whether `--recurse-submodules` was given, or `submodule.recurse` asks for it and `--no-recurse-submodules` did not.
pub(super) fn wants_recursion(git: &GitHome, (on, off): (bool, bool)) -> bool {
    match (on, off) {
        (true, _) => true,
        (_, true) => false,
        _ => git
            .config()
            .get_bool("submodule.recurse")
            .ok()
            .flatten()
            .unwrap_or(false),
    }
}

/// Brings the checked-out submodules to the commits the index now records, as `checkout --recurse-submodules` does.
pub(super) fn follow_head(git: &GitHome, force: bool) -> Result<()> {
    let root = lexical(git.repo.root()?);
    let scope = Scope::top(git, &root, true)?;
    let opts = Update {
        init: false,
        remote: false,
        recursive: true,
        mode: Some("checkout"),
        force,
        no_fetch: true,
        depth: None,
        populated_only: true,
        say: false,
    };
    update(&scope, &root, &opts, &[])
}

/// How a fetch treats submodules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Fetching {
    No,
    OnDemand,
    Yes,
}

impl Fetching {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "on-demand" => Some(Self::OnDemand),
            "yes" | "true" | "on" | "1" => Some(Self::Yes),
            "no" | "false" | "off" | "0" => Some(Self::No),
            _ => None,
        }
    }

    /// The command line (`Some(None)` is a bare flag), then `fetch.recurseSubmodules`, then `submodule.recurse`,
    /// then fetching on demand.
    pub(super) fn resolve(git: &GitHome, flag: Option<Option<&str>>, off: bool) -> Result<Self> {
        if off {
            return Ok(Self::No);
        }
        match flag {
            Some(None) => return Ok(Self::Yes),
            Some(Some(v)) => {
                return Self::parse(v)
                    .ok_or_else(|| fatal(format!("bad recurse-submodules argument: {v}")));
            }
            None => {}
        }
        if let Some(v) = git.config().get("fetch.recursesubmodules") {
            return Self::parse(v)
                .ok_or_else(|| fatal(format!("bad fetch.recurseSubmodules argument: {v}")));
        }
        Ok(
            match git.config().get_bool("submodule.recurse").ok().flatten() {
                Some(true) => Self::Yes,
                _ => Self::OnDemand,
            },
        )
    }
}

/// Fetches in the checked-out submodules: all of them, or those the repository's refs record commits they lack.
pub(super) fn fetch_submodules(git: &GitHome, mode: Fetching, quiet: bool) -> Result<()> {
    if mode == Fetching::No || git.is_bare() {
        return Ok(());
    }
    let root = lexical(git.repo.root()?);
    let scope = Scope::top(git, &root, quiet)?;
    fetch_in(&scope, mode)
}

fn fetch_in(scope: &Scope, mode: Fetching) -> Result<()> {
    let git = scope.git;
    let modules = cfg::read(git)?;
    let local = cfg::local(git)?;
    for l in scope.links(&scope.root, &[])? {
        let Some(m) = module_of(&modules, &l.path) else {
            continue;
        };
        if !scope.populated(&l.path)? || !active(git, m)? {
            continue;
        }
        let per = local
            .values("submodule", Some(&m.name), "fetchrecursesubmodules")
            .pop()
            .or_else(|| m.fetch_recurse.clone())
            .and_then(|v| Fetching::parse(&v));
        let mode = per.unwrap_or(mode);
        let child = scope.open(&l.path)?;
        let wanted = match mode {
            Fetching::No => false,
            Fetching::Yes => true,
            Fetching::OnDemand => lacks_recorded(git, &l, &child)?,
        };
        if !wanted {
            continue;
        }
        if !scope.quiet {
            eprintln!("Fetching submodule {}", scope.shown(&l.path));
        }
        let argv: Vec<String> = if scope.quiet {
            vec!["fetch".into(), "--quiet".into()]
        } else {
            vec!["fetch".into()]
        };
        inside(&child, || fetch::fetch(&child, parsed::<FetchArgs>(&argv)))?;
        fetch_in(&scope.child(&child, &l.path), mode)?;
    }
    Ok(())
}

/// Whether the index or some ref of the superproject records a commit for the submodule that it does not have.
fn lacks_recorded(git: &GitHome, link: &Link, child: &GitHome) -> Result<bool> {
    if !has(child, &link.id)? {
        return Ok(true);
    }
    for (_, id) in git.repo.refs().list("refs/")? {
        let Ok(tip) = git.repo.commit(&id) else {
            continue;
        };
        if let Some(e) = git.repo.entry_at(&tip.tree, link.path.as_bytes())?
            && e.mode == pyn_object::FileMode::Gitlink
            && !has(child, &e.id)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

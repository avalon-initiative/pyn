//! `pyn clone` on a git remote: git's clone over the transports, without git.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{ArgAction, Args};
use pyn_git::{ConfigFile, Expect, ObjectFilter, is_valid_remote_name, set_tracking};
use pyn_gittransport::{Env, redact_text, user_config};
use pyn_gitwire::{Deepen, RemoteRef};
use pyn_object::{Algorithm, ObjectId};

use super::GitHome;
use super::convert;
use super::fetch::{Options, Plan, TagMode, fetch_core};
use super::partial::{FilterFlags, register_in};
use super::shallow::ShallowFlags;
use super::switch::detach_advice;
use super::worktree::{Mode, move_tree};

/// Options of `pyn clone <git url>`; those git has and pyn does not build are refused as usage errors.
#[derive(Debug, Args, Default, PartialEq, Eq)]
pub struct CloneOptions {
    /// Check out this branch (or detach at this tag) instead of the remote's `HEAD`.
    #[arg(short, long, value_name = "NAME")]
    branch: Option<String>,
    /// Name the remote this instead of `origin`.
    #[arg(short, long, value_name = "NAME")]
    origin: Option<String>,
    /// Leave the work tree and index empty.
    #[arg(short = 'n', long)]
    no_checkout: bool,
    /// Make a bare repository: no work tree, branches copied to `refs/heads`.
    #[arg(long)]
    bare: bool,
    /// Print nothing but errors.
    #[arg(short, long)]
    quiet: bool,
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
    /// Show the server's progress even when standard error is not a terminal.
    #[arg(long, overrides_with = "no_progress")]
    progress: bool,
    #[arg(long, overrides_with = "progress")]
    no_progress: bool,
    /// Fetch only the branch being cloned.
    #[arg(long, overrides_with = "no_single_branch")]
    single_branch: bool,
    #[arg(long, overrides_with = "single_branch")]
    no_single_branch: bool,
    /// Fetch no tags, and keep fetching none (`remote.<name>.tagOpt`).
    #[arg(long, overrides_with = "tags")]
    no_tags: bool,
    #[arg(long, overrides_with = "no_tags")]
    tags: bool,
    /// Set a configuration variable in the new repository before the fetch.
    #[arg(short = 'c', long = "config", value_name = "KEY=VALUE")]
    config: Vec<String>,
    /// Accepted; submodules are cloned one after the other.
    #[arg(short, long, value_name = "N")]
    jobs: Option<usize>,
    /// Make a bare repository that mirrors every ref, fetched with `+refs/*:refs/*`.
    #[arg(long)]
    mirror: bool,
    #[command(flatten)]
    shallow: ShallowFlags,
    /// Keep the git directory here and leave a `.git` file pointing at it in the work tree.
    #[arg(long, value_name = "DIR")]
    separate_git_dir: Option<PathBuf>,
    #[command(flatten)]
    filter: FilterFlags,
    #[arg(long, hide = true)]
    also_filter_submodules: bool,
    /// Clone the submodules too, or those matching the pathspec.
    #[arg(long, num_args = 0..=1, value_name = "PATHSPEC", alias = "recursive", default_missing_value = ".", require_equals = true)]
    recurse_submodules: Vec<String>,
    /// Clone submodules with a history truncated to depth 1.
    #[arg(long, overrides_with = "no_shallow_submodules")]
    shallow_submodules: bool,
    #[arg(long, overrides_with = "shallow_submodules")]
    no_shallow_submodules: bool,
    /// Check out only the files in the root, as `sparse-checkout init --cone` does.
    #[arg(long)]
    sparse: bool,
    #[arg(long, hide = true, value_name = "REPO")]
    reference: Vec<String>,
    #[arg(long, hide = true, value_name = "REPO")]
    reference_if_able: Vec<String>,
    #[arg(long, hide = true)]
    dissociate: bool,
    #[arg(long, hide = true, value_name = "DIR")]
    template: Option<String>,
    #[arg(short, long, hide = true)]
    local: bool,
    #[arg(long, hide = true)]
    no_local: bool,
    #[arg(long, hide = true)]
    no_hardlinks: bool,
    #[arg(short, long, hide = true)]
    shared: bool,
    #[arg(short = 'u', long, hide = true, value_name = "PATH")]
    upload_pack: Option<String>,
    #[arg(long, hide = true, value_name = "OPTION")]
    server_option: Vec<String>,
    #[arg(long, hide = true, value_name = "URI")]
    bundle_uri: Option<String>,
    #[arg(long, hide = true, value_name = "FORMAT")]
    ref_format: Option<String>,
}

impl CloneOptions {
    /// Whether only one branch is fetched: asked for, or implied by a shallow flag.
    fn single(&self) -> bool {
        self.single_branch || (!self.no_single_branch && self.shallow.given())
    }

    fn is_bare(&self) -> bool {
        self.bare || self.mirror
    }

    fn refuse_unsupported(&self) {
        let partial = "submodules are not cloned partially yet";
        let local =
            "a local clone goes through the transfer protocol; hardlinks and sharing are not built";
        let given = [
            (
                "--also-filter-submodules",
                self.also_filter_submodules,
                partial,
            ),
            (
                "--reference",
                !self.reference.is_empty() || !self.reference_if_able.is_empty(),
                "alternates are not supported",
            ),
            (
                "--dissociate",
                self.dissociate,
                "alternates are not supported",
            ),
            (
                "--template",
                self.template.is_some(),
                "repositories are created without templates",
            ),
            ("--local", self.local, local),
            ("--no-local", self.no_local, local),
            ("--no-hardlinks", self.no_hardlinks, local),
            ("--shared", self.shared, local),
            (
                "--upload-pack",
                self.upload_pack.is_some(),
                "the remote command is built in",
            ),
            (
                "--server-option",
                !self.server_option.is_empty(),
                "protocol options are not supported",
            ),
            (
                "--bundle-uri",
                self.bundle_uri.is_some(),
                "bundles are not supported",
            ),
            (
                "--ref-format",
                self.ref_format.is_some(),
                "only the files ref format is supported",
            ),
        ];
        if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
            crate::usage_error(&format!("`pyn clone {flag}`: {why}"));
        }
    }

    /// The git-only flag that was given, for a refusal when the source is a pyn server.
    pub fn first_given(&self) -> Option<&'static str> {
        let flags = [
            ("--branch", self.branch.is_some()),
            ("--origin", self.origin.is_some()),
            ("--no-checkout", self.no_checkout),
            ("--bare", self.bare),
            ("--mirror", self.mirror),
            ("--separate-git-dir", self.separate_git_dir.is_some()),
            ("--quiet", self.quiet),
            ("--verbose", self.verbose > 0),
            ("--progress", self.progress || self.no_progress),
            (
                "--single-branch",
                self.single_branch || self.no_single_branch,
            ),
            ("--tags", self.no_tags || self.tags),
            ("--config", !self.config.is_empty()),
            ("--jobs", self.jobs.is_some()),
            ("--filter", self.filter.given()),
            ("--recurse-submodules", !self.recurse_submodules.is_empty()),
            ("--shallow-submodules", self.shallow_submodules),
            ("--no-shallow-submodules", self.no_shallow_submodules),
        ];
        flags.into_iter().find(|(_, on)| *on).map(|(f, _)| f)
    }
}

/// Where the remote's `HEAD` leads.
#[derive(Debug, PartialEq, Eq)]
enum RemoteHead {
    /// A branch that exists (short name).
    Branch(String),
    /// No branch is at the commit `HEAD` names.
    Detached(ObjectId),
    /// `HEAD` names a branch the remote does not have yet (short name).
    Unborn(String),
    /// The remote says nothing about `HEAD`.
    Unknown,
}

fn head_of(refs: &[RemoteRef], prefer: &str) -> RemoteHead {
    let branches = || refs.iter().filter(|r| r.name.starts_with("refs/heads/"));
    let Some(head) = refs.iter().find(|r| r.name == "HEAD") else {
        return RemoteHead::Unknown;
    };
    let short = |name: &str| name.strip_prefix("refs/heads/").map(str::to_owned);
    match (head.id, head.symref_target.as_deref().and_then(short)) {
        (Some(_), Some(b)) if branches().any(|r| r.id.is_some() && r.name[11..] == b) => {
            RemoteHead::Branch(b)
        }
        (None, Some(b)) => RemoteHead::Unborn(b),
        (Some(id), _) => {
            let same = || branches().filter(|r| r.id == Some(id));
            let wanted = format!("refs/heads/{prefer}");
            match same()
                .find(|r| r.name == wanted)
                .or_else(|| same().min_by(|a, b| a.name.cmp(&b.name)))
            {
                Some(r) => RemoteHead::Branch(r.name[11..].to_owned()),
                None => RemoteHead::Detached(id),
            }
        }
        (None, None) => RemoteHead::Unknown,
    }
}

/// The directory git would name for `source`: the last path part without `.git`, and `.git` added for a bare clone.
pub fn default_dir(source: &str, bare: bool) -> Option<String> {
    let mut s = source.trim_end_matches('/');
    s = s.strip_suffix("/.git").unwrap_or(s).trim_end_matches('/');
    let (scheme_less, had_scheme) = match s.split_once("://") {
        Some((_, rest)) => (rest, true),
        None => (s, false),
    };
    let name = if had_scheme && !scheme_less.contains('/') {
        let host = scheme_less.rsplit('@').next().unwrap_or(scheme_less);
        host.split(':').next().unwrap_or(host)
    } else {
        scheme_less.rsplit(['/', ':']).next().unwrap_or(scheme_less)
    };
    let name = name
        .strip_suffix(".git")
        .or_else(|| name.strip_suffix(".bundle"))
        .unwrap_or(name);
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(if bare {
        format!("{name}.git")
    } else {
        name.to_owned()
    })
}

/// Splits `section.sub.key` as git does: the section is up to the first dot, the key after the last.
fn split_key(name: &str) -> Result<(String, Option<String>, String)> {
    let (Some(first), Some(last)) = (name.find('.'), name.rfind('.')) else {
        bail!("key does not contain a section: {name}");
    };
    let key = &name[last + 1..];
    if first == 0 || key.is_empty() {
        bail!("invalid key: {name}");
    }
    let sub = (first != last).then(|| name[first + 1..last].to_owned());
    Ok((name[..first].to_ascii_lowercase(), sub, key.to_owned()))
}

fn apply_settings(file: &mut ConfigFile, settings: &[String]) -> Result<()> {
    for setting in settings {
        let (name, value) = setting.split_once('=').unwrap_or((setting, "true"));
        let (section, sub, key) = split_key(name)?;
        file.add(&section, sub.as_deref(), &key, value)?;
    }
    Ok(())
}

fn default_branch(user: &pyn_git::Config) -> String {
    user.get("init.defaultbranch")
        .filter(|b| !b.is_empty())
        .unwrap_or("master")
        .to_owned()
}

/// The refspecs a clone fetches, once the remote's refs and its `HEAD` are known.
fn choose_specs(
    refs: &[RemoteRef],
    opts: &CloneOptions,
    origin: &str,
    prefer: &str,
) -> Result<Vec<String>> {
    let id_of = |name: String| refs.iter().find(|r| r.name == name && r.id.is_some());
    let single = opts.single();
    let dest = if opts.is_bare() {
        "refs/heads/".to_owned()
    } else {
        format!("refs/remotes/{origin}/")
    };
    let all = if opts.mirror {
        vec!["+refs/*:refs/*".to_owned()]
    } else {
        vec![format!("+refs/heads/*:{dest}*")]
    };
    let branch_spec = |b: &str| vec![format!("+refs/heads/{b}:{dest}{b}")];
    if let Some(b) = &opts.branch {
        let is_branch = id_of(format!("refs/heads/{b}")).is_some();
        if !is_branch && id_of(format!("refs/tags/{b}")).is_none() {
            bail!("Remote branch {b} not found in upstream {origin}");
        }
        return Ok(match (single, is_branch) {
            (false, _) => all,
            (true, true) => branch_spec(b),
            (true, false) => vec![format!("+refs/tags/{b}:refs/tags/{b}")],
        });
    }
    Ok(match (single, head_of(refs, prefer)) {
        (true, RemoteHead::Branch(b)) => branch_spec(&b),
        _ => all,
    })
}

fn remove_contents(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let _ = match e.file_type() {
                Ok(t) if t.is_dir() => std::fs::remove_dir_all(e.path()),
                _ => std::fs::remove_file(e.path()),
            };
        }
    }
}

/// Where a failed clone's leftovers are: the directory it made, or what it put in an existing empty one.
fn clean_up(target: &Path, existed: bool, git_dir: &Path, bare: bool) {
    match (existed, bare) {
        (false, _) => {
            let _ = std::fs::remove_dir_all(target);
        }
        (true, true) => remove_contents(target),
        (true, false) => {
            let dot_git = target.join(".git");
            let _ = std::fs::remove_dir_all(&dot_git);
            let _ = std::fs::remove_file(&dot_git);
        }
    }
    if git_dir != target && git_dir != target.join(".git") {
        let _ = std::fs::remove_dir_all(git_dir);
    }
}

/// Everything up to and including the fetch: the repository, its config and its objects and remote refs.
struct Downloaded {
    remote_refs: Vec<RemoteRef>,
    specs: Vec<String>,
}

/// What the server is asked to leave out of the clone.
struct Limits<'a> {
    deepen: Option<&'a Deepen>,
    filter: Option<&'a ObjectFilter>,
}

fn download(
    opts: &CloneOptions,
    limits: &Limits<'_>,
    origin: &str,
    url: &str,
    prefer: &str,
    git_dir: &Path,
    root: &Path,
) -> Result<Downloaded> {
    pyn_git::init(git_dir, opts.is_bare(), Algorithm::Sha1, prefer)?;
    let config_path = git_dir.join("config");
    let mut file = ConfigFile::read(&config_path)?;
    apply_settings(&mut file, &opts.config)?;
    for spec in &opts.recurse_submodules {
        file.add("submodule", None, "active", spec)?;
    }
    file.add("remote", Some(origin), "url", url)?;
    if opts.no_tags {
        file.add("remote", Some(origin), "tagopt", "--no-tags")?;
    }
    file.write(&config_path)?;
    let git = GitHome::open_at(root)?;

    let chosen = RefCell::new(Vec::new());
    let choose = |refs: &[RemoteRef]| {
        let specs = choose_specs(refs, opts, origin, prefer)?;
        chosen.replace(specs.clone());
        Ok(specs)
    };
    let single = opts.single();
    let tags = match (opts.no_tags, !single) {
        (true, _) => TagMode::None,
        (false, true) => TagMode::All,
        (false, false) => TagMode::Default,
    };
    let progress = match (opts.progress, opts.no_progress) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let o = Options::for_clone(opts.quiet, progress, tags);
    let plan = Plan {
        deepen: limits.deepen,
        opts: &o,
        action: "clone",
        choose: Some(&choose),
        filter: limits.filter,
        no_filter: false,
        refetch: false,
    };
    let fetched = fetch_core(&git, Some(origin), &[], &plan)?;
    if fetched.failed {
        bail!("some refs could not be stored");
    }
    Ok(Downloaded {
        remote_refs: fetched.remote_refs,
        specs: chosen.into_inner(),
    })
}

/// What the clone ends up on.
enum Landing {
    Branch(String),
    Detached(ObjectId),
    Unborn(String),
    Nothing,
}

fn peeled_commit(r: &RemoteRef) -> Option<ObjectId> {
    r.peeled.or(r.id)
}

fn landing(refs: &[RemoteRef], opts: &CloneOptions, prefer: &str) -> Landing {
    let find = |name: String| refs.iter().find(|r| r.name == name && r.id.is_some());
    if let Some(b) = &opts.branch {
        if find(format!("refs/heads/{b}")).is_some() {
            return Landing::Branch(b.clone());
        }
        return find(format!("refs/tags/{b}"))
            .and_then(peeled_commit)
            .map_or(Landing::Nothing, Landing::Detached);
    }
    match head_of(refs, prefer) {
        RemoteHead::Branch(b) => Landing::Branch(b),
        RemoteHead::Detached(id) => Landing::Detached(id),
        RemoteHead::Unborn(b) => Landing::Unborn(b),
        RemoteHead::Unknown => Landing::Nothing,
    }
}

fn finish(
    opts: &CloneOptions,
    origin: &str,
    url: &str,
    prefer: &str,
    fetched: Downloaded,
    filter: Option<&ObjectFilter>,
    root: &Path,
) -> Result<()> {
    let git = GitHome::open_at(root)?;
    let refs = &fetched.remote_refs;
    let empty = refs.iter().all(|r| r.id.is_none());
    let landing = match landing(refs, opts, prefer) {
        Landing::Nothing if empty => Landing::Unborn(prefer.to_owned()),
        other => other,
    };
    let message = format!("clone: from {}", redact_text(url));
    let config_path = git.repo.git().common_dir().join("config");
    let mut file = ConfigFile::read(&config_path)?;
    if opts.mirror {
        file.add("remote", Some(origin), "fetch", "+refs/*:refs/*")?;
        file.add("remote", Some(origin), "mirror", "true")?;
    } else if !opts.is_bare() {
        for spec in &fetched.specs {
            file.add("remote", Some(origin), "fetch", spec)?;
        }
        if let Landing::Branch(b) | Landing::Unborn(b) = &landing {
            set_tracking(&mut file, b, origin, Some(&format!("refs/heads/{b}")))?;
        }
    }
    if let Some(f) = filter {
        register_in(&mut file, git.config(), origin, f)?;
    }
    file.write(&config_path)?;
    let git = if filter.is_some() { git.reopen()? } else { git };

    let remote_id = |branch: &str| {
        refs.iter()
            .find(|r| r.name == format!("refs/heads/{branch}"))
            .and_then(|r| r.id)
    };
    let mut commit = None;
    match &landing {
        Landing::Branch(b) => {
            let id = remote_id(b).expect("a landing branch exists on the remote");
            let name = GitHome::branch_ref(b);
            if opts.is_bare() {
                git.repo.refs().set_symbolic("HEAD", &name)?;
            } else {
                git.repo.refs().set_symbolic("HEAD", &name)?;
                git.create_ref(&name, &id, &message)?;
                git.log_ref("HEAD", None, &id, &message, false)?;
            }
            commit = Some(id);
        }
        Landing::Detached(id) if opts.is_bare() => {
            git.repo.refs().update_checked("HEAD", Expect::Any, id)?;
            commit = Some(*id);
        }
        Landing::Detached(id) => {
            git.set_head(None, id, &message)?;
            commit = Some(*id);
        }
        Landing::Unborn(b) => git
            .repo
            .refs()
            .set_symbolic("HEAD", &GitHome::branch_ref(b))?,
        Landing::Nothing => {}
    }
    if !opts.is_bare()
        && let RemoteHead::Branch(b) = head_of(refs, prefer)
    {
        let tracking = format!("refs/remotes/{origin}/{b}");
        if let Some(id) = git.repo.refs().resolve(&tracking)? {
            let head = format!("refs/remotes/{origin}/HEAD");
            git.repo.refs().set_symbolic(&head, &tracking)?;
            git.log_ref(&head, None, &id, &message, true)?;
        }
    }

    if empty {
        eprintln!("warning: You appear to have cloned an empty repository.");
    } else if commit.is_none() && opts.branch.is_none() {
        eprintln!("warning: remote HEAD refers to nonexistent ref, unable to checkout");
    }
    let git = if opts.sparse && !opts.is_bare() {
        super::sparse::init_for_clone(&git)?;
        GitHome::open_at(root)?
    } else {
        git
    };
    if let (false, false, Some(id)) = (opts.is_bare(), opts.no_checkout, commit) {
        let tree = git.repo.commit(&id)?.tree;
        let opened = git.open_index()?;
        if let Err(blocked) = move_tree(&git, &opened, None, Some(&tree), Mode::Force)? {
            blocked.report("checkout", "clone", 128);
        }
        convert::warn_lfs_in_commit(&git, &id)?;
        if let (Landing::Detached(_), false) = (&landing, opts.quiet) {
            detach_advice(&git, &id.to_hex(), &id)?;
        }
    }
    Ok(())
}

/// What a submodule clone needs beyond its url: where the work tree and the git directory go.
pub(super) struct Nested<'a> {
    pub url: &'a str,
    pub work: &'a Path,
    pub git_dir: &'a Path,
    pub branch: Option<&'a str>,
    pub depth: Option<u32>,
    pub quiet: bool,
}

/// Clones into `git_dir` with a `.git` file in `work` pointing at it, and `core.worktree` pointing back.
pub(super) fn clone_nested(n: &Nested) -> Result<()> {
    let mut argv = vec!["clone".to_owned()];
    let worktree = super::submodule::relative(n.git_dir, n.work);
    argv.extend(["-c".into(), format!("core.worktree={}", worktree.display())]);
    if n.quiet {
        argv.push("--quiet".into());
    }
    if let Some(b) = n.branch {
        argv.extend(["--branch".into(), b.to_owned()]);
    }
    if let Some(d) = n.depth {
        argv.extend(["--depth".into(), d.to_string()]);
    }
    let opts: CloneOptions = super::submodule::parsed(&argv);
    let deepen = opts.shallow.request()?;
    let user = user_config(&Env::process())?;
    let prefer = default_branch(&user);
    let existed = n.work.exists();
    std::fs::create_dir_all(n.work)?;
    let link = n.work.join(".git");
    let target = super::submodule::relative(n.work, n.git_dir);
    std::fs::write(&link, format!("gitdir: {}\n", target.display()))?;
    let run = || -> Result<()> {
        let fetched = download(
            &opts,
            &Limits {
                deepen: deepen.as_ref(),
                filter: None,
            },
            "origin",
            n.url,
            &prefer,
            n.git_dir,
            n.work,
        )?;
        finish(&opts, "origin", n.url, &prefer, fetched, None, n.work)
    };
    run().inspect_err(|_| {
        let _ = std::fs::remove_dir_all(n.git_dir);
        let _ = std::fs::remove_file(&link);
        if !existed {
            let _ = std::fs::remove_dir(n.work);
        }
    })
}

/// A relative local-path source made absolute against `cwd`, as git records it.
fn absolute_local(source: &str, cwd: &Path) -> Option<String> {
    let path = cwd.join(source);
    (!source.contains("://")
        && !Path::new(source).is_absolute()
        && (path.is_dir() || path.is_file()))
    .then(|| path.display().to_string())
}

/// `hint` is added to a failure of the transfer, for when the address might have been a pyn server.
pub fn clone(
    source: &str,
    dir: Option<&Path>,
    cwd: &Path,
    opts: &CloneOptions,
    hint: Option<&str>,
) -> Result<()> {
    opts.refuse_unsupported();
    let mut deepen = opts.shallow.request()?;
    let filter = opts.filter.parse()?;
    if opts.is_bare() && opts.separate_git_dir.is_some() {
        bail!("options '--bare' and '--separate-git-dir' cannot be used together");
    }
    let origin = opts.origin.as_deref().unwrap_or("origin");
    if !is_valid_remote_name(origin) {
        bail!("'{origin}' is not a valid remote name");
    }
    let shown = match dir {
        Some(d) => d.to_path_buf(),
        None => PathBuf::from(default_dir(source, opts.is_bare()).ok_or_else(|| {
            anyhow!(
                "No directory name could be guessed.\nPlease specify a directory on the command line"
            )
        })?),
    };
    let target = cwd.join(&shown);
    let absolute = absolute_local(source, cwd);
    let source = absolute.as_deref().unwrap_or(source);
    let existed = target.exists();
    if existed && !(target.is_dir() && std::fs::read_dir(&target)?.next().is_none()) {
        bail!(
            "destination path '{}' already exists and is not an empty directory.",
            shown.display()
        );
    }
    let user = user_config(&Env::process())?;
    let prefer = opts
        .config
        .iter()
        .rev()
        .find_map(|c| {
            let (k, v) = c.split_once('=')?;
            k.eq_ignore_ascii_case("init.defaultbranch")
                .then(|| v.to_owned())
        })
        .unwrap_or_else(|| default_branch(&user));
    if !opts.quiet {
        let bare = if opts.is_bare() {
            "bare repository "
        } else {
            ""
        };
        eprintln!("Cloning into {bare}'{}'...", shown.display());
    }
    if Path::new(source).is_dir() {
        let f = &opts.shallow;
        for (given, flag) in [
            (f.depth.is_some(), "--depth"),
            (f.shallow_since.is_some(), "--shallow-since"),
            (!f.shallow_exclude.is_empty(), "--shallow-exclude"),
        ] {
            if given {
                eprintln!("warning: {flag} is ignored in local clones; use file:// instead.");
            }
        }
        deepen = None;
    }
    let mut wire_filter = filter.as_ref();
    if wire_filter.is_some() && Path::new(source).is_dir() {
        eprintln!("warning: --filter is ignored in local clones; use file:// instead.");
        wire_filter = None;
    }
    let separate = opts
        .separate_git_dir
        .as_deref()
        .map(|d| super::init_git::separate_dir(cwd, d))
        .transpose()?;
    std::fs::create_dir_all(&target)
        .with_context(|| format!("could not create work tree dir '{}'", shown.display()))?;
    let git_dir = match (&separate, opts.is_bare()) {
        (_, true) => target.clone(),
        (Some(dir), false) => dir.clone(),
        (None, false) => target.join(".git"),
    };
    if let Some(dir) = &separate {
        let link = target.join(".git");
        std::fs::write(&link, format!("gitdir: {}\n", dir.display()))
            .with_context(|| format!("cannot write {}", link.display()))?;
    }
    let fetched = download(
        opts,
        &Limits {
            deepen: deepen.as_ref(),
            filter: wire_filter,
        },
        origin,
        source,
        &prefer,
        &git_dir,
        &target,
    )
    .map_err(|e| {
        clean_up(&target, existed, &git_dir, opts.is_bare());
        match hint {
            Some(h) => anyhow!("{e:#}\nhint: {h}"),
            None => e,
        }
    })?;
    finish(
        opts,
        origin,
        source,
        &prefer,
        fetched,
        filter.as_ref(),
        &target,
    )
    .inspect_err(|e| {
        if e.downcast_ref::<super::worktree::Unsafe>().is_some() {
            clean_up(&target, existed, &git_dir, opts.is_bare());
        }
    })?;
    if !opts.recurse_submodules.is_empty() && !opts.is_bare() && !opts.no_checkout {
        super::submodule::after_clone(&target, opts.shallow_submodules, opts.quiet)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyn_object::Algorithm;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(Algorithm::Sha1, &[n; 20]).unwrap()
    }

    fn r(name: &str, n: Option<u8>, target: Option<&str>) -> RemoteRef {
        RemoteRef {
            name: name.into(),
            id: n.map(id),
            peeled: None,
            symref_target: target.map(str::to_owned),
        }
    }

    #[test]
    fn directories_are_named_as_git_names_them() {
        let d = |s: &str| default_dir(s, false);
        assert_eq!(d("https://h/x/repo.git").as_deref(), Some("repo"));
        assert_eq!(d("https://h/x/repo/").as_deref(), Some("repo"));
        assert_eq!(d("https://h/x/repo.git/").as_deref(), Some("repo"));
        assert_eq!(d("git@host:org/repo.git").as_deref(), Some("repo"));
        assert_eq!(d("host:repo").as_deref(), Some("repo"));
        assert_eq!(d("ssh://u@host:22/a/b.git").as_deref(), Some("b"));
        assert_eq!(d("https://example.com/").as_deref(), Some("example.com"));
        assert_eq!(d("ssh://u@host:2222").as_deref(), Some("host"));
        assert_eq!(d("https://h/x/repo/.git").as_deref(), Some("repo"));
        assert_eq!(
            default_dir("https://h/x/repo.git", true).as_deref(),
            Some("repo.git")
        );
        assert_eq!(
            default_dir("https://h/x/repo", true).as_deref(),
            Some("repo.git")
        );
        assert_eq!(d("https://h/"), Some("h".into()));
        assert_eq!(d("/"), None);
        assert_eq!(d("dir/x.bundle").as_deref(), Some("x"));
    }

    #[test]
    fn the_remote_head_is_a_branch_a_commit_or_an_unborn_name() {
        let head = |t: Option<&str>, n| r("HEAD", n, t);
        let main = r("refs/heads/main", Some(1), None);
        let dev = r("refs/heads/dev", Some(2), None);
        let refs = vec![
            head(Some("refs/heads/dev"), Some(2)),
            main.clone(),
            dev.clone(),
        ];
        assert_eq!(head_of(&refs, "master"), RemoteHead::Branch("dev".into()));
        let refs = vec![head(None, Some(2)), main.clone(), dev.clone()];
        assert_eq!(head_of(&refs, "master"), RemoteHead::Branch("dev".into()));
        let twin = r("refs/heads/master", Some(2), None);
        let refs = vec![head(None, Some(2)), main.clone(), dev.clone(), twin];
        assert_eq!(
            head_of(&refs, "master"),
            RemoteHead::Branch("master".into())
        );
        let refs = vec![head(None, Some(9)), main.clone()];
        assert_eq!(head_of(&refs, "master"), RemoteHead::Detached(id(9)));
        let refs = vec![head(Some("refs/heads/trunk"), None), main];
        assert_eq!(head_of(&refs, "master"), RemoteHead::Unborn("trunk".into()));
        assert_eq!(head_of(&[], "master"), RemoteHead::Unknown);
    }

    #[test]
    fn config_keys_split_into_section_subsection_and_key() {
        assert_eq!(
            split_key("http.https://h/.extraHeader").unwrap(),
            (
                "http".into(),
                Some("https://h/".into()),
                "extraHeader".into()
            )
        );
        assert_eq!(
            split_key("core.x").unwrap(),
            ("core".into(), None, "x".into())
        );
        assert!(split_key("nodot").is_err());
        assert!(split_key("a.").is_err());
    }

    #[test]
    fn single_branch_narrows_the_refspec_and_a_missing_branch_fails() {
        let refs = vec![
            r("HEAD", Some(1), Some("refs/heads/main")),
            r("refs/heads/main", Some(1), None),
            r("refs/heads/dev", Some(2), None),
            r("refs/tags/v1", Some(3), None),
        ];
        let o = |args: &[&str]| {
            let mut o = CloneOptions::default();
            for a in args {
                match *a {
                    "single" => o.single_branch = true,
                    "bare" => o.bare = true,
                    b => o.branch = Some(b.to_owned()),
                }
            }
            o
        };
        let spec = |args: &[&str]| choose_specs(&refs, &o(args), "origin", "master").unwrap();
        assert_eq!(spec(&[]), ["+refs/heads/*:refs/remotes/origin/*"]);
        assert_eq!(
            spec(&["single"]),
            ["+refs/heads/main:refs/remotes/origin/main"]
        );
        assert_eq!(
            spec(&["single", "dev"]),
            ["+refs/heads/dev:refs/remotes/origin/dev"]
        );
        assert_eq!(spec(&["single", "v1"]), ["+refs/tags/v1:refs/tags/v1"]);
        assert_eq!(
            spec(&["bare", "single"]),
            ["+refs/heads/main:refs/heads/main"]
        );
        assert_eq!(spec(&["bare"]), ["+refs/heads/*:refs/heads/*"]);
        let e = choose_specs(&refs, &o(&["nope"]), "origin", "master").unwrap_err();
        assert_eq!(
            e.to_string(),
            "Remote branch nope not found in upstream origin"
        );
    }
}

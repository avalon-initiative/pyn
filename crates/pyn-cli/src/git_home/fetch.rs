//! `pyn fetch` on a git repository: git's fetch over the transports, without git.

use std::cell::Cell;
use std::collections::HashSet;
use std::io::{IsTerminal, Write};

use anyhow::{Result, anyhow, bail};
use clap::{ArgAction, Args};
use pyn_git::{ConfigFile, Ref, Refspec, Remote, map_ref, tracking};
use pyn_gittransport::exchange::{fetch_objects, fetch_shallow_with, list_remote_refs};
use pyn_gittransport::{Connector, Env};
use pyn_gitwire::{Deepen, LsRefsOptions, RemoteRef, Transport, UploadPack};
use pyn_gitwork::Head;
use pyn_object::{Algorithm, ObjectId};

use super::GitHome;
use super::convert;
use super::fetch_apply::{Settings, prune, stale_refs, store, tidy_url, truncate_fetch_head};
pub(super) use super::fetch_map::TagMode;
use super::fetch_map::{Entry, Inputs, build, follow_tags};
use super::shallow::FetchShallow;

const DEFAULT_ABBREV: usize = 7;

/// Download objects and refs from a git remote (a git repository only).
#[derive(Debug, Args)]
pub struct FetchArgs {
    /// Fetch every remote, except those with `remote.<name>.skipFetchAll`.
    #[arg(long)]
    all: bool,
    /// Treat every argument as a remote name or group.
    #[arg(long)]
    multiple: bool,
    /// Append to `FETCH_HEAD` instead of overwriting it.
    #[arg(short, long)]
    append: bool,
    /// Delete remote-tracking refs the remote no longer has.
    #[arg(short, long, overrides_with = "no_prune")]
    prune: bool,
    #[arg(long, overrides_with = "prune")]
    no_prune: bool,
    /// With --prune, also delete local tags the remote no longer has.
    #[arg(short = 'P', long, overrides_with = "no_prune_tags")]
    prune_tags: bool,
    #[arg(long, overrides_with = "prune_tags")]
    no_prune_tags: bool,
    /// Fetch every tag.
    #[arg(short, long, overrides_with = "no_tags")]
    tags: bool,
    /// Fetch no tags, not even those that point into the fetched history.
    #[arg(short = 'n', long, overrides_with = "tags")]
    no_tags: bool,
    /// Update refs even when that is not a fast-forward; the same as a `+` on every refspec.
    #[arg(short, long)]
    force: bool,
    /// Allow fetching into the checked-out branch.
    #[arg(short = 'u', long)]
    update_head_ok: bool,
    /// Show what would be done and change no ref, `FETCH_HEAD` or reflog.
    #[arg(long)]
    dry_run: bool,
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
    #[arg(short, long, action = ArgAction::Count)]
    quiet: u8,
    /// Show the server's progress even when standard error is not a terminal.
    #[arg(long, overrides_with = "no_progress")]
    progress: bool,
    #[arg(long, overrides_with = "progress")]
    no_progress: bool,
    /// Accepted; remotes are fetched one after the other.
    #[arg(short, long, value_name = "N")]
    jobs: Option<usize>,
    #[arg(long, hide = true)]
    auto_gc: bool,
    #[arg(long, hide = true)]
    no_auto_gc: bool,
    #[arg(long, hide = true)]
    write_commit_graph: bool,
    #[arg(long, hide = true)]
    no_write_commit_graph: bool,
    #[arg(long, hide = true)]
    no_recurse_submodules: bool,
    #[command(flatten)]
    shallow: FetchShallow,
    #[arg(long, hide = true)]
    update_shallow: bool,
    #[arg(long, hide = true, value_name = "SPEC")]
    filter: Option<String>,
    #[arg(long, hide = true)]
    refetch: bool,
    #[arg(long, hide = true)]
    atomic: bool,
    #[arg(long, hide = true)]
    set_upstream: bool,
    #[arg(long, hide = true, num_args = 0..=1, value_name = "MODE")]
    recurse_submodules: Option<Option<String>>,
    /// A remote name or a URL; the default is the current branch's remote, else `origin`.
    repository: Option<String>,
    /// What to fetch, as `[+]<src>[:<dst>]`; replaces the remote's configured refspecs.
    refspecs: Vec<String>,
}

#[derive(Clone, Copy)]
pub(super) struct Options {
    verbosity: i32,
    dry_run: bool,
    force: bool,
    update_head_ok: bool,
    append: bool,
    tags: TagMode,
    prune: Option<bool>,
    prune_tags: Option<bool>,
    progress: Option<bool>,
    /// A clone's fetch: no `FETCH_HEAD`, no reflog lines, no summary, and the remote's unborn `HEAD` is asked for.
    clone: bool,
}

impl Options {
    fn of(a: &FetchArgs) -> Self {
        let tags = if a.no_tags {
            TagMode::None
        } else if a.tags {
            TagMode::All
        } else {
            TagMode::Default
        };
        let on_off = |on: bool, off: bool| on.then_some(true).or(off.then_some(false));
        Self {
            verbosity: i32::from(a.verbose) - i32::from(a.quiet),
            dry_run: a.dry_run,
            force: a.force,
            update_head_ok: a.update_head_ok,
            append: a.append,
            tags,
            prune: on_off(a.prune, a.no_prune),
            prune_tags: on_off(a.prune_tags, a.no_prune_tags),
            progress: on_off(a.progress, a.no_progress),
            clone: false,
        }
    }

    /// The fetch inside `pyn clone`: every ref is taken as the chosen refspecs say, forced and into the checked-out branch.
    pub(super) fn for_clone(quiet: bool, progress: Option<bool>, tags: TagMode) -> Self {
        Self {
            verbosity: if quiet { -1 } else { 0 },
            force: true,
            update_head_ok: true,
            tags,
            progress,
            clone: true,
            ..Self::plain()
        }
    }

    /// `remote update`: `--prune` when asked, otherwise the configured default.
    pub(super) fn for_update(prune: Option<bool>, verbose: bool) -> Self {
        Self {
            prune,
            verbosity: i32::from(verbose),
            ..Self::plain()
        }
    }

    fn plain() -> Self {
        Self {
            verbosity: 0,
            dry_run: false,
            force: false,
            update_head_ok: false,
            append: false,
            tags: TagMode::Default,
            prune: None,
            prune_tags: None,
            progress: None,
            clone: false,
        }
    }
}

impl FetchArgs {
    fn refuse_unsupported(&self) {
        let shallow = "it is not built; a shallow fetch does not take it";
        let partial = "partial fetch is not built; a fetch always brings every object";
        let given = [
            ("--update-shallow", self.update_shallow, shallow),
            ("--filter", self.filter.is_some(), partial),
            ("--refetch", self.refetch, partial),
            (
                "--atomic",
                self.atomic,
                "it is not built; refs are updated one at a time",
            ),
            (
                "--set-upstream",
                self.set_upstream,
                "it is not built; use `pyn branch --set-upstream-to`",
            ),
            (
                "--recurse-submodules",
                self.recurse_submodules
                    .as_ref()
                    .is_some_and(|m| m.as_deref().is_none_or(|v| v != "no")),
                "submodules are not supported",
            ),
        ];
        if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
            crate::usage_error(&format!("`pyn fetch {flag}`: {why}"));
        }
    }
}

/// Where a fetch goes.
struct Target {
    name: Option<String>,
    url: String,
    remote: Option<Remote>,
}

/// Where a fetch goes; `None` when no remote is named and none can be picked, which git treats as nothing to do.
fn resolve_target(git: &GitHome, repo: Option<&str>) -> Result<Option<Target>> {
    let config = git.config();
    let name = match repo {
        Some(arg) => arg.to_owned(),
        None => match default_remote(git)? {
            Some(name) => name,
            None => return Ok(None),
        },
    };
    let target = match Remote::read(config, &name) {
        Some(r) => match r.urls.first() {
            Some(url) => Target {
                name: Some(name),
                url: url.clone(),
                remote: Some(r),
            },
            None => bail!("'{name}' does not appear to be a git repository"),
        },
        None if repo.is_some() => Target {
            name: None,
            url: name,
            remote: None,
        },
        None => bail!("'{name}' does not appear to be a git repository"),
    };
    Ok(Some(target))
}

/// The current branch's remote, else `origin`, else the only remote there is.
fn default_remote(git: &GitHome) -> Result<Option<String>> {
    if let Head::Branch { name, .. } = git.repo.head()?
        && let Some(remote) = tracking(git.config(), &name).remote
    {
        return Ok(Some(remote));
    }
    let names = remotes_in_order(git);
    Ok(match names.as_slice() {
        _ if names.iter().any(|n| n == "origin") => Some("origin".into()),
        [only] => Some(only.clone()),
        _ => None,
    })
}

/// Remote names in the order the configuration first mentions them.
pub(super) fn remotes_in_order(git: &GitHome) -> Vec<String> {
    let mut seen = HashSet::new();
    git.config()
        .entries()
        .iter()
        .filter(|e| e.section == "remote")
        .filter_map(|e| e.subsection.clone())
        .filter(|n| seen.insert(n.clone()))
        .collect()
}

/// The refspecs as git reads them from the command line: `tag <name>` means `refs/tags/<name>:refs/tags/<name>`.
fn parse_specs(args: &[String]) -> Result<Vec<Refspec>> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let text = match (arg.as_str(), it.as_slice().first()) {
            ("tag", Some(name)) => {
                it.next();
                format!("refs/tags/{name}:refs/tags/{name}")
            }
            _ => arg.clone(),
        };
        out.push(Refspec::parse(&text).map_err(|_| anyhow!("invalid refspec '{arg}'"))?);
    }
    Ok(out)
}

fn abbrev_len(git: &GitHome) -> usize {
    let hex = git.algorithm().digest_len() * 2;
    match git.config().get("core.abbrev") {
        Some(v) if v.eq_ignore_ascii_case("no") => hex,
        Some(v) => v
            .parse::<usize>()
            .map_or(DEFAULT_ABBREV, |n| n.clamp(4, hex)),
        None => DEFAULT_ABBREV,
    }
}

/// Whether nothing has been written to this repository yet: no refs, objects or index.
fn is_pristine(git: &GitHome) -> Result<bool> {
    let objects = git.repo.git().common_dir().join("objects");
    let has_objects = std::fs::read_dir(&objects)?.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let packs =
            name == "pack" && std::fs::read_dir(e.path()).is_ok_and(|mut d| d.next().is_some());
        packs || (name.len() == 2 && name.bytes().all(|b| b.is_ascii_hexdigit()))
    });
    Ok(!has_objects
        && git.repo.refs().list("refs/")?.is_empty()
        && git.repo.head_commit()?.is_none()
        && !git.repo.git().index_path().exists())
}

/// Switches an empty repository to the remote's object format.
fn adopt_format(git: &GitHome, format: Algorithm) -> Result<GitHome> {
    let path = git.repo.git().common_dir().join("config");
    let mut file = ConfigFile::read(&path)?;
    file.set("core", None, "repositoryformatversion", "1")?;
    file.set("extensions", None, "objectformat", format.name())?;
    file.write(&path)?;
    git.reopen()
}

fn open_session(
    git: &GitHome,
    transport: &mut dyn Transport,
) -> Result<(Option<GitHome>, UploadPack)> {
    match UploadPack::open(transport, git.algorithm()) {
        Ok(up) => Ok((None, up)),
        Err(pyn_gitwire::Error::ObjectFormatMismatch { server, local }) => {
            if !is_pristine(git)? {
                bail!(
                    "mismatched algorithms: client {}; server {}",
                    local.name(),
                    server.name()
                );
            }
            let reopened = adopt_format(git, server)?;
            Ok((Some(reopened), UploadPack::open(transport, server)?))
        }
        Err(e) => Err(e.into()),
    }
}

/// Shows the server's sideband progress line by line, as `remote: <text>`.
struct Relay {
    on: bool,
    pending: Vec<u8>,
}

impl Relay {
    fn feed(&mut self, bytes: &[u8]) {
        if !self.on {
            return;
        }
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| matches!(b, b'\n' | b'\r')) {
            let segment: Vec<u8> = self.pending.drain(..=end).collect();
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(b"remote: ");
            let _ = err.write_all(&crate::sanitize::bytes(&segment));
        }
    }

    fn finish(&mut self) {
        if self.on && !self.pending.is_empty() {
            self.feed(b"\n");
        }
    }
}

fn config_bool(git: &GitHome, key: &str) -> Result<Option<bool>> {
    Ok(git.config().get_bool(key)?)
}

fn ref_names(git: &GitHome) -> Result<HashSet<String>> {
    Ok(git
        .repo
        .refs()
        .list("refs/")?
        .into_iter()
        .map(|(n, _)| n)
        .collect())
}

fn unique_ids(entries: &[Entry]) -> Vec<ObjectId> {
    let mut seen = HashSet::new();
    entries
        .iter()
        .map(|e| e.id)
        .filter(|id| seen.insert(*id))
        .collect()
}

/// Fails unless everything `wants` reach is present, as git's connectivity check does.
fn check_connected(git: &GitHome, wants: &[ObjectId]) -> Result<()> {
    let db = git.repo.objects();
    let mut tips: Vec<ObjectId> = git
        .repo
        .refs()
        .list("refs/")?
        .into_iter()
        .map(|(_, i)| i)
        .collect();
    tips.extend(git.repo.refs().resolve("HEAD")?);
    let have = pyn_git::existing(db, &tips)?;
    pyn_git::reachable_objects(db, wants, &have)
        .map(|_| ())
        .map_err(|e| anyhow!("remote did not send all necessary objects: {e}"))
}

/// The refs a remote holds, from its URL, without fetching anything.
pub(super) fn query_refs(git: &GitHome, url: &str) -> Result<Vec<RemoteRef>> {
    let conn = Connector::new(git.config().clone(), Env::process()).connect(url, false)?;
    let mut transport = conn.transport;
    let opts = LsRefsOptions {
        symrefs: true,
        ..LsRefsOptions::default()
    };
    let (server, refs) = list_remote_refs(&mut *transport, None, &opts)?;
    if server != git.algorithm() {
        bail!(
            "mismatched algorithms: client {}; server {}",
            git.algorithm().name(),
            server.name()
        );
    }
    Ok(refs)
}

/// Picks the refspecs once the remote's refs are known.
pub(super) type Choose<'a> = &'a dyn Fn(&[RemoteRef]) -> Result<Vec<String>>;

pub(super) struct Plan<'a> {
    pub(super) deepen: Option<&'a Deepen>,
    pub(super) opts: &'a Options,
    pub(super) action: &'a str,
    pub(super) choose: Option<Choose<'a>>,
}

/// What a fetch learned: whether a ref failed, and the remote's refs.
pub(super) struct Fetched {
    pub(super) failed: bool,
    pub(super) remote_refs: Vec<RemoteRef>,
}

/// Fetches one remote or URL; true when some ref could not be updated.
fn fetch_one(git: &GitHome, repo: Option<&str>, specs: &[String], plan: &Plan<'_>) -> Result<bool> {
    Ok(fetch_core(git, repo, specs, plan)?.failed)
}

pub(super) fn fetch_core(
    git: &GitHome,
    repo: Option<&str>,
    specs: &[String],
    plan: &Plan<'_>,
) -> Result<Fetched> {
    let o = plan.opts;
    let Some(target) = resolve_target(git, repo)? else {
        return Ok(Fetched {
            failed: false,
            remote_refs: Vec::new(),
        });
    };
    let conn = Connector::new(git.config().clone(), Env::process()).connect(&target.url, false)?;
    let url = tidy_url(&conn.display);
    let mut transport = conn.transport;
    let (adopted, mut up) = open_session(git, &mut *transport)?;
    let git = adopted.as_ref().unwrap_or(git);
    let remote_refs: Vec<RemoteRef> = up.ls_refs(&LsRefsOptions {
        prefixes: Vec::new(),
        symrefs: true,
        peel: true,
        unborn: o.clone,
    })?;
    let chosen = plan.choose.map(|c| c(&remote_refs)).transpose()?;
    let specs = chosen.as_deref().unwrap_or(specs);

    if !o.append && !o.dry_run && !o.clone {
        truncate_fetch_head(git)?;
    }
    let name = target.name.as_deref();
    let remote_key = |k: &str| name.map(|n| format!("remote.{n}.{k}"));
    let remote_flag = |k: &str| -> Result<Option<bool>> {
        remote_key(k).map_or(Ok(None), |key| config_bool(git, &key))
    };
    let prune_on = match o.prune {
        Some(p) => p,
        None => match remote_flag("prune")? {
            Some(p) => p,
            None => config_bool(git, "fetch.prune")?.unwrap_or(false),
        },
    };
    let prune_tags_on = match o.prune_tags {
        Some(p) => p,
        None => match remote_flag("prunetags")? {
            Some(p) => p,
            None => config_bool(git, "fetch.prunetags")?.unwrap_or(false),
        },
    };
    let tag_mode = match o.tags {
        TagMode::Default => match remote_key("tagopt").and_then(|k| git.config().get(&k)) {
            Some("--tags") => TagMode::All,
            Some("--no-tags") => TagMode::None,
            _ => TagMode::Default,
        },
        given => given,
    };

    let mut cmdline = parse_specs(specs)?;
    let mut configured = target
        .remote
        .as_ref()
        .map(Remote::fetch_specs)
        .unwrap_or_default();
    if prune_on && prune_tags_on {
        let tag_spec = Refspec::parse("refs/tags/*:refs/tags/*")?;
        match cmdline.is_empty() {
            true => configured.push(tag_spec),
            false => cmdline.push(tag_spec),
        }
    }
    let (merge, has_merge) = match git.repo.head()? {
        Head::Branch { name: branch, .. } => {
            let t = tracking(git.config(), &branch);
            let applies = name.is_some() && t.remote.as_deref() == name;
            let has = !t.merge.is_empty();
            (if applies { t.merge } else { Vec::new() }, has)
        }
        Head::Detached(_) => (Vec::new(), false),
    };

    let db = git.repo.objects();
    let have = |id: &ObjectId| db.contains(id).unwrap_or(false);
    let local = ref_names(git)?;
    let follow = |taken: &[Entry]| follow_tags(&remote_refs, &|n| local.contains(n), &have, taken);
    let map = build(&Inputs {
        remote_refs: &remote_refs,
        cmdline: &cmdline,
        configured: &configured,
        merge: &merge,
        has_merge,
        tags: tag_mode,
        follow: &follow,
    })?;
    let entries = map.entries;

    if !o.update_head_ok {
        for e in &entries {
            if let Some(branch) = e
                .local
                .as_deref()
                .and_then(|l| l.strip_prefix("refs/heads/"))
                && let Some(tree) = git.checked_out_path(branch)?
            {
                bail!(
                    "refusing to fetch into branch 'refs/heads/{branch}' checked out at '{}'",
                    tree.display()
                );
            }
        }
    }

    let settings = Settings {
        clone: o.clone,
        verbosity: if o.clone { -1 } else { o.verbosity },
        dry_run: o.dry_run,
        force: o.force,
        abbrev: abbrev_len(git),
        action: plan.action.to_owned(),
        url,
        remote: target.name.clone(),
        shown: Cell::new(false),
    };
    if prune_on {
        let specs = if cmdline.is_empty() {
            &configured
        } else {
            &cmdline
        };
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        prune(git, &settings, &stale_refs(git, specs, &names)?)?;
    }

    let mut relay = Relay {
        on: o
            .progress
            .unwrap_or(std::io::stderr().is_terminal() && o.verbosity >= 0),
        pending: Vec::new(),
    };
    let wants = unique_ids(&entries);
    if !wants.is_empty() {
        let got = fetch_shallow_with(
            &mut up,
            git.repo.git(),
            &wants,
            plan.deepen.cloned(),
            &mut |m| relay.feed(m),
        )?;
        relay.finish();
        reload_shallow(git)?;
        if got.is_some() {
            check_connected(git, &wants)?;
        }
    }
    for e in &entries {
        if !have(&e.id) {
            bail!("remote did not send {} ({})", e.id, e.name);
        }
    }
    let moved: Vec<ObjectId> = if o.clone || o.dry_run {
        Vec::new()
    } else {
        entries
            .iter()
            .filter(|e| {
                e.local
                    .as_deref()
                    .is_some_and(|l| tip_of(git, l) != Some(e.id))
            })
            .map(|e| e.id)
            .collect()
    };
    let mut failed = store(git, &settings, &entries)?;
    for id in &moved {
        convert::warn_lfs_in_commit(git, id)?;
    }

    if tag_mode == TagMode::Default && map.autotags {
        let mut known = ref_names(git)?;
        known.extend(entries.iter().filter_map(|e| e.local.clone()));
        let extra = follow_tags(&remote_refs, &|n| known.contains(n), &have, &[]);
        if !extra.is_empty() {
            let missing: Vec<ObjectId> = unique_ids(&extra)
                .into_iter()
                .filter(|i| !have(i))
                .collect();
            if !missing.is_empty() {
                fetch_objects(git.repo.git(), &mut *transport, &missing, &mut |m| {
                    relay.feed(m)
                })?;
                relay.finish();
                check_connected(git, &missing)?;
            }
            failed |= store(git, &settings, &extra)?;
        }
    }
    if let (Some(n), true, true) = (name, cmdline.is_empty(), !o.dry_run) {
        follow_remote_head(git, n, &configured, &remote_refs, o.verbosity)?;
    }
    Ok(Fetched {
        failed,
        remote_refs,
    })
}

/// Re-reads `shallow` into the open object database after a fetch changed it.
fn reload_shallow(git: &GitHome) -> Result<()> {
    let repo = git.repo.git();
    git.repo
        .objects()
        .set_shallow(pyn_git::read_shallow(repo.common_dir(), repo.algorithm())?);
    Ok(())
}

pub(super) fn is_shallow(git: &GitHome) -> Result<bool> {
    let repo = git.repo.git();
    Ok(!pyn_git::read_shallow(repo.common_dir(), repo.algorithm())?.is_empty())
}

fn tip_of(git: &GitHome, name: &str) -> Option<ObjectId> {
    git.repo.refs().resolve(name).ok().flatten()
}

/// `remote.<name>.followRemoteHEAD`: keeps `refs/remotes/<name>/HEAD` pointing where the remote's `HEAD` does.
fn follow_remote_head(
    git: &GitHome,
    name: &str,
    configured: &[Refspec],
    remote_refs: &[RemoteRef],
    verbosity: i32,
) -> Result<()> {
    let mode = git
        .config()
        .get(&format!("remote.{name}.followremotehead"))
        .unwrap_or("create")
        .to_ascii_lowercase();
    let Some(branch) = remote_refs
        .iter()
        .find(|r| r.name == "HEAD" && r.id.is_some())
        .and_then(|r| r.symref_target.as_deref())
    else {
        return Ok(());
    };
    let prefix = format!("refs/remotes/{name}/");
    let refs = git.repo.refs();
    let Some(tracking) = map_ref(configured, branch)
        .filter(|t| t.starts_with(&prefix) && refs.resolve(t).ok().flatten().is_some())
    else {
        return Ok(());
    };
    let head = format!("{prefix}HEAD");
    let current = match refs.read(&head)? {
        Some(Ref::Symbolic(t)) => Some(t),
        Some(Ref::Direct(_)) => Some(String::new()),
        None => None,
    };
    let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    let write = match (mode.as_str(), &current) {
        ("never", _) => false,
        (_, None) => true,
        ("always", Some(c)) => *c != tracking,
        (m, Some(c)) if *c != tracking && m.starts_with("warn") => {
            let quiet = m.strip_prefix("warn-if-not-").is_some_and(|b| b == short);
            if verbosity >= 0 && !quiet {
                eprintln!(
                    "warning: the HEAD of '{name}' is now '{short}' but {head} still points at '{}'; it is not updated",
                    c.strip_prefix(&prefix).unwrap_or(c)
                );
            }
            false
        }
        _ => false,
    };
    if write {
        refs.set_symbolic(&head, &tracking)?;
    }
    Ok(())
}

/// The remote names in a `remotes.<group>` setting.
pub(super) fn group_members(git: &GitHome, group: &str) -> Vec<String> {
    git.config()
        .get_all(&format!("remotes.{group}"))
        .iter()
        .flat_map(|v| v.split_whitespace().map(str::to_owned))
        .collect()
}

/// Every remote in configuration order except those with `skipFetchAll` or `skipDefaultUpdate`.
pub(super) fn default_update_set(git: &GitHome) -> Vec<String> {
    let config = git.config();
    remotes_in_order(git)
        .into_iter()
        .filter(|n| {
            let skip = |k: &str| config.get_bool(&format!("remote.{n}.{k}")).ok().flatten();
            skip("skipfetchall") != Some(true) && skip("skipdefaultupdate") != Some(true)
        })
        .collect()
}

pub(super) fn fetch_many(
    git: &GitHome,
    names: &[String],
    o: &Options,
    deepen: Option<&Deepen>,
) -> Result<bool> {
    if !o.dry_run && !o.append {
        truncate_fetch_head(git)?;
    }
    let appending = Options { append: true, ..*o };
    let mut failed = false;
    for name in names {
        if o.verbosity >= 0 {
            println!("Fetching {name}");
        }
        let action = format!("fetch --append --no-auto-gc --no-write-commit-graph {name}");
        let plan = Plan {
            deepen,
            opts: &appending,
            action: &action,
            choose: None,
        };
        match fetch_one(git, Some(name), &[], &plan) {
            Ok(false) => {}
            Ok(true) => {
                eprintln!("error: Could not fetch {name}");
                failed = true;
            }
            Err(e) => {
                eprintln!("fatal: {}", crate::sanitize::text(&format!("{e:#}")));
                eprintln!("error: Could not fetch {name}");
                failed = true;
            }
        }
    }
    Ok(failed)
}

pub(super) fn run(git: &GitHome, a: &FetchArgs, action: &str) -> Result<bool> {
    a.refuse_unsupported();
    let opts = Options::of(a);
    let deepen = a.shallow.request(is_shallow(git)?)?;
    let config = git.config();
    if a.all {
        if a.repository.is_some() {
            bail!("fetch --all does not take a repository argument");
        }
        if !a.refspecs.is_empty() {
            bail!("fetch --all does not make sense with refspecs");
        }
        return fetch_many(git, &default_update_set(git), &opts, deepen.as_ref());
    }
    if a.multiple {
        let names: Vec<String> = a.repository.iter().chain(&a.refspecs).cloned().collect();
        return fetch_many(git, &names, &opts, deepen.as_ref());
    }
    if let Some(group) = a.repository.as_deref()
        && Remote::read(config, group).is_none()
        && !group_members(git, group).is_empty()
    {
        if !a.refspecs.is_empty() {
            bail!("fetch group and refspecs does not make sense");
        }
        return fetch_many(git, &group_members(git, group), &opts, deepen.as_ref());
    }
    let plan = Plan {
        deepen: deepen.as_ref(),
        opts: &opts,
        action,
        choose: None,
    };
    fetch_one(git, a.repository.as_deref(), &a.refspecs, &plan)
}

pub fn fetch(git: &GitHome, args: FetchArgs) -> Result<()> {
    let typed: Vec<String> = std::env::args()
        .skip_while(|a| a != "fetch")
        .skip(1)
        .collect();
    let action = std::iter::once("fetch".to_owned())
        .chain(typed)
        .collect::<Vec<_>>()
        .join(" ");
    let auto_gc = !args.no_auto_gc && !args.dry_run;
    if run(git, &args, &action)? {
        std::process::exit(1);
    }
    if auto_gc {
        super::gc::auto(git);
    }
    Ok(())
}

/// `remote update` of one remote: a plain fetch with no heading; false when it fetched.
pub(super) fn fetch_single(git: &GitHome, name: &str, o: &Options) -> bool {
    let action = format!("fetch {name}");
    let plan = Plan {
        deepen: None,
        opts: o,
        action: &action,
        choose: None,
    };
    match fetch_one(git, Some(name), &[], &plan) {
        Ok(failed) => failed,
        Err(e) => {
            eprintln!("fatal: {}", crate::sanitize::text(&format!("{e:#}")));
            true
        }
    }
}

/// `pyn remote add -f`: fetches the remote just added.
pub fn after_add(git: &GitHome, name: &str) -> Result<()> {
    let root = git.repo.root()?.to_path_buf();
    let fresh =
        super::detect(&root)?.ok_or_else(|| anyhow!("no repository at {}", root.display()))?;
    let opts = Options::plain();
    let action = format!("fetch {name}");
    let plan = Plan {
        deepen: None,
        opts: &opts,
        action: &action,
        choose: None,
    };
    if fetch_one(&fresh, Some(name), &[], &plan)? {
        std::process::exit(1);
    }
    Ok(())
}

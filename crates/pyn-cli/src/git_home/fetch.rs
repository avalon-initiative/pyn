//! `pyn fetch` on a git repository: git's fetch over the transports, without git.

use std::cell::Cell;
use std::collections::HashSet;
use std::io::{IsTerminal, Write};

use anyhow::{Result, anyhow, bail};
use clap::{ArgAction, Args};
use pyn_git::{ConfigFile, Refspec, Remote, tracking};
use pyn_gittransport::exchange::{fetch_objects, fetch_with};
use pyn_gittransport::{Connector, Env};
use pyn_gitwire::{LsRefsOptions, RemoteRef, Transport, UploadPack};
use pyn_gitwork::Head;
use pyn_object::{Algorithm, ObjectId};

use super::GitHome;
use super::fetch_apply::{Settings, prune, stale_refs, store, tidy_url, truncate_fetch_head};
use super::fetch_map::{Entry, Inputs, TagMode, build, follow_tags};

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
    #[arg(long, hide = true, value_name = "DEPTH")]
    depth: Option<String>,
    #[arg(long, hide = true, value_name = "DEPTH")]
    deepen: Option<String>,
    #[arg(long, hide = true, value_name = "DATE")]
    shallow_since: Option<String>,
    #[arg(long, hide = true, value_name = "REF")]
    shallow_exclude: Option<String>,
    #[arg(long, hide = true)]
    unshallow: bool,
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
struct Options {
    verbosity: i32,
    dry_run: bool,
    force: bool,
    update_head_ok: bool,
    append: bool,
    tags: TagMode,
    prune: Option<bool>,
    prune_tags: Option<bool>,
    progress: Option<bool>,
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
        }
    }
}

impl FetchArgs {
    fn refuse_unsupported(&self) {
        let shallow = "shallow fetch is not built; a fetch always brings the whole history";
        let partial = "partial fetch is not built; a fetch always brings every object";
        let given = [
            ("--depth", self.depth.is_some(), shallow),
            ("--deepen", self.deepen.is_some(), shallow),
            ("--shallow-since", self.shallow_since.is_some(), shallow),
            ("--shallow-exclude", self.shallow_exclude.is_some(), shallow),
            ("--unshallow", self.unshallow, shallow),
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
fn remotes_in_order(git: &GitHome) -> Vec<String> {
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
    let root = git.repo.root()?.to_path_buf();
    super::detect(&root)?.ok_or_else(|| anyhow!("the repository at {} vanished", root.display()))
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
            let _ = err.write_all(&segment);
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

struct Plan<'a> {
    opts: &'a Options,
    action: &'a str,
}

/// Fetches one remote or URL; true when some ref could not be updated.
fn fetch_one(git: &GitHome, repo: Option<&str>, specs: &[String], plan: &Plan<'_>) -> Result<bool> {
    let o = plan.opts;
    let Some(target) = resolve_target(git, repo)? else {
        return Ok(false);
    };
    let conn = Connector::new(git.config().clone(), Env::process()).connect(&target.url, false)?;
    let url = tidy_url(&conn.display);
    let mut transport = conn.transport;
    let (adopted, mut up) = open_session(git, &mut *transport)?;
    let git = adopted.as_ref().unwrap_or(git);
    let remote_refs: Vec<RemoteRef> = up.ls_refs(&LsRefsOptions {
        prefixes: Vec::new(),
        symrefs: false,
        peel: true,
        unborn: false,
    })?;

    if !o.append && !o.dry_run {
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
        verbosity: o.verbosity,
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
        prune(git, &settings, &stale_refs(git, specs, &entries)?)?;
    }

    let mut relay = Relay {
        on: o
            .progress
            .unwrap_or(std::io::stderr().is_terminal() && o.verbosity >= 0),
        pending: Vec::new(),
    };
    let wants = unique_ids(&entries);
    if !wants.is_empty() {
        let got = fetch_with(&mut up, git.repo.git(), &wants, &mut |m| relay.feed(m))?;
        relay.finish();
        if got.is_some() {
            check_connected(git, &wants)?;
        }
    }
    for e in &entries {
        if !have(&e.id) {
            bail!("remote did not send {} ({})", e.id, e.name);
        }
    }
    let mut failed = store(git, &settings, &entries)?;

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
    Ok(failed)
}

/// The remote names in a `remotes.<group>` setting.
fn group_members(git: &GitHome, group: &str) -> Vec<String> {
    git.config()
        .get_all(&format!("remotes.{group}"))
        .iter()
        .flat_map(|v| v.split_whitespace().map(str::to_owned))
        .collect()
}

fn fetch_many(git: &GitHome, names: &[String], o: &Options) -> Result<bool> {
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
            opts: &appending,
            action: &action,
        };
        match fetch_one(git, Some(name), &[], &plan) {
            Ok(false) => {}
            Ok(true) => {
                eprintln!("error: Could not fetch {name}");
                failed = true;
            }
            Err(e) => {
                eprintln!("fatal: {e:#}");
                eprintln!("error: Could not fetch {name}");
                failed = true;
            }
        }
    }
    Ok(failed)
}

fn run(git: &GitHome, a: &FetchArgs, action: &str) -> Result<bool> {
    a.refuse_unsupported();
    let opts = Options::of(a);
    let config = git.config();
    if a.all {
        if a.repository.is_some() {
            bail!("fetch --all does not take a repository argument");
        }
        if !a.refspecs.is_empty() {
            bail!("fetch --all does not make sense with refspecs");
        }
        let names: Vec<String> = remotes_in_order(git)
            .into_iter()
            .filter(|n| {
                let skip = |k: &str| config.get_bool(&format!("remote.{n}.{k}")).ok().flatten();
                skip("skipfetchall") != Some(true) && skip("skipdefaultupdate") != Some(true)
            })
            .collect();
        return fetch_many(git, &names, &opts);
    }
    if a.multiple {
        let names: Vec<String> = a.repository.iter().chain(&a.refspecs).cloned().collect();
        return fetch_many(git, &names, &opts);
    }
    if let Some(group) = a.repository.as_deref()
        && Remote::read(config, group).is_none()
        && !group_members(git, group).is_empty()
    {
        if !a.refspecs.is_empty() {
            bail!("fetch group and refspecs does not make sense");
        }
        return fetch_many(git, &group_members(git, group), &opts);
    }
    let plan = Plan {
        opts: &opts,
        action,
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
    if run(git, &args, &action)? {
        std::process::exit(1);
    }
    Ok(())
}

/// `pyn remote add -f`: fetches the remote just added.
pub fn after_add(git: &GitHome, name: &str) -> Result<()> {
    let root = git.repo.root()?.to_path_buf();
    let fresh =
        super::detect(&root)?.ok_or_else(|| anyhow!("no repository at {}", root.display()))?;
    let opts = Options::plain();
    let action = format!("fetch {name}");
    let plan = Plan {
        opts: &opts,
        action: &action,
    };
    if fetch_one(&fresh, Some(name), &[], &plan)? {
        std::process::exit(1);
    }
    Ok(())
}

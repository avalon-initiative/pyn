//! `pyn push` on a git repository: git's push over the transports, without git.

use std::collections::HashSet;
use std::io::Write;

use anyhow::{Result, anyhow, bail};
use clap::{ArgAction, Args};
use pyn_git::{ConfigFile, Expect, Remote, map_ref, set_tracking};
use pyn_gittransport::exchange::push_with;
use pyn_gitwire::{PushOptions, ReceivePack, RefStatus, RefUpdate};
use pyn_gitwork::Head;
use pyn_object::{Kind, ObjectId};

use super::GitHome;
use super::push_plan::{
    Entry, Judge, LocalView, PlanError, Reason, Spec, Status, build, glob_map, judge,
    ref_name_matches,
};

/// Update remote refs and the objects they need on a git remote (a git repository only).
#[derive(Debug, Args)]
pub struct PushArgs {
    /// Update refs even when that is not a fast-forward.
    #[arg(short, long)]
    force: bool,
    /// Refuse to update a ref unless it is where expected: `--force-with-lease[=<ref>[:<expect>]]`.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "", action = ArgAction::Append, value_name = "REF[:EXPECT]")]
    force_with_lease: Vec<String>,
    #[arg(long)]
    no_force_with_lease: bool,
    #[arg(long, hide = true)]
    force_if_includes: bool,
    #[arg(long, hide = true)]
    no_force_if_includes: bool,
    /// Delete the named remote refs.
    #[arg(short, long)]
    delete: bool,
    /// Push every tag.
    #[arg(long)]
    tags: bool,
    /// Also push the annotated tags that point into the history being pushed.
    #[arg(long, overrides_with = "no_follow_tags")]
    follow_tags: bool,
    #[arg(long, overrides_with = "follow_tags")]
    no_follow_tags: bool,
    /// Push every branch.
    #[arg(long, visible_alias = "branches")]
    all: bool,
    /// Make the pushed branch track the remote one.
    #[arg(short = 'u', long)]
    set_upstream: bool,
    /// Show what would be pushed and change nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Update all refs or none.
    #[arg(long)]
    atomic: bool,
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
    #[arg(short, long, action = ArgAction::Count)]
    quiet: u8,
    /// Accepted; only the server's own messages are shown.
    #[arg(long, overrides_with = "no_progress")]
    progress: bool,
    #[arg(long, overrides_with = "progress")]
    no_progress: bool,
    /// Machine-readable output on standard output.
    #[arg(long)]
    porcelain: bool,
    /// Run the `pre-push` hook (the default).
    #[arg(long, overrides_with = "no_verify")]
    verify: bool,
    /// Skip the `pre-push` hook.
    #[arg(long, overrides_with = "verify")]
    no_verify: bool,
    #[arg(short = '4', long = "ipv4", hide = true)]
    ipv4: bool,
    #[arg(short = '6', long = "ipv6", hide = true)]
    ipv6: bool,
    #[arg(long, hide = true)]
    thin: bool,
    #[arg(long, hide = true)]
    no_thin: bool,
    /// Send every ref and delete the remote refs that are gone here.
    #[arg(long)]
    mirror: bool,
    /// Delete remote refs the refspecs map from a local ref that is gone.
    #[arg(long)]
    prune: bool,
    #[arg(long, hide = true, num_args = 0..=1, require_equals = true, default_missing_value = "yes")]
    signed: Option<String>,
    #[arg(short = 'o', long, hide = true)]
    push_option: Vec<String>,
    #[arg(long, hide = true, num_args = 0..=1, require_equals = true, default_missing_value = "yes")]
    recurse_submodules: Option<String>,
    #[arg(long, hide = true)]
    receive_pack: Option<String>,
    #[arg(long, hide = true)]
    exec: Option<String>,
    /// A remote name or a URL; the default is the current branch's push remote.
    repository: Option<String>,
    /// What to push, as `[+]<src>[:<dst>]`, `:<dst>` to delete, or `tag <name>`.
    refspecs: Vec<String>,
}

/// What a push to a pyn server can say.
#[derive(Debug, Default)]
pub struct ServerPush {
    pub set_upstream: bool,
    pub delete: bool,
    pub repository: Option<String>,
    pub refspecs: Vec<String>,
}

impl PushArgs {
    /// Takes out the options a pyn server's push understands, leaving the rest for the bare-command check.
    pub fn take_server(&mut self) -> ServerPush {
        ServerPush {
            set_upstream: std::mem::take(&mut self.set_upstream),
            delete: std::mem::take(&mut self.delete),
            repository: self.repository.take(),
            refspecs: std::mem::take(&mut self.refspecs),
        }
    }

    fn refuse_unsupported(&self) {
        let given = [
            (
                "--signed",
                self.signed.is_some(),
                "signed pushes are not built",
            ),
            (
                "--push-option",
                !self.push_option.is_empty(),
                "push options are not built",
            ),
            (
                "--recurse-submodules",
                self.recurse_submodules
                    .as_deref()
                    .is_some_and(|v| v != "no"),
                "submodules are not supported",
            ),
            (
                "--receive-pack",
                self.receive_pack.is_some(),
                "a different remote program cannot be named",
            ),
            (
                "--exec",
                self.exec.is_some(),
                "a different remote program cannot be named",
            ),
            (
                "--repo",
                std::env::args().any(|a| a == "--repo" || a.starts_with("--repo=")),
                "name the repository as an argument",
            ),
            (
                "--force-if-includes",
                self.force_if_includes,
                "its extra safety check is not built, and ignoring it would push less safely than asked",
            ),
        ];
        if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
            crate::usage_error(&format!("`pyn push {flag}`: {why}"));
        }
    }
}

struct Opts {
    dry_run: bool,
    force: bool,
    atomic: bool,
    verbosity: i32,
    porcelain: bool,
    set_upstream: bool,
}

struct Target {
    name: Option<String>,
    /// What messages call the remote: its name, or the URL typed.
    shown: String,
    /// Set by `remote.<name>.mirror`.
    mirror: bool,
    remote: Option<Remote>,
    urls: Vec<String>,
}

/// A remote's URL without user name or password, as git shows it after `To`.
fn anonymize(url: &str) -> String {
    let Some(at) = url.find('@') else {
        return url.to_owned();
    };
    let anon = |keep: usize| format!("{}{}", &url[..keep], &url[at + 1..]);
    match url.find("://") {
        None => {
            if url[at..].contains(':') && !url.starts_with('/') {
                anon(0)
            } else {
                url.to_owned()
            }
        }
        Some(scheme) => {
            let slash = url[scheme + 3..].find('/').map(|i| i + scheme + 3);
            if slash.is_some_and(|s| s < at) {
                url.to_owned()
            } else {
                anon(scheme + 3)
            }
        }
    }
}

fn pretty(name: &str) -> &str {
    for prefix in ["refs/heads/", "refs/tags/", "refs/remotes/"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest;
        }
    }
    name
}

/// Shows the server's sideband text line by line, as `remote: <text>`.
struct Relay {
    pending: Vec<u8>,
}

impl Relay {
    fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| matches!(b, b'\n' | b'\r')) {
            let segment: Vec<u8> = self.pending.drain(..=end).collect();
            let mut err = std::io::stderr().lock();
            let _ = err.write_all(b"remote: ");
            let _ = err.write_all(&crate::sanitize::bytes(&segment));
        }
    }

    fn finish(&mut self) {
        if !self.pending.is_empty() {
            self.feed(b"\n");
        }
    }
}

fn current_branch(git: &GitHome) -> Result<Option<String>> {
    Ok(match git.repo.head()? {
        Head::Branch { name, .. } => Some(name),
        Head::Detached(_) => None,
    })
}

fn config_get(git: &GitHome, key: &str) -> Option<String> {
    git.config().get(key).map(str::to_owned)
}

/// The remote a plain `push` uses: the branch's `pushRemote`, `remote.pushDefault`, the branch's remote.
fn configured_remote(git: &GitHome, branch: Option<&str>) -> Option<String> {
    branch
        .and_then(|b| config_get(git, &format!("branch.{b}.pushremote")))
        .or_else(|| config_get(git, "remote.pushdefault"))
        .or_else(|| branch.and_then(|b| config_get(git, &format!("branch.{b}.remote"))))
}

fn resolve_target(git: &GitHome, repo: Option<&str>, branch: Option<&str>) -> Result<Target> {
    let (name, implicit) = match repo {
        Some(r) => (r.to_owned(), false),
        None => match configured_remote(git, branch) {
            Some(n) => (n, false),
            None => ("origin".to_owned(), true),
        },
    };
    match Remote::read(git.config(), &name) {
        Some(r) => {
            let urls = r.push_targets().to_vec();
            if urls.is_empty() {
                bail!("'{name}' does not appear to be a git repository");
            }
            let mirror = git
                .config()
                .get_bool(&format!("remote.{name}.mirror"))?
                .unwrap_or(false);
            Ok(Target {
                mirror,
                shown: name.clone(),
                name: Some(name),
                remote: Some(r),
                urls,
            })
        }
        None if implicit => bail!(
            "No configured push destination.\nEither specify the URL from the command-line or configure a remote repository using\n\n    pyn remote add <name> <url>\n\nand then push using the remote name\n\n    pyn push <name>\n"
        ),
        None => Ok(Target {
            mirror: false,
            shown: name.clone(),
            urls: vec![name],
            name: None,
            remote: None,
        }),
    }
}

fn detached_message(remote: &str) -> anyhow::Error {
    anyhow!(
        "You are not currently on a branch.\nTo push the history leading to the current (detached HEAD)\nstate now, use\n\n    pyn push {remote} HEAD:<name-of-remote-branch>\n"
    )
}

/// The refspec `push.default` stands for when none is given.
fn default_spec(git: &GitHome, target: &Target, branch: Option<&str>, mode: &str) -> Result<Spec> {
    let current = || -> Result<Spec> {
        let b = branch.ok_or_else(|| detached_message(&target.shown))?;
        Ok(Spec::parse(&format!("refs/heads/{b}")))
    };
    let fetch_remote = branch
        .and_then(|b| config_get(git, &format!("branch.{b}.remote")))
        .unwrap_or_else(|| "origin".to_owned());
    let triangular = target.name.as_deref() != Some(fetch_remote.as_str());
    let upstream = |simple: bool| -> Result<Spec> {
        let b = branch.ok_or_else(|| detached_message(&target.shown))?;
        let remote = config_get(git, &format!("branch.{b}.remote"));
        let merge = git.config().get_all(&format!("branch.{b}.merge"));
        if merge.is_empty() || remote.is_none() {
            bail!(
                "The current branch {b} has no upstream branch.\nTo push the current branch and set the remote as upstream, use\n\n    pyn push --set-upstream {} {b}\n",
                target.shown
            );
        }
        if merge.len() != 1 {
            bail!("The current branch {b} has multiple upstream branches, refusing to push.");
        }
        if triangular {
            bail!(
                "You are pushing to remote '{}', which is not the upstream of\nyour current branch '{b}', without telling me what to push\nto update which remote branch.",
                target.shown
            );
        }
        if simple && merge[0] != format!("refs/heads/{b}") {
            bail!(
                "The upstream branch of your current branch does not match\nthe name of your current branch.  To push to the upstream branch\non the remote, use\n\n    pyn push {r} HEAD:{m}\n\nTo push to the branch of the same name on the remote, use\n\n    pyn push {r} HEAD\n\nTo choose either option permanently, see push.default in 'pyn config'.\n",
                r = target.shown,
                m = merge[0]
            );
        }
        Ok(Spec::parse(&format!("refs/heads/{b}:{}", merge[0])))
    };
    match mode {
        "nothing" => {
            bail!("You didn't specify any refspecs to push, and push.default is \"nothing\".")
        }
        "matching" => Ok(Spec::parse(":")),
        "current" => current(),
        "upstream" | "tracking" => upstream(false),
        "simple" => {
            if triangular {
                current()
            } else {
                upstream(true)
            }
        }
        other => bail!("malformed value for push.default: {other}"),
    }
}

fn refspecs(
    git: &GitHome,
    args: &PushArgs,
    target: &Target,
    branch: Option<&str>,
) -> Result<Vec<Spec>> {
    if args.mirror || target.mirror {
        if !args.refspecs.is_empty() {
            bail!("--mirror can't be combined with refspecs");
        }
        return Ok(vec![Spec::parse("+refs/*:refs/*")]);
    }
    let mut specs = Vec::new();
    let mut it = args.refspecs.iter();
    while let Some(arg) = it.next() {
        let text = if arg == "tag" {
            let name = it
                .next()
                .ok_or_else(|| anyhow!("tag shorthand without <tag>"))?;
            format!("refs/tags/{name}:refs/tags/{name}")
        } else {
            arg.clone()
        };
        if args.delete {
            if text.contains(':') && arg != "tag" {
                bail!("--delete only accepts plain target ref names");
            }
            let dst = text.split(':').next_back().unwrap_or_default().to_owned();
            specs.push(Spec {
                force: false,
                src: String::new(),
                dst: Some(dst),
            });
        } else {
            let spec = Spec::parse(&text);
            let stars = |s: &str| s.matches('*').count();
            let bad_dst = spec.dst.as_deref().is_some_and(|d| {
                !d.is_empty()
                    && pyn_git::check_ref_name(&format!("refs/x/{}", d.replace('*', "s"))).is_err()
            });
            let mismatch = spec
                .dst
                .as_deref()
                .is_some_and(|d| stars(d) != stars(&spec.src) && !d.is_empty() || stars(d) > 1);
            if bad_dst || mismatch || stars(&spec.src) > 1 {
                bail!("invalid refspec '{arg}'");
            }
            specs.push(spec);
        }
    }
    if args.all {
        return Ok(vec![Spec::parse("refs/heads/*")]);
    }
    if args.tags {
        specs.push(Spec::parse("refs/tags/*"));
    }
    if !specs.is_empty() {
        return Ok(specs);
    }
    let configured: Vec<Spec> = target
        .name
        .as_ref()
        .map(|n| git.config().get_all(&format!("remote.{n}.push")))
        .unwrap_or_default()
        .into_iter()
        .map(Spec::parse)
        .collect();
    if !configured.is_empty() {
        return Ok(configured);
    }
    let mode = config_get(git, "push.default").unwrap_or_else(|| "simple".into());
    Ok(vec![default_spec(
        git,
        target,
        branch,
        &mode.to_ascii_lowercase(),
    )?])
}

fn peel_commit(git: &GitHome, id: &ObjectId) -> Option<ObjectId> {
    let mut current = *id;
    loop {
        let object = git.repo.objects().read(&current).ok()?;
        match object.kind {
            Kind::Commit => return Some(current),
            Kind::Tag => {
                current = pyn_object::Tag::parse(git.algorithm(), &object.data)
                    .ok()?
                    .object;
            }
            _ => return None,
        }
    }
}

/// Commits reachable from `starts`, not entering `stop`.
fn commits_from(git: &GitHome, starts: &[ObjectId], stop: &HashSet<ObjectId>) -> HashSet<ObjectId> {
    let mut seen = HashSet::new();
    let mut queue: Vec<ObjectId> = starts.to_vec();
    while let Some(id) = queue.pop() {
        if stop.contains(&id) || !seen.insert(id) {
            continue;
        }
        if let Ok(c) = git.repo.commit(&id) {
            queue.extend(c.parents);
        }
    }
    seen
}

/// Specs for the annotated tags that point into the history `entries` send and that the remote lacks.
fn follow_tags(
    git: &GitHome,
    local: &[(String, ObjectId)],
    remote: &[(String, ObjectId)],
    entries: &[Entry],
) -> Vec<Spec> {
    let remote_tips: Vec<ObjectId> = remote
        .iter()
        .filter_map(|(_, id)| peel_commit(git, id))
        .collect();
    let known = commits_from(git, &remote_tips, &HashSet::new());
    let tips: Vec<ObjectId> = entries
        .iter()
        .filter_map(|e| e.new.as_ref().and_then(|n| peel_commit(git, n)))
        .collect();
    let sent = commits_from(git, &tips, &known);
    local
        .iter()
        .filter(|(name, _)| {
            name.starts_with("refs/tags/") && !remote.iter().any(|(n, _)| n == name)
        })
        .filter(|(_, id)| {
            git.repo
                .objects()
                .read(id)
                .is_ok_and(|o| o.kind == Kind::Tag)
        })
        .filter(|(_, id)| peel_commit(git, id).is_some_and(|c| sent.contains(&c)))
        .map(|(name, _)| Spec::plain(name, name))
        .collect()
}

/// What `--force-with-lease` asks for.
struct Lease {
    all: bool,
    named: Vec<(String, Option<Option<ObjectId>>)>,
}

fn parse_lease(git: &GitHome, args: &PushArgs) -> Result<Lease> {
    let mut lease = Lease {
        all: false,
        named: Vec::new(),
    };
    if args.no_force_with_lease {
        return Ok(lease);
    }
    for v in &args.force_with_lease {
        if v.is_empty() {
            lease.all = true;
            continue;
        }
        match v.split_once(':') {
            None => lease.named.push((v.clone(), None)),
            Some((name, "")) => lease.named.push((name.to_owned(), Some(None))),
            Some((name, expect)) => {
                let id = ObjectId::from_hex(git.algorithm(), expect)
                    .or_else(|_| git.repo.resolve(expect))
                    .map_err(|_| anyhow!("cannot parse expected object name '{expect}'"))?;
                lease.named.push((name.to_owned(), Some(Some(id))));
            }
        }
    }
    Ok(lease)
}

struct Round<'a> {
    git: &'a GitHome,
    target: &'a Target,
    opts: &'a Opts,
    shown_url: String,
    abbrev: pyn_gitwork::Abbreviations,
    width: usize,
}

impl Round<'_> {
    fn short(&self, id: &ObjectId) -> String {
        self.abbrev.short(id)
    }

    fn describe(&self, e: &Entry) -> (char, String, Option<String>) {
        match &e.status {
            Status::UpToDate => ('=', "[up to date]".into(), None),
            Status::Reject(r) => ('!', "[rejected]".into(), Some(r.text().to_owned())),
            Status::RemoteReject(m) => ('!', "[remote rejected]".into(), m.clone()),
            Status::NoReport => (
                '!',
                "[remote failure]".into(),
                Some("remote failed to report status".into()),
            ),
            Status::Pending | Status::Ok => match (e.old, e.new) {
                (_, None) => ('-', "[deleted]".into(), None),
                (None, Some(_)) => {
                    let what = if e.name.starts_with("refs/tags/") {
                        "[new tag]"
                    } else if e.name.starts_with("refs/heads/") {
                        "[new branch]"
                    } else {
                        "[new reference]"
                    };
                    ('*', what.into(), None)
                }
                (Some(old), Some(new)) => {
                    let dots = if e.forced_update { "..." } else { ".." };
                    (
                        if e.forced_update { '+' } else { ' ' },
                        format!("{}{dots}{}", self.short(&old), self.short(&new)),
                        e.forced_update.then(|| "forced update".to_owned()),
                    )
                }
            },
        }
    }

    fn line(&self, e: &Entry, header: &mut bool) {
        let (flag, summary, msg) = self.describe(e);
        if self.opts.porcelain {
            if !std::mem::replace(header, true) {
                println!("To {}", self.shown_url);
            }
            let from = e.src.as_deref().unwrap_or("");
            match msg {
                Some(m) => println!("{flag}\t{from}:{}\t{summary} ({m})", e.name),
                None => println!("{flag}\t{from}:{}\t{summary}", e.name),
            }
            return;
        }
        if !std::mem::replace(header, true) {
            eprintln!("To {}", self.shown_url);
        }
        let width = self.width;
        let names = match &e.src {
            Some(s) => format!("{} -> {}", pretty(s), pretty(&e.name)),
            None => pretty(&e.name).to_owned(),
        };
        let tail = msg.map(|m| format!(" ({m})")).unwrap_or_default();
        eprintln!(" {flag} {summary:<width$} {names}{tail}");
    }

    fn table(&self, entries: &[Entry]) {
        let mut header = false;
        if self.opts.verbosity > 0 {
            for e in entries.iter().filter(|e| e.status == Status::UpToDate) {
                self.line(e, &mut header);
            }
        }
        for e in entries.iter().filter(|e| e.status == Status::Ok) {
            self.line(e, &mut header);
        }
        for e in entries
            .iter()
            .filter(|e| !matches!(e.status, Status::Ok | Status::UpToDate | Status::Pending))
        {
            self.line(e, &mut header);
        }
    }

    /// `hint:` lines for the first kind of rejection, as git gives them, naming `pyn pull`.
    fn advise(&self, entries: &[Entry], head_ref: Option<&str>) {
        let config = self.git.config();
        let on = |key: &str| config.get_bool(key).ok().flatten().unwrap_or(true);
        if !on("advice.pushupdaterejected") {
            return;
        }
        let rejected = |r: Reason| entries.iter().any(|e| e.status == Status::Reject(r));
        let head = entries.iter().any(|e| {
            e.status == Status::Reject(Reason::NonFastForward)
                && head_ref.is_some()
                && e.src.as_deref() == head_ref
        });
        let (key, lines): (&str, &[&str]) = if head {
            (
                "advice.pushnonffcurrent",
                &[
                    "Updates were rejected because the tip of your current branch is behind",
                    "its remote counterpart. If you want to integrate the remote changes,",
                    "use 'pyn pull' before pushing again.",
                ],
            )
        } else if rejected(Reason::NonFastForward) {
            (
                "advice.pushnonffmatching",
                &[
                    "Updates were rejected because a pushed branch tip is behind its remote",
                    "counterpart. If you want to integrate the remote changes, use 'pyn pull'",
                    "before pushing again.",
                ],
            )
        } else if rejected(Reason::AlreadyExists) {
            (
                "advice.pushalreadyexists",
                &["Updates were rejected because the tag already exists in the remote."],
            )
        } else if rejected(Reason::FetchFirst) {
            (
                "advice.pushfetchfirst",
                &[
                    "Updates were rejected because the remote contains work that you do not",
                    "have locally. This is usually caused by another repository pushing to",
                    "the same ref. If you want to integrate the remote changes, use",
                    "'pyn pull' before pushing again.",
                ],
            )
        } else if rejected(Reason::NeedsForce) {
            (
                "advice.pushneedsforce",
                &[
                    "Updates were rejected because the tip of the remote ref is not a commit",
                    "or the pushed value is not; force the update only if that is intended.",
                ],
            )
        } else {
            return;
        };
        if on(key) {
            for l in lines {
                eprintln!("hint: {l}");
            }
        }
    }

    fn update_tracking(&self, entries: &[Entry]) -> Result<()> {
        let Some(remote) = &self.target.remote else {
            return Ok(());
        };
        let specs = remote.fetch_specs();
        let refs = self.git.repo.refs();
        for e in entries
            .iter()
            .filter(|e| matches!(e.status, Status::Ok | Status::UpToDate))
        {
            let Some(local) = map_ref(&specs, &e.name) else {
                continue;
            };
            let current = refs.resolve(&local)?;
            if self.opts.verbosity > 0 {
                eprintln!("updating local tracking ref '{local}'");
            }
            match (e.new, current) {
                (None, Some(cur)) => {
                    refs.delete(&local, Expect::Id(cur))?;
                    refs.write_reflog(&local, &[])?;
                }
                (Some(new), cur) if cur != Some(new) => {
                    refs.update(&local, &new)?;
                    self.git
                        .log_ref(&local, cur.as_ref(), &new, "update by push", false)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn set_upstreams(&self, entries: &[Entry]) -> Result<()> {
        for e in entries
            .iter()
            .filter(|e| matches!(e.status, Status::Ok | Status::UpToDate) && e.new.is_some())
        {
            let (Some(branch), Some(short)) = (
                e.branch
                    .as_deref()
                    .and_then(|b| b.strip_prefix("refs/heads/")),
                e.name.strip_prefix("refs/heads/"),
            ) else {
                continue;
            };
            let remote = &self.target.shown;
            if self.opts.dry_run {
                if self.opts.verbosity >= 0 {
                    eprintln!("Would set upstream of '{branch}' to '{short}' of '{remote}'");
                }
                continue;
            }
            let path = self.git.repo.git().common_dir().join("config");
            let mut file = ConfigFile::read(&path)?;
            set_tracking(&mut file, branch, remote, Some(&e.name))?;
            file.write(&path)?;
            if self.opts.verbosity >= 0 {
                println!("branch '{branch}' set up to track '{remote}/{short}'.");
            }
        }
        Ok(())
    }
}

fn push_one(
    git: &GitHome,
    target: &Target,
    url: &str,
    specs: &[Spec],
    args: &PushArgs,
    opts: &Opts,
) -> Result<bool> {
    let conn = git.connector().connect(url, true)?;
    let round = Round {
        git,
        target,
        opts,
        shown_url: anonymize(&conn.display),
        abbrev: git.repo.abbreviations(None)?,
        width: 2 * git.repo.abbreviation_len(None)? + 3,
    };
    if opts.verbosity > 0 {
        eprintln!("Pushing to {}", round.shown_url);
    }
    let mut transport = conn.transport;
    let mut rp = match ReceivePack::open(&mut *transport, git.algorithm()) {
        Ok(rp) => rp,
        Err(pyn_gitwire::Error::ObjectFormatMismatch { server, local }) => bail!(
            "mismatched algorithms: client {}; server {}",
            local.name(),
            server.name()
        ),
        Err(e) => return Err(e.into()),
    };
    let remote: Vec<(String, ObjectId)> = rp
        .refs()
        .iter()
        .filter(|r| r.name.starts_with("refs/"))
        .map(|r| (r.name.clone(), r.id))
        .collect();
    let mut local = git.repo.refs().list("refs/")?;
    local.sort_by(|a, b| a.0.cmp(&b.0));
    let head_ref = git.head_target()?;
    let rev = |s: &str| git.repo.resolve(s).ok();
    let view = LocalView {
        refs: &local,
        head_ref: head_ref.as_deref(),
        rev: &rev,
    };
    let mut specs = specs.to_vec();
    if args.mirror || target.mirror {
        specs.extend(
            remote
                .iter()
                .filter(|(name, _)| !local.iter().any(|(l, _)| l == name))
                .map(|(name, _)| Spec {
                    force: false,
                    src: String::new(),
                    dst: Some(name.clone()),
                }),
        );
    }
    if args.prune && !(args.mirror || target.mirror) {
        specs.extend(pruned(&specs, &local, &remote));
    }
    let specs = specs.as_slice();
    let planned = |specs: &[Spec]| match build(specs, &view, &remote) {
        Ok(v) => Ok(v),
        Err(PlanError::Fatal(m)) => Err(anyhow!("{m}")),
    };
    let (mut entries, mut errors) = planned(specs)?;
    let follow = args.follow_tags
        || (!args.no_follow_tags && git.config().get_bool("push.followtags")?.unwrap_or(false));
    if follow {
        let extra = follow_tags(git, &local, &remote, &entries);
        if !extra.is_empty() {
            let mut all = specs.to_vec();
            all.extend(extra);
            (entries, errors) = planned(&all)?;
        }
    }

    let url_shown = round.shown_url.clone();
    let fail = |errors: &[String]| {
        for m in errors {
            eprintln!("error: {m}");
        }
        eprintln!("error: failed to push some refs to '{url_shown}'");
    };
    if !errors.is_empty() {
        fail(&errors);
        return Ok(true);
    }

    let lease = parse_lease(git, args)?;
    let tracking_value = |name: &str| -> Option<ObjectId> {
        let specs = target.remote.as_ref()?.fetch_specs();
        let local = map_ref(&specs, name)?;
        git.repo.refs().resolve(&local).ok().flatten()
    };
    let db = git.repo.objects();
    let has_object = |id: &ObjectId| db.contains(id).unwrap_or(false);
    let peel = |id: &ObjectId| peel_commit(git, id);
    let ancestor = |o: &ObjectId, n: &ObjectId| git.is_ancestor(o, n).unwrap_or(false);
    let lease_of = |e: &Entry| -> Option<Option<ObjectId>> {
        if let Some((_, expect)) = lease
            .named
            .iter()
            .find(|(r, _)| ref_name_matches(r, &e.name))
        {
            return Some(expect.unwrap_or_else(|| tracking_value(&e.name)));
        }
        lease.all.then(|| tracking_value(&e.name))
    };
    judge(
        &mut entries,
        &Judge {
            force_all: opts.force,
            allow_delete: rp.capabilities().has("delete-refs"),
            has_object: &has_object,
            peel_commit: &peel,
            is_ancestor: &ancestor,
            lease: &lease_of,
        },
    );

    let any_rejected = |entries: &[Entry]| {
        entries
            .iter()
            .any(|e| matches!(e.status, Status::Reject(_)))
    };
    if opts.atomic && any_rejected(&entries) {
        for e in entries.iter_mut().filter(|e| e.status == Status::Pending) {
            e.status = Status::Reject(Reason::Atomic);
        }
    }

    git.warn_if_hooks_off(&["pre-push"]);
    if !args.no_verify && pre_push_vetoed(git, &round, url, &entries)? {
        fail(&[]);
        return Ok(true);
    }

    let updates: Vec<RefUpdate> = entries
        .iter()
        .filter(|e| e.status == Status::Pending)
        .map(|e| RefUpdate {
            name: e.name.clone(),
            old: e.old,
            new: e.new,
        })
        .collect();
    if opts.dry_run {
        for e in entries.iter_mut().filter(|e| e.status == Status::Pending) {
            e.status = Status::Ok;
        }
    } else if !updates.is_empty() {
        if opts.atomic && !rp.capabilities().has("atomic") {
            bail!("the receiving end does not support --atomic push");
        }
        let mut relay = Relay {
            pending: Vec::new(),
        };
        let report = push_with(
            &mut rp,
            git.repo.git(),
            &updates,
            &PushOptions {
                atomic: opts.atomic,
                quiet: opts.verbosity < 0,
            },
            &mut |m| relay.feed(m),
        )?;
        relay.finish();
        if let Some(msg) = &report.unpack_error {
            eprintln!("error: remote unpack failed: {msg}");
        }
        for e in entries.iter_mut().filter(|e| e.status == Status::Pending) {
            e.status = match report.refs.iter().find(|r| r.name == e.name) {
                Some(r) => match &r.status {
                    RefStatus::Ok => Status::Ok,
                    RefStatus::Rejected(why) => Status::RemoteReject(
                        (!why.is_empty()).then(|| crate::sanitize::text(why).into_owned()),
                    ),
                },
                None => Status::NoReport,
            };
        }
    }

    let failed = entries.iter().any(|e| {
        matches!(
            e.status,
            Status::Reject(_) | Status::RemoteReject(_) | Status::NoReport
        )
    });
    if opts.verbosity >= 0 || failed {
        round.table(&entries);
    }
    if opts.set_upstream {
        round.set_upstreams(&entries)?;
    }
    if !opts.dry_run {
        round.update_tracking(&entries)?;
    }
    if failed {
        eprintln!("error: failed to push some refs to '{url_shown}'");
        round.advise(&entries, head_ref.as_deref());
    } else if opts.porcelain {
        println!("Done");
    } else if opts.verbosity >= 0
        && !entries
            .iter()
            .any(|e| !matches!(e.status, Status::UpToDate))
    {
        eprintln!("Everything up-to-date");
    }
    Ok(failed)
}

/// Runs `pre-push` with one line per ref about to be sent; true when it exited nonzero.
fn pre_push_vetoed(git: &GitHome, round: &Round, url: &str, entries: &[Entry]) -> Result<bool> {
    let zero = "0".repeat(git.algorithm().digest_len() * 2);
    let hex = |id: &Option<ObjectId>| id.as_ref().map_or_else(|| zero.clone(), ObjectId::to_hex);
    let mut lines = String::new();
    for e in entries.iter().filter(|e| e.status == Status::Pending) {
        let local = e.src.as_deref().unwrap_or("(delete)");
        lines.push_str(&format!(
            "{local} {} {} {}\n",
            hex(&e.new),
            e.name,
            hex(&e.old)
        ));
    }
    let args = [round.target.shown.clone(), url.to_string()];
    Ok(git
        .run_hook("pre-push", &args, Some(lines.as_bytes()), &[])?
        .is_some_and(|s| s != 0))
}

/// Deletions for the remote refs that a glob refspec maps from a local ref that does not exist.
fn pruned(
    specs: &[Spec],
    local: &[(String, ObjectId)],
    remote: &[(String, ObjectId)],
) -> Vec<Spec> {
    let mut out = Vec::new();
    for (name, _) in remote {
        let source = specs.iter().filter(|s| s.src.contains('*')).find_map(|s| {
            let dst = s.dst.as_deref().filter(|d| !d.is_empty()).unwrap_or(&s.src);
            glob_map(dst, &s.src, name)
        });
        if source.is_some_and(|src| !local.iter().any(|(l, _)| *l == src)) {
            out.push(Spec {
                force: false,
                src: String::new(),
                dst: Some(name.clone()),
            });
        }
    }
    out
}

fn run(git: &GitHome, a: &PushArgs) -> Result<bool> {
    a.refuse_unsupported();
    let given = usize::from(a.repository.is_some()) + a.refspecs.len();
    if a.mirror {
        for (flag, on) in [("--all", a.all), ("--tags", a.tags)] {
            if on {
                bail!("options '{flag}' and '--mirror' cannot be used together");
            }
        }
        if a.delete {
            bail!(
                "options '--delete' and '--all/--branches/--mirror/--tags' cannot be used together"
            );
        }
    }
    if a.all && a.tags {
        bail!("options '--all' and '--tags' cannot be used together");
    }
    if a.all && given > 1 {
        bail!("--all can't be combined with refspecs");
    }
    if a.delete && given < 2 {
        bail!("--delete doesn't make sense without any refs");
    }
    if a.delete && (a.all || a.tags) {
        bail!("--delete doesn't make sense with --all or --tags");
    }
    let opts = Opts {
        dry_run: a.dry_run,
        force: a.force,
        atomic: a.atomic,
        verbosity: i32::from(a.verbose) - i32::from(a.quiet),
        porcelain: a.porcelain,
        set_upstream: a.set_upstream,
    };
    let branch = current_branch(git)?;
    let target = resolve_target(git, a.repository.as_deref(), branch.as_deref())?;
    let specs = refspecs(git, a, &target, branch.as_deref())?;
    let mut failed = false;
    for url in &target.urls {
        failed |= push_one(git, &target, url, &specs, a, &opts)?;
    }
    Ok(failed)
}

pub fn push(git: &GitHome, args: PushArgs) -> Result<()> {
    if run(git, &args)? {
        std::process::exit(1);
    }
    Ok(())
}

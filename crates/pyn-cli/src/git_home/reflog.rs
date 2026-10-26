//! `pyn reflog` on a git repository: show, list, check, expire and delete the reflog lines every ref move leaves.

use std::collections::{BTreeSet, HashSet};

use anyhow::{Result, bail};
use chrono::Local;
use clap::{Args, Subcommand};
use pyn_git::{Expect, Ref, ReflogEntry};
use pyn_object::ObjectId;

use super::log_match::parse_date;
use super::{Exits, GitHome, commands::emit};

const ABBREV: usize = 7;

/// Show and prune the reflog.
#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ReflogArgs {
    #[command(subcommand)]
    action: Option<ReflogAction>,
    #[command(flatten)]
    show: ShowArgs,
}

#[derive(Debug, Args, Default)]
struct ShowArgs {
    /// Show at most this many lines.
    #[arg(short = 'n', long = "max-count")]
    max_count: Option<usize>,
    /// The ref whose log to show (default `HEAD`).
    name: Option<String>,
}

#[derive(Debug, Subcommand)]
enum ReflogAction {
    /// Show a ref's reflog, newest first (the default).
    Show(ShowArgs),
    /// List the refs that have a reflog.
    List,
    /// Exit with status 1 unless the full ref name has a reflog.
    Exists { name: String },
    /// Remove old reflog lines.
    Expire(ExpireArgs),
    /// Remove single reflog lines, named `<ref>@{<n>}`.
    Delete(DeleteArgs),
}

#[derive(Debug, Args)]
struct Prune {
    /// Show what would be removed and change nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Print each line kept or removed.
    #[arg(long)]
    verbose: bool,
    /// Make each kept line start from the value the line before it ended at.
    #[arg(long)]
    rewrite: bool,
    /// Set the ref to the newest kept line's value.
    #[arg(long)]
    updateref: bool,
}

#[derive(Debug, Args)]
struct ExpireArgs {
    /// Remove lines older than this (`never`, `all`, a date, `2.weeks.ago`).
    #[arg(long)]
    expire: Option<String>,
    /// Remove lines older than this whose commits the ref no longer reaches.
    #[arg(long)]
    expire_unreachable: Option<String>,
    /// Process every ref that has a reflog.
    #[arg(long)]
    all: bool,
    #[command(flatten)]
    prune: Prune,
    /// The refs to process.
    refs: Vec<String>,
}

#[derive(Debug, Args)]
struct DeleteArgs {
    #[command(flatten)]
    prune: Prune,
    /// The lines to remove, as `<ref>@{<n>}`.
    selectors: Vec<String>,
}

pub fn reflog(git: &GitHome, args: ReflogArgs) -> Result<()> {
    match args.action {
        None => show(git, &args.show),
        Some(ReflogAction::Show(s)) => show(git, &s),
        Some(ReflogAction::List) => list(git),
        Some(ReflogAction::Exists { name }) => {
            if !git.repo.refs().has_reflog(&name) {
                std::process::exit(1);
            }
            Ok(())
        }
        Some(ReflogAction::Expire(a)) => expire(git, a).map_err(exit_255),
        Some(ReflogAction::Delete(a)) if a.selectors.is_empty() => {
            Err(Exits(255, "no reflog specified to delete".into()).into())
        }
        Some(ReflogAction::Delete(a)) => delete(git, a).map_err(exit_255),
    }
}

/// git exits 255 for a failed reflog expire or delete.
fn exit_255(e: anyhow::Error) -> anyhow::Error {
    Exits(255, crate::fatal_message(&e)).into()
}

fn show(git: &GitHome, args: &ShowArgs) -> Result<()> {
    let typed = args.name.as_deref().unwrap_or("HEAD");
    let Some(full) = git.repo.expand_ref_name(typed)? else {
        bail!("ambiguous argument '{typed}': unknown revision or path not in the working tree");
    };
    let shown = match full.strip_prefix("refs/heads/") {
        _ if full == "HEAD" => "HEAD",
        _ if typed.starts_with("refs/") => typed,
        Some(short) => short,
        None => &full,
    };
    let mut out = String::new();
    let log = git.repo.refs().read_reflog(&full)?;
    for (n, entry) in log
        .iter()
        .rev()
        .enumerate()
        .take(args.max_count.unwrap_or(usize::MAX))
    {
        out.push_str(&format!(
            "{} {shown}@{{{n}}}: {}\n",
            &entry.new.to_hex()[..ABBREV],
            entry.message
        ));
    }
    emit(&out);
    Ok(())
}

/// Every ref with a reflog, `HEAD` first.
fn logged_refs(git: &GitHome) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if git.repo.refs().has_reflog("HEAD") {
        names.push("HEAD".to_string());
    }
    let mut rest: BTreeSet<String> = git
        .repo
        .refs()
        .list("refs/")?
        .into_iter()
        .map(|(name, _)| name)
        .filter(|n| git.repo.refs().has_reflog(n))
        .collect();
    if git.repo.refs().has_reflog("refs/stash") {
        rest.insert("refs/stash".into());
    }
    names.extend(rest);
    Ok(names)
}

fn list(git: &GitHome) -> Result<()> {
    let mut out = String::new();
    for name in logged_refs(git)? {
        out.push_str(&name);
        out.push('\n');
    }
    emit(&out);
    Ok(())
}

/// A reflog age limit: lines with an older time are removed. `never` removes nothing, `all` everything.
pub(super) fn limit(text: &str) -> Result<i64> {
    match text {
        "never" | "false" => Ok(0),
        "all" => Ok(i64::MAX),
        _ => {
            let text = if text.ends_with(".ago") {
                text.replace('.', " ")
            } else {
                text.to_string()
            };
            parse_date(&text, Local::now())
        }
    }
}

fn configured_limit(git: &GitHome, key: &str, default: &str) -> Result<i64> {
    limit(git.config().get(key).unwrap_or(default))
}

/// The commits reachable from where `name` points; for `HEAD`, from every ref.
fn tips_reach(git: &GitHome, name: &str) -> Result<HashSet<ObjectId>> {
    let mut tips = Vec::new();
    if name == "HEAD" {
        for (_, id) in git.repo.refs().list("refs/")? {
            tips.push(id);
        }
    }
    if let Some(id) = git.repo.refs().resolve(name)? {
        tips.push(id);
    }
    let mut reach = HashSet::new();
    for tip in tips {
        let Ok(commit) = git.repo.resolve(&tip.to_hex()) else {
            continue;
        };
        reach.extend(git.reachable(&commit)?);
    }
    Ok(reach)
}

/// Applies `decide` to each line oldest first, prints and writes the outcome as the options ask.
fn apply(
    git: &GitHome,
    name: &str,
    prune: &Prune,
    mut decide: impl FnMut(usize, &ReflogEntry) -> Result<bool>,
) -> Result<()> {
    let log = git.repo.refs().read_reflog(name)?;
    let mut kept: Vec<ReflogEntry> = Vec::new();
    let mut last = git.zero_id();
    for (i, entry) in log.iter().enumerate() {
        if decide(i, entry)? {
            if prune.verbose {
                eprintln!("keep {}", entry.message);
            }
            let mut line = entry.clone();
            if prune.rewrite {
                line.old = last;
            }
            last = line.new;
            kept.push(line);
        } else if prune.verbose {
            let word = if prune.dry_run {
                "would prune"
            } else {
                "prune"
            };
            eprintln!("{word} {}", entry.message);
        }
    }
    if prune.dry_run || kept.len() == log.len() && !prune.rewrite {
        return Ok(());
    }
    git.repo.refs().replace_reflog(name, &kept)?;
    let top_pruned =
        log.len() > kept.len() && kept.last().map(|k| k.new) != log.last().map(|l| l.new);
    if prune.updateref && top_pruned && kept.last().is_some() && !is_symbolic(git, name)? {
        set_value(git, name, &last)?;
    }
    Ok(())
}

fn is_symbolic(git: &GitHome, name: &str) -> Result<bool> {
    Ok(name == "HEAD" && matches!(git.repo.git().refs().head()?, Ref::Symbolic(_)))
}

fn set_value(git: &GitHome, name: &str, id: &ObjectId) -> Result<()> {
    git.repo.refs().update_checked(name, Expect::Any, id)?;
    Ok(())
}

fn expand(git: &GitHome, typed: &str) -> Result<String> {
    match git.repo.expand_ref_name(typed)? {
        Some(full) => Ok(full),
        None if git.repo.refs().has_reflog(typed) => Ok(typed.to_string()),
        None => bail!("'{typed}' is not a valid ref"),
    }
}

fn expire(git: &GitHome, args: ExpireArgs) -> Result<()> {
    let total = match &args.expire {
        Some(t) => limit(t)?,
        None => configured_limit(git, "gc.reflogexpire", "90.days.ago")?,
    };
    let unreachable = match &args.expire_unreachable {
        Some(t) => limit(t)?,
        None => configured_limit(git, "gc.reflogexpireunreachable", "30.days.ago")?,
    };
    let names = if args.all {
        logged_refs(git)?
    } else {
        args.refs
            .iter()
            .map(|r| expand(git, r))
            .collect::<Result<Vec<_>>>()?
    };
    for name in names {
        let reach = if unreachable > total {
            Some(tips_reach(git, &name)?)
        } else {
            None
        };
        let outside = |id: &ObjectId, reach: &HashSet<ObjectId>| {
            *id != git.zero_id() && git.repo.commit(id).is_ok() && !reach.contains(id)
        };
        apply(git, &name, &args.prune, |_, e| {
            let time = e.who.time;
            let expired = time < total
                || time < unreachable
                    && match &reach {
                        None => true,
                        Some(r) => outside(&e.old, r) || outside(&e.new, r),
                    };
            Ok(!expired)
        })?;
    }
    Ok(())
}

/// `reflog expire --all` with the configured limits, as `gc` runs it.
pub(super) fn expire_all(git: &GitHome) -> Result<()> {
    expire(
        git,
        ExpireArgs {
            expire: None,
            expire_unreachable: None,
            all: true,
            prune: Prune {
                dry_run: false,
                verbose: false,
                rewrite: false,
                updateref: false,
            },
            refs: Vec::new(),
        },
    )
}

fn delete(git: &GitHome, args: DeleteArgs) -> Result<()> {
    let mut picked = Vec::new();
    for selector in &args.selectors {
        let parsed = selector
            .strip_suffix('}')
            .and_then(|s| s.rsplit_once("@{"))
            .and_then(|(r, n)| Some((r, n.parse::<usize>().ok()?)));
        let Some((typed, n)) = parsed else {
            bail!("not a reflog: {selector}");
        };
        let full = if typed.is_empty() {
            git.head_target()?.unwrap_or_else(|| "HEAD".into())
        } else {
            expand(git, typed)?
        };
        picked.push((full, n));
    }
    for (name, n) in picked {
        let len = git.repo.refs().read_reflog(&name)?.len();
        apply(git, &name, &args.prune, |i, _| Ok(len - 1 - i != n))?;
    }
    Ok(())
}

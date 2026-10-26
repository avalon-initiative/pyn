//! `pyn prune` on a git repository: delete the loose objects nothing reaches.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_object::ObjectId;

use super::GitHome;
use super::commands::emit;
use super::reflog::limit;
use super::storage_links::mark;
use super::storage_roots::collect;

/// Remove unreachable loose objects.
#[derive(Debug, Args)]
pub struct PruneArgs {
    /// List what would be removed and remove nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// List what is removed.
    #[arg(short, long)]
    verbose: bool,
    /// Only remove objects older than this (`now`, `never`, a date, `2.weeks.ago`).
    #[arg(long)]
    expire: Option<String>,
    /// Accepted; progress is never shown.
    #[arg(long)]
    progress: bool,
    /// Also keep what these objects reach.
    heads: Vec<String>,
}

pub(super) struct Prune {
    pub dry_run: bool,
    pub verbose: bool,
    /// Objects with a modification time at or before this are old enough to remove.
    pub expire: i64,
    pub heads: Vec<ObjectId>,
}

/// An age limit for pruning: `now` and `all` mean everything.
pub(super) fn expiry(text: &str) -> Result<i64> {
    match text {
        "now" => Ok(i64::MAX),
        _ => limit(text),
    }
}

pub(super) fn prune(git: &GitHome, args: PruneArgs) -> Result<()> {
    let expire = match &args.expire {
        Some(t) => expiry(t)?,
        None => i64::MAX,
    };
    let mut heads = Vec::new();
    for h in &args.heads {
        let id = ObjectId::from_hex(git.algorithm(), h).or_else(|_| git.repo.resolve(h));
        match id {
            Ok(id) => heads.push(id),
            Err(_) => bail!("not a valid object name: '{h}'"),
        }
    }
    run(
        git,
        &Prune {
            dry_run: args.dry_run,
            verbose: args.verbose,
            expire,
            heads,
        },
    )
}

fn mtime(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

pub(super) fn run(git: &GitHome, opts: &Prune) -> Result<()> {
    let db = git.repo.git().objects()?;
    let found = collect(git, true, true)?;
    if let Some((name, _)) = found.bad_refs.first() {
        bail!("bad ref {name}");
    }
    let mut roots = found.ids();
    roots.extend(&opts.heads);
    let mut keep = mark(&db, &roots, false)?.seen;
    let (loose, garbage) = db.loose_objects()?;
    let recent: Vec<ObjectId> = loose
        .iter()
        .filter(|l| l.mtime > opts.expire)
        .map(|l| l.id)
        .collect();
    keep.extend(mark(&db, &recent, true)?.seen);

    let mut out = String::new();
    for l in loose.iter().filter(|l| !keep.contains(&l.id)) {
        if opts.verbose || opts.dry_run {
            let kind = db.read_opt(&l.id).ok().flatten().map(|o| o.kind.name());
            writeln!(out, "{} {}", l.id, kind.unwrap_or("unknown"))?;
        }
        if !opts.dry_run {
            db.remove_loose(&l.id)?;
        }
    }
    let stale = garbage.into_iter().chain(db.pack_garbage()?).filter(|g| {
        let name = g.path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        (name.starts_with("tmp_obj_") || name.starts_with("tmp_")) && mtime(&g.path) <= opts.expire
    });
    for g in stale {
        if opts.verbose || opts.dry_run {
            writeln!(out, "Removing stale temporary file {}", g.path.display())?;
        }
        if !opts.dry_run {
            let _ = fs::remove_file(&g.path);
            if let Some(dir) = g
                .path
                .parent()
                .filter(|d| d.file_name().is_some_and(|n| n.len() == 2))
            {
                let _ = fs::remove_dir(dir);
            }
        }
    }
    emit(&out);
    Ok(())
}

//! `pyn prune-packed`, `pack-objects` and `unpack-objects` on a git repository.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use pyn_git::{IngestLimits, PackOptions, index_pack, write_pack};
use pyn_object::ObjectId;

use super::GitHome;
use super::storage_links::mark;
use super::storage_roots::collect;

/// Remove loose objects that a pack also holds.
#[derive(Debug, Args)]
pub struct PrunePackedArgs {
    /// List what would be removed and remove nothing.
    #[arg(short = 'n', long, overrides_with = "no_dry_run")]
    dry_run: bool,
    #[arg(long, overrides_with = "dry_run")]
    no_dry_run: bool,
    #[arg(short, long)]
    quiet: bool,
}

impl PrunePackedArgs {
    pub(super) fn quiet() -> Self {
        Self {
            dry_run: false,
            no_dry_run: false,
            quiet: true,
        }
    }
}

fn shown(path: &std::path::Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    path.strip_prefix(&cwd)
        .unwrap_or(path)
        .display()
        .to_string()
}

pub(super) fn prune_packed(git: &GitHome, args: PrunePackedArgs) -> Result<()> {
    let db = git.repo.git().objects()?;
    let (loose, _) = db.loose_objects()?;
    let mut out = String::new();
    for l in loose.iter().filter(|l| db.is_packed(&l.id)) {
        if args.dry_run {
            out.push_str(&format!("rm -f {}\n", shown(&l.path)));
        } else {
            db.remove_loose(&l.id)?;
        }
    }
    super::commands::emit(&out);
    Ok(())
}

/// Write a pack of the listed objects.
#[derive(Debug, Args)]
pub struct PackObjectsArgs {
    /// Where to write `<base-name>-<hash>.pack` and `.idx` (omit with `--stdout`).
    base_name: Option<String>,
    /// Write the pack to standard output.
    #[arg(long)]
    stdout: bool,
    /// Read revisions from standard input and pack what they reach.
    #[arg(long)]
    revs: bool,
    /// With `--revs`, start from every ref.
    #[arg(long)]
    all: bool,
    /// Leave out objects that are already packed.
    #[arg(long)]
    incremental: bool,
    /// Leave out objects borrowed from alternates.
    #[arg(long)]
    local: bool,
    #[arg(short, long)]
    quiet: bool,
    /// Accepted; the pack holds whole objects or offset deltas only.
    #[arg(long)]
    thin: bool,
    #[arg(long)]
    delta_base_offset: bool,
    #[arg(long)]
    no_reuse_delta: bool,
    #[arg(long)]
    no_reuse_object: bool,
    #[arg(long)]
    window: Option<usize>,
    #[arg(long)]
    depth: Option<usize>,
}

fn read_ids(git: &GitHome, args: &PackObjectsArgs) -> Result<Vec<ObjectId>> {
    let db = git.repo.git().objects()?;
    let lines: Vec<String> = std::io::stdin()
        .lock()
        .lines()
        .collect::<std::io::Result<_>>()?;
    if !args.revs {
        let mut ids = Vec::new();
        for line in lines.iter().filter(|l| !l.trim().is_empty()) {
            let hex = line.split_whitespace().next().unwrap_or("");
            let id = ObjectId::from_hex(git.algorithm(), hex)
                .or_else(|_| bail!("expected object ID, got garbage:\n {line}"))?;
            if !db.contains(&id)? {
                bail!("unable to read {id}");
            }
            ids.push(id);
        }
        return Ok(ids);
    }
    let (mut positive, mut negative) = (Vec::new(), Vec::new());
    let mut flip = false;
    if args.all {
        positive.extend(collect(git, false, false)?.ids());
    }
    for line in lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        if line == "--not" {
            flip = !flip;
        } else if let Some(spec) = line.strip_prefix('^') {
            let id = git.repo.resolve(spec)?;
            if flip {
                positive.push(id)
            } else {
                negative.push(id)
            }
        } else {
            let id = git.repo.resolve(line)?;
            if flip {
                negative.push(id)
            } else {
                positive.push(id)
            }
        }
    }
    let skip = mark(&db, &negative, true)?.seen;
    let mut ids: Vec<ObjectId> = mark(&db, &positive, false)?
        .seen
        .into_iter()
        .filter(|id| !skip.contains(id))
        .collect();
    ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(ids)
}

pub(super) fn pack_objects(git: &GitHome, args: PackObjectsArgs) -> Result<()> {
    if args.stdout == args.base_name.is_some() {
        bail!("usage: pyn pack-objects (--stdout | <base-name>) [options]");
    }
    let db = git.repo.git().objects()?;
    let mut ids = read_ids(git, &args)?;
    if args.incremental {
        ids.retain(|id| !db.is_packed(id));
    }
    if args.local {
        ids.retain(|id| db.is_local(id));
    }
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(*id));
    if args.stdout {
        let mut out = std::io::stdout().lock();
        write_pack(&db, &ids, &mut out)?;
        out.flush()?;
        return Ok(());
    }
    let items: Vec<_> = ids
        .iter()
        .map(|id| pyn_git::PackItem { id: *id, hint: 0 })
        .collect();
    let defaults = PackOptions::default();
    let options = PackOptions {
        deltas: true,
        window: args.window.unwrap_or(defaults.window),
        depth: args.depth.unwrap_or(defaults.depth),
        reuse: !(args.no_reuse_delta || args.no_reuse_object),
    };
    let Some(name) = db.write_packfile(&items, &options)? else {
        return Ok(());
    };
    let base = args.base_name.expect("checked above");
    let from = db.dir().join("pack").join(format!("pack-{name}"));
    let to = PathBuf::from(format!("{base}-{name}"));
    if from != to {
        for ext in ["pack", "idx"] {
            fs::copy(from.with_extension(ext), to.with_extension(ext))?;
            let _ = fs::remove_file(from.with_extension(ext));
        }
    }
    println!("{name}");
    Ok(())
}

/// Unpack the objects of a pack read from standard input into loose objects.
#[derive(Debug, Args)]
pub struct UnpackObjectsArgs {
    /// Check the pack but write nothing.
    #[arg(short = 'n')]
    dry_run: bool,
    #[arg(short, long)]
    quiet: bool,
    /// Accepted; objects are checked as they are read.
    #[arg(long)]
    strict: bool,
    /// Accepted; a damaged pack is an error.
    #[arg(short = 'r')]
    recover: bool,
}

pub(super) fn unpack_objects(git: &GitHome, args: UnpackObjectsArgs) -> Result<()> {
    let db = git.repo.git().objects()?;
    let mut input = Vec::new();
    std::io::stdin().lock().read_to_end(&mut input)?;
    let ingested = index_pack(&db, input.as_slice(), &IngestLimits::default())?;
    let pack = ingested.pack;
    let mut objects = Vec::new();
    for id in pack.ids() {
        if let Some(o) = pack.read(&id)? {
            objects.push(o);
        }
    }
    let base = pack.pack_path().with_extension("");
    for ext in ["idx", "rev", "pack"] {
        let _ = fs::remove_file(base.with_extension(ext));
    }
    if args.dry_run {
        return Ok(());
    }
    let fresh = git.repo.git().objects()?;
    for o in objects {
        fresh.write(o.kind, &o.data)?;
    }
    Ok(())
}

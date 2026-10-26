//! `pyn repack` on a git repository: gather objects into packs, with deltas, and drop what they replace.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Result, bail};
use clap::Args;
use pyn_git::{PackItem, PackOptions};
use pyn_object::ObjectId;

use super::GitHome;
use super::prune::expiry;
use super::storage_links::mark;
use super::storage_roots::collect;

/// Pack unpacked objects, or all objects, into packs.
#[derive(Debug, Args)]
pub struct RepackArgs {
    /// Pack everything reachable into one pack.
    #[arg(short = 'a')]
    all: bool,
    /// Like -a, but unreachable objects in packs that go away become loose.
    #[arg(short = 'A')]
    all_loosen: bool,
    /// Remove packs the new pack replaces, and loose objects that are now packed.
    #[arg(short = 'd')]
    delete: bool,
    /// Leave out objects borrowed from alternates.
    #[arg(short = 'l', long)]
    local: bool,
    /// Do not reuse existing deltas (every pack here is built afresh).
    #[arg(short = 'f')]
    no_reuse_delta: bool,
    #[arg(short = 'F')]
    no_reuse_object: bool,
    #[arg(short = 'q', long)]
    quiet: bool,
    /// Accepted; there is no server info to update.
    #[arg(short = 'n')]
    no_update_server_info: bool,
    /// How many objects to try as a delta base.
    #[arg(long)]
    window: Option<usize>,
    /// How long a chain of deltas may be.
    #[arg(long)]
    depth: Option<usize>,
    /// Accepted; packing is single threaded.
    #[arg(long)]
    threads: Option<usize>,
    /// With -A, only loosen unreachable objects from packs newer than this.
    #[arg(long)]
    unpack_unreachable: Option<String>,
    /// Treat the named pack as kept.
    #[arg(long = "keep-pack")]
    keep_pack: Vec<String>,
}

pub(super) struct Repack {
    pub all: bool,
    /// Loosen unreachable packed objects from packs modified after this time; `None` drops them.
    pub loosen: Option<i64>,
    pub delete: bool,
    pub local: bool,
    pub quiet: bool,
    pub keep_packs: Vec<String>,
    pub pack: PackOptions,
}

fn number(git: &GitHome, key: &str, default: usize) -> usize {
    git.config()
        .get(key)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

pub(super) fn repack(git: &GitHome, args: RepackArgs) -> Result<()> {
    let loosen = match (&args.unpack_unreachable, args.all_loosen) {
        (Some(when), _) => Some(expiry(when)?),
        (None, true) => Some(i64::MIN),
        _ => None,
    };
    let defaults = PackOptions::default();
    run(
        git,
        &Repack {
            all: args.all || args.all_loosen,
            loosen,
            delete: args.delete,
            local: args.local,
            quiet: args.quiet,
            keep_packs: args.keep_pack,
            pack: PackOptions {
                deltas: true,
                window: args
                    .window
                    .unwrap_or_else(|| number(git, "pack.window", defaults.window)),
                depth: args
                    .depth
                    .unwrap_or_else(|| number(git, "pack.depth", defaults.depth)),
            },
        },
    )?;
    Ok(())
}

fn mtime(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

/// Per-object times from a cruft pack's `.mtimes`, in index order.
fn cruft_times(pack: &Path, count: usize) -> Option<Vec<i64>> {
    let bytes = fs::read(pack.with_extension("mtimes")).ok()?;
    if bytes.len() < 12 + 4 * count || &bytes[..4] != b"MTME" {
        return None;
    }
    Some(
        (0..count)
            .map(|i| {
                let at = 12 + 4 * i;
                i64::from(u32::from_be_bytes([
                    bytes[at],
                    bytes[at + 1],
                    bytes[at + 2],
                    bytes[at + 3],
                ]))
            })
            .collect(),
    )
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Packs, or removes the leftovers of packing. Returns whether a new pack was written.
pub(super) fn run(git: &GitHome, plan: &Repack) -> Result<bool> {
    let db = git.repo.git().objects()?;
    let found = collect(git, true, true)?;
    if let Some((name, _)) = found.bad_refs.first() {
        bail!("bad ref {name}");
    }
    let reach = mark(&db, &found.ids(), false)?;
    let packs = db.packs();
    let is_kept = |p: &pyn_git::Pack| {
        p.pack_path().with_extension("keep").exists()
            || plan.keep_packs.iter().any(|k| {
                let k = Path::new(k);
                let k = k.file_stem().map(|s| s.to_string_lossy().into_owned());
                k.is_some_and(|k| k == stem(p.pack_path()))
            })
    };
    let kept: Vec<_> = packs.iter().filter(|p| is_kept(p)).collect();
    let in_kept = |id: &ObjectId| kept.iter().any(|p| p.contains(id));

    let mut ids: Vec<ObjectId> = if plan.all {
        reach
            .seen
            .iter()
            .filter(|id| !in_kept(id))
            .filter(|id| !plan.local || db.is_local(id))
            .copied()
            .collect()
    } else {
        let (loose, _) = db.loose_objects()?;
        loose
            .iter()
            .map(|l| l.id)
            .filter(|id| reach.seen.contains(id) && !db.is_packed(id))
            .collect()
    };
    ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let items: Vec<PackItem> = ids
        .iter()
        .map(|id| PackItem {
            id: *id,
            hint: reach.hints.get(id).copied().unwrap_or(0),
        })
        .collect();
    let Some(name) = db.write_packfile(&items, &plan.pack)? else {
        if !plan.quiet {
            println!("Nothing new to pack.");
        }
        return Ok(false);
    };
    if !plan.delete {
        return Ok(true);
    }

    let packed: HashSet<ObjectId> = ids.into_iter().collect();
    let new_stem = format!("pack-{name}");
    let doomed: Vec<_> = packs
        .iter()
        .filter(|p| !is_kept(p) && stem(p.pack_path()) != new_stem)
        .filter(|p| plan.all || p.ids().all(|id| packed.contains(&id)))
        .collect();
    if let (true, Some(threshold)) = (plan.all, plan.loosen) {
        let fresh = git.repo.git().objects()?;
        for p in &doomed {
            let pack_stamp = mtime(p.pack_path());
            let times = cruft_times(p.pack_path(), p.len());
            for (n, id) in p.ids().enumerate() {
                let stamp = times.as_ref().map_or(pack_stamp, |t| t[n]);
                if stamp <= threshold || reach.seen.contains(&id) || in_kept(&id) {
                    continue;
                }
                if let Some(o) = p.read(&id)? {
                    let at = UNIX_EPOCH + Duration::from_secs(stamp.max(0) as u64);
                    fresh.write_loose(o.kind, &o.data, Some(at))?;
                }
            }
        }
    }
    for p in &doomed {
        let base = p.pack_path().with_extension("");
        for ext in ["idx", "rev", "bitmap", "mtimes", "pack"] {
            let _ = fs::remove_file(base.with_extension(ext));
        }
    }

    let after = git.repo.git().objects()?;
    let (loose, _) = after.loose_objects()?;
    for l in loose {
        let is_packed = if plan.local {
            after.is_local_packed(&l.id)
        } else {
            after.is_packed(&l.id)
        };
        if is_packed {
            after.remove_loose(&l.id)?;
        }
    }
    Ok(true)
}

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
    /// Compute every delta afresh instead of reusing the ones packs already hold.
    #[arg(short = 'f')]
    no_reuse_delta: bool,
    /// Like -f, and also store every object afresh.
    #[arg(short = 'F')]
    no_reuse_object: bool,
    /// With -a or -A, write the unreachable objects into a cruft pack.
    #[arg(long, overrides_with = "no_cruft")]
    cruft: bool,
    #[arg(long, overrides_with = "cruft")]
    no_cruft: bool,
    /// Drop cruft objects older than this (`now`, a date, `2.weeks.ago`).
    #[arg(long)]
    cruft_expiration: Option<String>,
    /// With -a, keep unreachable objects of the packs that go away, in the new pack.
    #[arg(short = 'k', long)]
    keep_unreachable: bool,
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
    /// Write unreachable objects younger than the limit (all when `None`) into a cruft pack.
    pub cruft: Option<Option<i64>>,
    pub keep_unreachable: bool,
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
            cruft: match (args.cruft, &args.cruft_expiration) {
                (true, Some(when)) => Some(Some(expiry(when)?)),
                (true, None) => Some(None),
                _ => None,
            },
            keep_unreachable: args.keep_unreachable,
            pack: PackOptions {
                deltas: true,
                reuse: !(args.no_reuse_delta || args.no_reuse_object),
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

/// Unreachable objects of this store (loose, and in the packs that are not kept) with their last-modified time.
fn unreachable_objects(
    db: &pyn_git::ObjectDb,
    packs: &[std::sync::Arc<pyn_git::Pack>],
    reachable: &HashSet<ObjectId>,
    is_kept: &dyn Fn(&pyn_git::Pack) -> bool,
    plan: &Repack,
) -> Result<Vec<(ObjectId, i64)>> {
    let mut newest: std::collections::HashMap<ObjectId, i64> = std::collections::HashMap::new();
    let mut note = |id: ObjectId, stamp: i64| {
        let slot = newest.entry(id).or_insert(stamp);
        *slot = (*slot).max(stamp);
    };
    let (loose, _) = db.loose_objects()?;
    for l in &loose {
        if !reachable.contains(&l.id) {
            note(l.id, l.mtime);
        }
    }
    let kept: Vec<_> = packs.iter().filter(|p| is_kept(p)).collect();
    for p in packs.iter().filter(|p| !is_kept(p)) {
        let stamp = mtime(p.pack_path());
        let times = cruft_times(p.pack_path(), p.len());
        for (n, id) in p.ids().enumerate() {
            if reachable.contains(&id) || kept.iter().any(|k| k.contains(&id)) {
                continue;
            }
            if plan.local && !db.is_local(&id) {
                continue;
            }
            note(id, times.as_ref().map_or(stamp, |t| t[n]));
        }
    }
    let mut out: Vec<_> = newest.into_iter().collect();
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
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

    let unreachable = if plan.all && (plan.cruft.is_some() || plan.keep_unreachable) {
        unreachable_objects(&db, &packs, &reach.seen, &is_kept, plan)?
    } else {
        Vec::new()
    };
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
    if plan.keep_unreachable {
        ids.extend(unreachable.iter().map(|(id, _)| *id));
    }
    if db.is_promisor() {
        ids.retain(|id| db.contains(id).unwrap_or(false));
    }
    ids.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let in_promisor = |id: &ObjectId| {
        packs
            .iter()
            .any(|p| db.is_promisor_pack(p) && p.contains(id))
    };
    let (promised, plain): (Vec<ObjectId>, Vec<ObjectId>) =
        ids.iter().partition(|id| in_promisor(id));
    let item = |id: &ObjectId| PackItem {
        id: *id,
        hint: reach.hints.get(id).copied().unwrap_or(0),
    };
    let mut names = Vec::new();
    for (group, promisor) in [(&plain, false), (&promised, true)] {
        let items: Vec<PackItem> = group.iter().map(item).collect();
        if let Some(name) = db.write_packfile(&items, &plan.pack)? {
            if promisor {
                let marker = db.dir().join("pack").join(format!("pack-{name}.promisor"));
                fs::write(&marker, b"")?;
            }
            names.push(name);
        }
    }
    let mut cruft_name = None;
    if let (Some(expire), false) = (plan.cruft, plan.keep_unreachable) {
        let stamps: std::collections::HashMap<ObjectId, i64> = unreachable
            .iter()
            .filter(|(_, t)| expire.is_none_or(|e| *t > e))
            .copied()
            .collect();
        let items: Vec<PackItem> = stamps
            .keys()
            .map(|id| PackItem { id: *id, hint: 0 })
            .collect();
        if let Some(name) = db.write_packfile(&items, &plan.pack)? {
            db.write_mtimes(&name, |id| stamps[id].clamp(0, i64::from(u32::MAX)) as u32)?;
            cruft_name = Some(name);
        }
    }
    if names.is_empty() && cruft_name.is_none() {
        if !plan.quiet {
            println!("Nothing new to pack.");
        }
        return Ok(false);
    }
    if !plan.delete {
        return Ok(true);
    }

    let packed: HashSet<ObjectId> = ids.into_iter().collect();
    let new_stems: Vec<String> = names
        .iter()
        .chain(&cruft_name)
        .map(|n| format!("pack-{n}"))
        .collect();
    let doomed: Vec<_> = packs
        .iter()
        .filter(|p| !is_kept(p) && !new_stems.contains(&stem(p.pack_path())))
        .filter(|p| plan.all || p.ids().all(|id| packed.contains(&id)))
        .collect();
    if let (true, Some(threshold), None, false) =
        (plan.all, plan.loosen, plan.cruft, plan.keep_unreachable)
    {
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
        for ext in ["idx", "rev", "bitmap", "mtimes", "promisor", "pack"] {
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

//! `pyn count-objects` on a git repository: how many loose objects and packs there are and the space they take.

use std::fmt::Write as _;

use anyhow::Result;
use clap::Args;

use super::GitHome;
use super::commands::emit;

/// Count unpacked objects and the disk space they use.
#[derive(Debug, Args)]
pub struct CountObjectsArgs {
    /// Also report packs, the loose objects a pack already holds, garbage and alternates.
    #[arg(short, long)]
    verbose: bool,
    /// Print sizes in KiB, MiB and GiB.
    #[arg(short = 'H', long)]
    human_readable: bool,
}

/// git's `strbuf_humanise_bytes`.
pub(super) fn humanise(bytes: u64) -> String {
    let two = |whole: u64, frac: u64, unit: &str| format!("{whole}.{frac:02} {unit}");
    if bytes > 1 << 30 {
        two(bytes >> 30, (bytes & ((1 << 30) - 1)) / 10_737_419, "GiB")
    } else if bytes > 1 << 20 {
        let x = bytes + 5243;
        two(x >> 20, ((x & ((1 << 20) - 1)) * 100) >> 20, "MiB")
    } else if bytes > 1 << 10 {
        let x = bytes + 5;
        two(x >> 10, ((x & ((1 << 10) - 1)) * 100) >> 10, "KiB")
    } else {
        format!("{bytes} bytes")
    }
}

pub(super) fn count_objects(git: &GitHome, args: CountObjectsArgs) -> Result<()> {
    let db = git.repo.objects();
    let (loose, mut garbage) = db.loose_objects()?;
    garbage.extend(db.pack_garbage()?);
    let loose_bytes: u64 = loose.iter().map(|l| l.disk_bytes).sum();
    let size = |bytes: u64| {
        if args.human_readable {
            humanise(bytes)
        } else {
            (bytes / 1024).to_string()
        }
    };
    let mut out = String::new();
    if !args.verbose {
        let shown = if args.human_readable {
            humanise(loose_bytes)
        } else {
            format!("{} kilobytes", loose_bytes / 1024)
        };
        writeln!(out, "{} objects, {shown}", loose.len())?;
        emit(&out);
        return Ok(());
    }
    let mut in_pack = 0;
    let mut pack_bytes = 0;
    let packs = db.packs();
    for p in &packs {
        in_pack += p.len();
        for path in [p.pack_path(), p.idx_path()] {
            pack_bytes += std::fs::metadata(path).map_or(0, |m| m.len());
        }
    }
    let packable = loose.iter().filter(|l| db.is_packed(&l.id)).count();
    let garbage_bytes: u64 = garbage.iter().map(|g| g.bytes).sum();
    for g in &garbage {
        eprintln!("warning: garbage found: {}", g.path.display());
    }
    writeln!(out, "count: {}", loose.len())?;
    writeln!(out, "size: {}", size(loose_bytes))?;
    writeln!(out, "in-pack: {in_pack}")?;
    writeln!(out, "packs: {}", packs.len())?;
    writeln!(out, "size-pack: {}", size(pack_bytes))?;
    writeln!(out, "prune-packable: {packable}")?;
    writeln!(out, "garbage: {}", garbage.len())?;
    writeln!(out, "size-garbage: {}", size(garbage_bytes))?;
    for alt in db.alternate_dirs() {
        writeln!(out, "alternate: {}", alt.display())?;
    }
    emit(&out);
    Ok(())
}

//! `pyn gc` on a git repository: pack refs, expire reflogs, repack and prune, with git's `--auto` thresholds.

use std::fs;

use anyhow::Result;
use clap::Args;
use pyn_git::PackOptions;

use super::GitHome;
use super::prune::{self, expiry};
use super::repack::{self, Repack};
use super::{reflog, worktrees};

const DEFAULT_AUTO: i64 = 6700;
const DEFAULT_AUTO_PACK_LIMIT: usize = 50;
const DEFAULT_PRUNE_EXPIRE: &str = "2.weeks.ago";

/// Clean up unnecessary files and optimize the local repository.
#[derive(Debug, Args)]
pub struct GcArgs {
    /// Repack more thoroughly, with a bigger delta window.
    #[arg(long)]
    aggressive: bool,
    /// Do nothing unless the repository has enough loose objects or packs to be worth it.
    #[arg(long)]
    auto: bool,
    #[arg(short, long)]
    quiet: bool,
    /// Prune loose objects older than this (default `gc.pruneExpire`, 2 weeks).
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "", overrides_with = "no_prune")]
    prune: Option<String>,
    /// Do not prune loose objects.
    #[arg(long, overrides_with = "prune")]
    no_prune: bool,
    /// Accepted; there is no other `gc` to wait for.
    #[arg(long)]
    force: bool,
    /// Accepted; every pack is rewritten.
    #[arg(long)]
    keep_largest_pack: bool,
    /// Accepted; `gc` runs in the foreground and writes no cruft packs.
    #[arg(long, overrides_with = "no_detach")]
    detach: bool,
    #[arg(long, overrides_with = "detach")]
    no_detach: bool,
    #[arg(long)]
    cruft: bool,
    #[arg(long)]
    no_cruft: bool,
}

fn config_int(git: &GitHome, key: &str, default: i64) -> i64 {
    git.config()
        .get(key)
        .and_then(|v| {
            let v = v.trim().to_ascii_lowercase();
            let (digits, scale) = match v.chars().last()? {
                'k' => (&v[..v.len() - 1], 1 << 10),
                'm' => (&v[..v.len() - 1], 1 << 20),
                'g' => (&v[..v.len() - 1], 1 << 30),
                _ => (v.as_str(), 1),
            };
            digits.parse::<i64>().ok().map(|n| n.saturating_mul(scale))
        })
        .unwrap_or(default)
}

/// Whether the repository has outgrown its loose objects or its packs, per `gc.auto` and `gc.autoPackLimit`.
fn auto_need(git: &GitHome) -> Result<Option<bool>> {
    let threshold = config_int(git, "gc.auto", DEFAULT_AUTO);
    if threshold <= 0 {
        return Ok(None);
    }
    let db = git.repo.git().objects()?;
    let limit = config_int(git, "gc.autopacklimit", DEFAULT_AUTO_PACK_LIMIT as i64);
    let packs = db
        .packs()
        .iter()
        .filter(|p| !p.pack_path().with_extension("keep").exists())
        .count() as i64;
    if limit > 0 && packs > limit {
        return Ok(Some(true));
    }
    let per_dir = (threshold + 255) / 256;
    let rest = git.algorithm().digest_len() * 2 - 2;
    let in_seventeen = fs::read_dir(db.dir().join("17"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| {
                    let n = e.file_name().to_string_lossy().into_owned();
                    n.len() == rest && n.bytes().all(|b| b.is_ascii_hexdigit())
                })
                .count() as i64
        })
        .unwrap_or(0);
    Ok((in_seventeen > per_dir).then_some(false))
}

/// What `gc` runs: `all` rewrites every pack, otherwise only loose objects are packed.
struct Gc {
    all: bool,
    aggressive: bool,
    prune_expire: Option<String>,
}

fn run(git: &GitHome, gc: &Gc) -> Result<()> {
    let repo = git.repo.git();
    let pack_refs = git
        .config()
        .get_bool("gc.packrefs")
        .ok()
        .flatten()
        .unwrap_or(true);
    if pack_refs {
        repo.refs().pack_refs(&repo.objects()?, true, true)?;
    }
    reflog::expire_all(git)?;

    let defaults = PackOptions::default();
    let (window, depth) = if gc.aggressive {
        (
            config_int(git, "gc.aggressivewindow", 250) as usize,
            config_int(git, "gc.aggressivedepth", 50) as usize,
        )
    } else {
        (
            config_int(git, "pack.window", defaults.window as i64) as usize,
            config_int(git, "pack.depth", defaults.depth as i64) as usize,
        )
    };
    let now = gc.prune_expire.as_deref() == Some("now");
    let loosen = match (&gc.prune_expire, gc.all, now) {
        (Some(when), true, false) => Some(expiry(when)?),
        _ => None,
    };
    repack::run(
        git,
        &Repack {
            all: gc.all,
            loosen,
            delete: true,
            local: true,
            quiet: true,
            keep_packs: Vec::new(),
            pack: PackOptions {
                deltas: true,
                window,
                depth,
            },
        },
    )?;
    if let Some(when) = &gc.prune_expire {
        prune::run(
            git,
            &prune::Prune {
                dry_run: false,
                verbose: false,
                expire: expiry(when)?,
                heads: Vec::new(),
            },
        )?;
    }
    let wt = git
        .config()
        .get("gc.worktreepruneexpire")
        .unwrap_or("3.months.ago")
        .to_string();
    worktrees::prune_expired(git, &wt)?;
    Ok(())
}

pub(super) fn gc(git: &GitHome, args: GcArgs) -> Result<()> {
    let configured = git
        .config()
        .get("gc.pruneexpire")
        .unwrap_or(DEFAULT_PRUNE_EXPIRE)
        .to_string();
    let prune_expire = match (&args.prune, args.no_prune) {
        (_, true) => None,
        (Some(v), _) if !v.is_empty() => Some(v.clone()),
        _ => Some(configured),
    };
    let mut all = true;
    if args.auto {
        let Some(too_many_packs) = auto_need(git)? else {
            return Ok(());
        };
        all = too_many_packs;
        if !args.quiet {
            eprintln!("Auto packing the repository for optimum performance.");
            eprintln!("See \"git help gc\" for manual housekeeping.");
        }
    }
    run(
        git,
        &Gc {
            all,
            aggressive: args.aggressive,
            prune_expire,
        },
    )
}

/// Runs `gc --auto` the way git does after commands that add objects; failures are reported, never fatal.
pub(super) fn auto(git: &GitHome) {
    let args = GcArgs {
        aggressive: false,
        auto: true,
        quiet: false,
        prune: None,
        no_prune: false,
        force: false,
        keep_largest_pack: false,
        detach: false,
        no_detach: false,
        cruft: false,
        no_cruft: false,
    };
    if let Err(e) = gc(git, args) {
        eprintln!("warning: auto gc failed: {e}");
    }
}

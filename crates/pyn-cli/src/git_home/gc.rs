//! `pyn gc` on a git repository: pack refs, expire reflogs, repack and prune, with git's `--auto` thresholds.

use std::fs;

use anyhow::{Result, bail};
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
    /// Run even when `gc.pid` names a running `gc`.
    #[arg(long)]
    force: bool,
    /// Leave the largest pack alone.
    #[arg(long)]
    keep_largest_pack: bool,
    /// Accepted; `gc` runs in the foreground.
    #[arg(long, overrides_with = "no_detach")]
    detach: bool,
    #[arg(long, overrides_with = "detach")]
    no_detach: bool,
    /// Write unreachable objects into a cruft pack (the default, `gc.cruftPacks`).
    #[arg(long, overrides_with = "no_cruft")]
    cruft: bool,
    #[arg(long, overrides_with = "cruft")]
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
    keep_largest: bool,
    cruft: Option<Option<i64>>,
}

const STALE_LOCK_SECS: u64 = 12 * 3600;

fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".into())
}

fn process_alive(pid: u32) -> bool {
    let proc = std::path::Path::new("/proc");
    !proc.exists() || proc.join(pid.to_string()).exists()
}

/// `gc.pid` in the common directory, removed when dropped.
struct GcLock(std::path::PathBuf);

impl Drop for GcLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Takes the `gc.pid` lock; a lock of a running `gc` (or one on another machine) is refused unless `force`.
fn lock(git: &GitHome, force: bool) -> Result<GcLock> {
    let path = git.repo.git().common_dir().join("gc.pid");
    if let Ok(text) = fs::read_to_string(&path) {
        let age = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map_or(0, |d| d.as_secs());
        let mut parts = text.split_whitespace();
        let pid = parts.next().and_then(|p| p.parse::<u32>().ok());
        let host = parts.next().unwrap_or("").to_string();
        if let Some(pid) = pid
            && !force
            && age < STALE_LOCK_SECS
            && (host != hostname() || process_alive(pid))
        {
            bail!("gc is already running on machine '{host}' pid {pid} (use --force if not)");
        }
    }
    fs::write(&path, format!("{} {}\n", std::process::id(), hostname()))?;
    Ok(GcLock(path))
}

/// Packs `--keep-largest-pack` and `gc.bigPackThreshold` leave alone.
fn big_packs(git: &GitHome, largest: bool, all: bool) -> Result<Vec<String>> {
    let db = git.repo.git().objects()?;
    let mut sized: Vec<(u64, String)> = db
        .packs()
        .iter()
        .map(|p| {
            let size = fs::metadata(p.pack_path()).map_or(0, |m| m.len());
            let name = p.pack_path().to_string_lossy().into_owned();
            (size, name)
        })
        .collect();
    sized.sort();
    let mut keep = Vec::new();
    if largest {
        keep.extend(sized.pop().map(|(_, n)| n));
        return Ok(keep);
    }
    let threshold = config_int(git, "gc.bigpackthreshold", 0);
    if all && threshold > 0 {
        keep = sized
            .into_iter()
            .filter(|(size, _)| *size as i64 >= threshold)
            .map(|(_, n)| n)
            .collect();
        let limit = config_int(git, "gc.autopacklimit", DEFAULT_AUTO_PACK_LIMIT as i64);
        if limit > 0 && keep.len() as i64 >= limit {
            keep.clear();
        }
    }
    Ok(keep)
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
            keep_packs: if gc.all {
                big_packs(git, gc.keep_largest, true)?
            } else {
                Vec::new()
            },
            cruft: gc.cruft,
            keep_unreachable: false,
            pack: PackOptions {
                deltas: true,
                reuse: !gc.aggressive,
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
        if git.vetoed("pre-auto-gc", &[], &[])? {
            return Ok(());
        }
        if !args.quiet {
            eprintln!("Auto packing the repository for optimum performance.");
            eprintln!("See \"git help gc\" for manual housekeeping.");
        }
        if report_last_error(git) {
            return Ok(());
        }
    }
    let cruft_wanted = !args.no_cruft
        && (args.cruft
            || git
                .config()
                .get_bool("gc.cruftpacks")
                .ok()
                .flatten()
                .unwrap_or(true));
    let cruft = match (&prune_expire, cruft_wanted && all) {
        (Some(when), true) if when != "now" => Some(Some(prune::expiry(when)?)),
        (None, true) => Some(None),
        _ => None,
    };
    let _lock = lock(git, args.force)?;
    let result = run(
        git,
        &Gc {
            all,
            aggressive: args.aggressive,
            prune_expire,
            keep_largest: args.keep_largest_pack,
            cruft,
        },
    );
    let log = git.repo.git().common_dir().join("gc.log");
    match &result {
        Err(e) if args.auto => {
            let _ = fs::write(&log, format!("error: {e}\n"));
        }
        Ok(()) => {
            let _ = fs::remove_file(&log);
        }
        _ => {}
    }
    result
}

/// An earlier `gc --auto` left `gc.log`: prints it and stops, unless it is older than `gc.logExpiry`.
fn report_last_error(git: &GitHome) -> bool {
    let log = git.repo.git().common_dir().join("gc.log");
    let Ok(text) = fs::read_to_string(&log) else {
        return false;
    };
    let expiry = git.config().get("gc.logexpiry").unwrap_or("1.day.ago");
    let cutoff = prune::expiry(expiry).unwrap_or(i64::MIN);
    let modified = fs::metadata(&log)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(i64::MAX, |d| d.as_secs() as i64);
    if modified <= cutoff {
        let _ = fs::remove_file(&log);
        return false;
    }
    eprintln!(
        "warning: The last gc run reported the following. Please correct the root cause\n\
         and remove {}\nAutomatic cleanup will not be performed until the file is removed.\n\n{text}",
        log.display()
    );
    true
}

/// `gc --quiet`, with `--auto` when `auto`, as `maintenance` runs it.
pub(super) fn quiet(git: &GitHome, auto: bool) -> Result<()> {
    gc(git, plain_args(auto, true))
}

/// Runs `gc --auto` the way git does after commands that add objects; failures are reported, never fatal.
pub(super) fn auto(git: &GitHome) {
    if git.config().get_bool("maintenance.auto").ok().flatten() == Some(false) {
        return;
    }
    if let Err(e) = gc(git, plain_args(true, false)) {
        eprintln!("warning: auto gc failed: {e}");
    }
}

fn plain_args(auto: bool, quiet: bool) -> GcArgs {
    GcArgs {
        aggressive: false,
        auto,
        quiet,
        prune: None,
        no_prune: false,
        force: false,
        keep_largest_pack: false,
        detach: false,
        no_detach: false,
        cruft: false,
        no_cruft: false,
    }
}

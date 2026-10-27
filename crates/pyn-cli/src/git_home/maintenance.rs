//! `pyn maintenance` on a git repository: run the housekeeping tasks and (un)register the repository for them.

use std::fs;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use pyn_git::{ConfigFile, PackItem, PackOptions};

use super::GitHome;
use super::config_flags::global_target;

/// Run tasks to optimize repository data.
#[derive(Debug, Args)]
pub struct MaintenanceArgs {
    #[command(subcommand)]
    command: Maintenance,
}

#[derive(Debug, Subcommand)]
enum Maintenance {
    /// Run the tasks.
    Run {
        /// Run a task only when the repository needs it.
        #[arg(long)]
        auto: bool,
        #[arg(long, overrides_with = "no_quiet")]
        quiet: bool,
        #[arg(long, overrides_with = "quiet")]
        no_quiet: bool,
        /// A task to run (repeatable): `gc`, `loose-objects` or `pack-refs`.
        #[arg(long = "task")]
        tasks: Vec<String>,
    },
    /// Add this repository to the list `maintenance.repo` keeps in the user's config.
    Register,
    /// Remove this repository from that list.
    Unregister {
        /// Do not fail when it is not listed.
        #[arg(long)]
        force: bool,
    },
}

const TASKS: [&str; 6] = [
    "prefetch",
    "gc",
    "commit-graph",
    "loose-objects",
    "incremental-repack",
    "pack-refs",
];
const DEFAULT_LOOSE_AUTO: usize = 100;
const DEFAULT_BATCH: usize = 50_000;

fn repo_path(git: &GitHome) -> Result<String> {
    let repo = git.repo.git();
    let dir = repo.work_tree().unwrap_or_else(|| repo.common_dir());
    Ok(fs::canonicalize(dir)?.display().to_string())
}

fn enabled_tasks(git: &GitHome) -> Vec<&'static str> {
    let on = |t: &str| {
        git.config()
            .get_bool(&format!("maintenance.{t}.enabled"))
            .ok()
            .flatten()
    };
    let incremental = git.config().get("maintenance.strategy") == Some("incremental");
    let picked: Vec<&'static str> = TASKS
        .into_iter()
        .filter(|t| {
            on(t).unwrap_or(
                incremental
                    && matches!(
                        *t,
                        "commit-graph" | "prefetch" | "loose-objects" | "incremental-repack"
                    ),
            )
        })
        .collect();
    if picked.is_empty() {
        vec!["gc"]
    } else {
        picked
    }
}

fn loose_needed(git: &GitHome) -> Result<bool> {
    let db = git.repo.git().objects()?;
    let limit = git
        .config()
        .get("maintenance.loose-objects.auto")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(DEFAULT_LOOSE_AUTO as i64);
    if limit == 0 {
        return Ok(false);
    }
    if limit < 0 {
        return Ok(true);
    }
    let in_seventeen = fs::read_dir(db.dir().join("17"))
        .map(|rd| rd.flatten().count())
        .unwrap_or(0);
    Ok(in_seventeen * 256 > limit as usize)
}

fn pack_loose(git: &GitHome) -> Result<()> {
    let db = git.repo.git().objects()?;
    let batch = git
        .config()
        .get("maintenance.loose-objects.batchsize")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_BATCH);
    let (loose, _) = db.loose_objects()?;
    let items: Vec<PackItem> = loose
        .iter()
        .filter(|l| !db.is_packed(&l.id))
        .take(batch)
        .map(|l| PackItem { id: l.id, hint: 0 })
        .collect();
    if let Some(name) = db.write_packfile(&items, &PackOptions::default())? {
        let dir = db.dir().join("pack");
        for ext in ["pack", "idx"] {
            fs::rename(
                dir.join(format!("pack-{name}.{ext}")),
                dir.join(format!("loose-{name}.{ext}")),
            )?;
        }
    }
    Ok(())
}

fn run_task(git: &GitHome, task: &str, auto: bool) -> Result<()> {
    match task {
        "gc" => super::gc::quiet(git, auto),
        "loose-objects" => {
            if auto && !loose_needed(git)? {
                return Ok(());
            }
            super::pack_plumbing::prune_packed(
                git,
                super::pack_plumbing::PrunePackedArgs::quiet(),
            )?;
            pack_loose(git)
        }
        "pack-refs" => {
            let repo = git.repo.git();
            repo.refs()
                .pack_refs_matching(&repo.objects()?, &["refs/*"], &[], true)?;
            Ok(())
        }
        other => bail!("the '{other}' task is not built"),
    }
}

fn config_edit(
    path: &std::path::Path,
    f: impl FnOnce(&mut ConfigFile) -> Result<()>,
) -> Result<()> {
    let mut file = ConfigFile::read(path)?;
    f(&mut file)?;
    file.write(path)?;
    Ok(())
}

pub(super) fn maintenance(git: &GitHome, args: MaintenanceArgs) -> Result<()> {
    match args.command {
        Maintenance::Run { auto, tasks, .. } => {
            for t in &tasks {
                if !TASKS.contains(&t.as_str()) {
                    eprintln!("error: '{t}' is not a valid task");
                    std::process::exit(129);
                }
            }
            let explicit = !tasks.is_empty();
            let tasks: Vec<String> = if tasks.is_empty() {
                enabled_tasks(git).into_iter().map(str::to_owned).collect()
            } else {
                tasks
            };
            let mut failed = false;
            for t in &tasks {
                if let Err(e) = run_task(git, t, auto) {
                    eprintln!("error: {e}");
                    failed |= explicit;
                }
            }
            if failed {
                std::process::exit(1);
            }
            Ok(())
        }
        Maintenance::Register => {
            let path = repo_path(git)?;
            config_edit(&git.local_config_path(), |c| {
                if c.values("maintenance", None, "auto").is_empty() {
                    c.set("maintenance", None, "auto", "false")?;
                }
                if c.values("maintenance", None, "strategy").is_empty() {
                    c.set("maintenance", None, "strategy", "incremental")?;
                }
                Ok(())
            })?;
            config_edit(&global_target(), |c| {
                if !c.values("maintenance", None, "repo").contains(&path) {
                    c.add("maintenance", None, "repo", &path)?;
                }
                Ok(())
            })
        }
        Maintenance::Unregister { force } => {
            let path = repo_path(git)?;
            let target = global_target();
            let listed = ConfigFile::read(&target)?
                .values("maintenance", None, "repo")
                .contains(&path);
            if !listed {
                if force {
                    return Ok(());
                }
                eprintln!("fatal: repository '{path}' is not registered");
                std::process::exit(128);
            }
            config_edit(&target, |c| {
                c.unset_where("maintenance", None, "repo", |v| v == path);
                Ok(())
            })
        }
    }
}

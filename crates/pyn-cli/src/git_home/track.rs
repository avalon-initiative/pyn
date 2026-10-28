//! Upstream setup for a branch that is created: `--track`, `--no-track`, `branch.autoSetupMerge` and `branch.autoSetupRebase`.

use anyhow::{Result, bail};
use pyn_git::{ConfigFile, Remote};

use super::GitHome;
use super::fetch::remotes_in_order;
use super::refwrite::BRANCHES;
use crate::branch::{TrackArgs, TrackMode};

const REMOTES: &str = "refs/remotes/";

/// What the command line asks for; `Config` leaves it to `branch.autoSetupMerge`.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Want {
    Config,
    Never,
    Direct,
    Inherit,
}

impl Want {
    pub(super) fn of(args: &TrackArgs) -> Self {
        match (args.track, args.no_track) {
            (Some(TrackMode::Direct), _) => Want::Direct,
            (Some(TrackMode::Inherit), _) => Want::Inherit,
            (None, true) => Want::Never,
            (None, false) => Want::Config,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Never,
    Remote,
    Always,
    Inherit,
    Simple,
    Explicit,
}

#[derive(Clone, Copy)]
enum Rebase {
    Never,
    Local,
    Remote,
    Always,
}

/// The `branch.<name>.*` settings to write; `origin` is `None` for a local branch.
pub(super) struct Plan {
    origin: Option<String>,
    merges: Vec<String>,
    rebase: bool,
}

fn settings(git: &GitHome) -> Result<(Mode, Rebase)> {
    let cfg = git.config();
    let merge = match cfg.get("branch.autosetupmerge") {
        Some("always") => Mode::Always,
        Some("inherit") => Mode::Inherit,
        Some("simple") => Mode::Simple,
        _ => match cfg.get_bool("branch.autosetupmerge")? {
            Some(false) => Mode::Never,
            _ => Mode::Remote,
        },
    };
    let rebase = match cfg.get("branch.autosetuprebase") {
        None | Some("never") => Rebase::Never,
        Some("local") => Rebase::Local,
        Some("remote") => Rebase::Remote,
        Some("always") => Rebase::Always,
        Some(other) => {
            bail!("bad config variable 'branch.autosetuprebase': malformed value '{other}'")
        }
    };
    Ok((merge, rebase))
}

fn wants_rebase(setting: Rebase, remote: bool) -> bool {
    match setting {
        Rebase::Never => false,
        Rebase::Local => !remote,
        Rebase::Remote => remote,
        Rebase::Always => true,
    }
}

/// The tracking git sets up for a new branch `new` started at `start` (default `HEAD`), or `None` for none.
pub(super) fn plan(
    git: &GitHome,
    want: Want,
    start: Option<&str>,
    new: &str,
) -> Result<Option<Plan>> {
    let (merge, rebase) = settings(git)?;
    let mode = match want {
        Want::Config => merge,
        Want::Never => Mode::Never,
        Want::Direct => Mode::Explicit,
        Want::Inherit => Mode::Inherit,
    };
    if mode == Mode::Never {
        return Ok(None);
    }
    let name = start.unwrap_or("HEAD");
    let real = super::plumbing::dwim(git, name)?
        .filter(|r| r.starts_with(BRANCHES) || r.starts_with(REMOTES));
    let Some(real) = real else {
        if mode == Mode::Explicit {
            bail!("cannot set up tracking information; starting point '{name}' is not a branch");
        }
        return Ok(None);
    };
    if mode == Mode::Inherit {
        return Ok(inherit(git, &real, rebase));
    }
    let mut hits = Vec::new();
    for remote in remotes_in_order(git) {
        if let Some(r) = Remote::read(git.config(), &remote)
            && let Some(src) = r.fetch_specs().iter().find_map(|s| s.source_of(&real))
        {
            hits.push((remote, src));
        }
    }
    if hits.is_empty() && matches!(mode, Mode::Remote | Mode::Simple) {
        return Ok(None);
    }
    if hits.is_empty() && real.starts_with(REMOTES) {
        if mode == Mode::Explicit {
            bail!("cannot set up tracking information; starting point '{name}' is not a branch");
        }
        return Ok(None);
    }
    if hits.len() > 1 {
        bail!("not tracking: ambiguous information for ref '{real}'");
    }
    let (origin, merges) = match hits.pop() {
        Some((remote, src)) => (Some(remote), vec![src]),
        None => (None, vec![real]),
    };
    let short = merges[0].strip_prefix(BRANCHES).unwrap_or(&merges[0]);
    if mode == Mode::Simple && short != new {
        return Ok(None);
    }
    if origin.is_none() && short == new {
        eprintln!("warning: not setting branch '{new}' as its own upstream");
        return Ok(None);
    }
    let rebase = wants_rebase(rebase, origin.is_some());
    Ok(Some(Plan {
        origin,
        merges,
        rebase,
    }))
}

/// The upstream the start branch has, copied; warns and gives none when it has no complete one.
fn inherit(git: &GitHome, real: &str, rebase: Rebase) -> Option<Plan> {
    let bare = real.strip_prefix(BRANCHES).unwrap_or(real);
    let cfg = git.config();
    let Some(remote) = cfg.get(&format!("branch.{bare}.remote")) else {
        eprintln!("warning: asked to inherit tracking from '{bare}', but no remote is set");
        return None;
    };
    let merges: Vec<String> = cfg
        .get_all(&format!("branch.{bare}.merge"))
        .into_iter()
        .map(str::to_owned)
        .collect();
    if merges.first().is_none_or(|m| m.is_empty()) {
        eprintln!(
            "warning: asked to inherit tracking from '{bare}', but no merge configuration is set"
        );
        return None;
    }
    Some(Plan {
        origin: Some(remote.to_owned()),
        merges,
        rebase: wants_rebase(rebase, true),
    })
}

/// Writes the plan into `branch.<new>.*` and says so.
pub(super) fn apply(git: &GitHome, new: &str, plan: &Plan, quiet: bool) -> Result<()> {
    let path = git.local_config_path();
    let mut file = ConfigFile::read(&path)?;
    let remote = plan.origin.as_deref().unwrap_or(".");
    super::upstream::replace(&mut file, new, "remote", remote)?;
    file.unset_all("branch", Some(new), "merge");
    for m in &plan.merges {
        file.add("branch", Some(new), "merge", m)?;
    }
    if plan.rebase {
        super::upstream::replace(&mut file, new, "rebase", "true")?;
    }
    file.write(&path)?;
    if quiet {
        return Ok(());
    }
    let shown: Vec<String> = plan
        .merges
        .iter()
        .map(|m| {
            let short = m.strip_prefix(BRANCHES).unwrap_or(m);
            match &plan.origin {
                Some(o) => format!("{o}/{short}"),
                None => short.to_owned(),
            }
        })
        .collect();
    let by = if plan.rebase { " by rebasing" } else { "" };
    match shown.as_slice() {
        [one] => println!("branch '{new}' set up to track '{one}'{by}."),
        many => {
            println!("branch '{new}' set up to track{by}:");
            for s in many {
                println!("  {s}");
            }
        }
    }
    Ok(())
}

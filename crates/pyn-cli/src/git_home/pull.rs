//! `pyn pull` on a git repository: `pyn fetch`, then the fetched merge candidate through `pyn merge`.

use std::sync::atomic::Ordering;

use anyhow::{Result, anyhow, bail};
use clap::{ArgAction, Args, Parser};
use pyn_git::{Remote, map_ref, tracking};
use pyn_gitwork::Head;
use pyn_object::ObjectId;

use super::fetch::{FetchArgs, remotes_in_order};
use super::fetch_apply::commit_of;
use super::merge::{BLOCKED_STATUS, QUIET, default_message, initial_checkout};
use super::rebase::PullMode;
use super::signing::SignFlag;
use super::{GitHome, merge};
use crate::merging::MergeArgs;

/// Fetch from a git remote and merge the result into the current branch (a git repository only).
#[derive(Debug, Args)]
pub struct PullArgs {
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
    #[arg(short, long, action = ArgAction::Count)]
    quiet: u8,
    #[arg(long, overrides_with = "no_progress")]
    progress: bool,
    #[arg(long, overrides_with = "progress")]
    no_progress: bool,
    /// Fetch only; the merge is not run.
    #[arg(long)]
    dry_run: bool,
    #[arg(short, long)]
    append: bool,
    #[arg(short, long, overrides_with = "no_prune")]
    prune: bool,
    #[arg(long, overrides_with = "prune")]
    no_prune: bool,
    #[arg(short, long, overrides_with = "no_tags")]
    tags: bool,
    #[arg(long, overrides_with = "tags")]
    no_tags: bool,
    #[arg(short, long)]
    force: bool,
    /// Fast-forward when possible, else merge.
    #[arg(long, overrides_with_all = ["no_ff", "ff_only"])]
    ff: bool,
    /// Always make a merge commit.
    #[arg(long, overrides_with_all = ["ff", "ff_only"])]
    no_ff: bool,
    /// Fail unless the branch can fast-forward.
    #[arg(long, overrides_with_all = ["ff", "no_ff"])]
    ff_only: bool,
    #[arg(long, overrides_with = "no_commit")]
    commit: bool,
    /// Merge but stop before committing.
    #[arg(long, overrides_with = "commit")]
    no_commit: bool,
    /// Accepted: no editor is ever started.
    #[arg(long, overrides_with = "edit")]
    no_edit: bool,
    #[arg(short, long, overrides_with = "no_edit")]
    edit: bool,
    #[arg(short, long, num_args = 0..=1, require_equals = true, value_name = "false|true|merges|interactive")]
    rebase: Option<Option<String>>,
    /// Merge instead of rebasing, whatever the configuration says.
    #[arg(long)]
    no_rebase: bool,
    #[arg(long)]
    all: bool,
    /// Give up a pull that stopped on conflicts.
    #[arg(long, conflicts_with_all = ["continue_pull", "skip"])]
    abort: bool,
    /// Conclude a pull that stopped on conflicts, as `pyn commit` does.
    #[arg(long = "continue", conflicts_with_all = ["abort", "skip"])]
    continue_pull: bool,
    /// Drop the commit a rebase stopped on.
    #[arg(long, conflicts_with_all = ["abort", "continue_pull"])]
    skip: bool,
    #[arg(long, hide = true)]
    squash: bool,
    /// The merge strategy: ort, recursive, resolve, octopus, ours or subtree.
    #[arg(short = 's', long, value_name = "STRATEGY")]
    strategy: Vec<String>,
    /// An option for the merge strategy, such as `ours` or `theirs`.
    #[arg(short = 'X', long, value_name = "OPTION")]
    strategy_option: Vec<String>,
    #[arg(long, hide = true)]
    autostash: bool,
    #[command(flatten)]
    sign: SignFlag,
    #[arg(long, hide = true)]
    signoff: bool,
    #[arg(long, hide = true, value_name = "MODE")]
    cleanup: Option<String>,
    #[arg(long, overrides_with = "no_stat")]
    stat: bool,
    #[arg(short = 'n', long, overrides_with = "stat")]
    no_stat: bool,
    /// Fetch in the checked-out submodules (`yes`, `on-demand` or `no`), and with `yes` update them to the merged result.
    #[arg(long, num_args = 0..=1, require_equals = true, value_name = "MODE", overrides_with = "no_recurse_submodules")]
    recurse_submodules: Option<Option<String>>,
    #[arg(long, overrides_with = "recurse_submodules")]
    no_recurse_submodules: bool,
    #[arg(long, hide = true)]
    allow_unrelated_histories: bool,
    /// A remote name or a URL; the default is the current branch's remote, else `origin`.
    repository: Option<String>,
    /// What to fetch, as `pyn fetch` takes it; the merged refs are the ones fetched for merging.
    refspecs: Vec<String>,
}

#[derive(Parser)]
struct AsFetch {
    #[command(flatten)]
    args: FetchArgs,
}

impl PullArgs {
    /// Whether `--rebase` was given (`--no-rebase` and `--rebase=false` mean merge); clears both, so that only the
    /// remaining options decide whether the command is bare.
    pub fn take_rebase(&mut self) -> bool {
        let no_rebase = std::mem::take(&mut self.no_rebase);
        match self.rebase.take() {
            Some(None) => !no_rebase,
            Some(Some(v)) if v == "true" => !no_rebase,
            Some(Some(v)) if v == "false" => false,
            other => {
                self.rebase = other;
                false
            }
        }
    }

    /// What was asked of a pull in progress; clears it, so that only the remaining options decide whether the command
    /// is bare.
    pub fn take_control(&mut self) -> Option<crate::local_merge::Control> {
        use crate::local_merge::Control;
        let (abort, cont, skip) = (
            std::mem::take(&mut self.abort),
            std::mem::take(&mut self.continue_pull),
            std::mem::take(&mut self.skip),
        );
        [
            (abort, Control::Abort),
            (cont, Control::Continue),
            (skip, Control::Skip),
        ]
        .into_iter()
        .find_map(|(on, c)| on.then_some(c))
    }

    fn refuse_unsupported(&self) {
        let given = [
            (
                "--abort",
                self.abort,
                "a git merge is given up with `pyn merge --abort`, a rebase with `pyn rebase --abort`",
            ),
            (
                "--continue",
                self.continue_pull,
                "a git merge is concluded with `pyn commit`, a rebase with `pyn rebase --continue`",
            ),
            (
                "--skip",
                self.skip,
                "a git rebase is skipped with `pyn rebase --skip`",
            ),
            ("--squash", self.squash, "a squash merge is not built"),
            ("--signoff", self.signoff, "trailers are not added"),
            (
                "--cleanup",
                self.cleanup.is_some(),
                "the message is always cleaned up",
            ),
            (
                "--allow-unrelated-histories",
                self.allow_unrelated_histories,
                "unrelated histories are refused",
            ),
            (
                "--all",
                self.all,
                "pull one remote; run `pyn fetch --all` to fetch every remote",
            ),
            (
                "--edit",
                self.edit,
                "no editor is started; give the message with `pyn merge -m` or finish with `pyn commit`",
            ),
        ];
        if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
            crate::usage_error(&format!("`pyn pull {flag}`: {why}"));
        }
    }

    fn fetch_args(&self) -> Result<FetchArgs> {
        let mut argv: Vec<String> = vec!["fetch".into()];
        let flags = [
            (self.append, "--append"),
            (self.prune, "--prune"),
            (self.no_prune, "--no-prune"),
            (self.tags, "--tags"),
            (self.no_tags, "--no-tags"),
            (self.force, "--force"),
            (self.dry_run, "--dry-run"),
            (self.progress, "--progress"),
            (self.no_progress, "--no-progress"),
        ];
        argv.extend(flags.iter().filter(|(on, _)| *on).map(|(_, f)| (*f).into()));
        argv.extend((0..self.verbose).map(|_| "-v".to_owned()));
        argv.extend((0..self.quiet).map(|_| "-q".to_owned()));
        if self.repository.is_some() || !self.refspecs.is_empty() {
            argv.push("--".into());
        }
        argv.extend(self.repository.clone());
        argv.extend(self.refspecs.iter().cloned());
        AsFetch::try_parse_from(argv)
            .map(|f| f.args)
            .map_err(|e| anyhow!("{e}"))
    }
}

/// A fetched ref that is a merge candidate, from a `FETCH_HEAD` line.
struct Candidate {
    id: ObjectId,
    kind: String,
    what: Option<String>,
    url: String,
}

fn candidates(git: &GitHome) -> Result<Vec<Candidate>> {
    let path = git.repo.git().git_dir().join("FETCH_HEAD");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let mut f = line.splitn(3, '\t');
        let (Some(id), Some(marker), Some(note)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if !marker.is_empty() {
            continue;
        }
        let id = ObjectId::from_hex(git.algorithm(), id)?;
        let (kind, rest) = ["branch ", "tag ", "remote-tracking branch "]
            .iter()
            .find_map(|k| note.strip_prefix(k).map(|r| (k.trim(), r)))
            .unwrap_or(("", note));
        let (what, url) = match rest.strip_prefix('\'').and_then(|r| r.split_once("' of ")) {
            Some((what, url)) => (Some(what.to_owned()), url.to_owned()),
            None => (None, rest.to_owned()),
        };
        out.push(Candidate {
            id,
            kind: kind.to_owned(),
            what,
            url,
        });
    }
    Ok(out)
}

impl Candidate {
    /// What a merge message calls this: `branch 'x' of <url>`, or `<url>` for a bare `HEAD`.
    fn describe(&self) -> String {
        let of = if self.url == "." {
            String::new()
        } else {
            format!(" of {}", self.url)
        };
        match &self.what {
            Some(what) if !self.kind.is_empty() => format!("{} '{what}'{of}", self.kind),
            Some(what) => format!("commit '{what}'{of}"),
            None => self.url.clone(),
        }
    }
}

enum Rebase {
    Off,
    On(PullMode),
    Unset,
}

impl Rebase {
    fn on(&self) -> bool {
        matches!(self, Rebase::On(_))
    }
}

/// A `--rebase` or `pull.rebase` value, or the text of git's complaint.
fn parse_rebase(v: &str) -> Result<Rebase, Option<&'static str>> {
    Ok(match v {
        "merges" | "m" => Rebase::On(PullMode::Merges),
        "interactive" | "i" => Rebase::On(PullMode::Interactive),
        "preserve" | "p" => return Err(Some("error: {}: 'preserve' superseded by 'merges'")),
        _ => match v.to_ascii_lowercase().as_str() {
            "false" | "no" | "off" | "0" | "" => Rebase::Off,
            "true" | "yes" | "on" | "1" => Rebase::On(PullMode::Plain),
            _ => return Err(None),
        },
    })
}

fn rebase_setting(git: &GitHome, args: &PullArgs, branch: Option<&str>) -> Result<Rebase> {
    if args.no_rebase {
        return Ok(Rebase::Off);
    }
    if let Some(given) = &args.rebase {
        let Some(v) = given else {
            return Ok(Rebase::On(PullMode::Plain));
        };
        return Ok(parse_rebase(v).unwrap_or_else(|why| {
            if let Some(line) = why {
                eprintln!("{}", line.replace("{}", v));
            }
            crate::usage_error(&format!("invalid value for '--rebase': '{v}'"))
        }));
    }
    let config = git.config();
    let keys = branch
        .map(|b| format!("branch.{b}.rebase"))
        .into_iter()
        .chain(["pull.rebase".to_owned()]);
    for key in keys {
        if let Some(v) = config.get(&key) {
            return Ok(parse_rebase(v).unwrap_or_else(|why| {
                if let Some(line) = why {
                    eprintln!("{}", line.replace("{}", v));
                }
                die(&format!("invalid value for '{key}': '{v}'"))
            }));
        }
    }
    Ok(Rebase::Unset)
}

/// The ref that `merge-base --fork-point` looks at, as `git pull` picks it: the tracking ref of the first refspec's
/// source, else the branch's upstream when the remote is its own.
fn fork_ref(git: &GitHome, args: &PullArgs, branch: Option<&str>) -> Option<String> {
    let config = git.config();
    let t = tracking(config, branch?);
    let remote = args.repository.clone().or_else(|| t.remote.clone())?;
    let from_specs =
        |src: &str| Remote::read(config, &remote).and_then(|r| map_ref(&r.fetch_specs(), src));
    match args.refspecs.first() {
        Some(spec) => {
            let src = spec.trim_start_matches('+').split(':').next()?;
            let src = src.strip_prefix("heads/").unwrap_or(src);
            let full = if src.starts_with("refs/") {
                src.to_owned()
            } else {
                format!("refs/heads/{src}")
            };
            from_specs(&full)
        }
        None if t.remote.as_deref() != Some(remote.as_str()) => None,
        None if remote == "." => t.merge.first().cloned(),
        None => from_specs(t.merge.first()?),
    }
}

/// How the histories are joined, from the command line, `pull.ff` and `merge.ff`.
struct Integration {
    no_ff: bool,
    ff_only: bool,
    /// Whether the command line or `pull.ff` said anything.
    stated: bool,
}

fn integration(git: &GitHome, args: &PullArgs) -> Result<Integration> {
    let by = |v: &str| match v.to_ascii_lowercase().as_str() {
        "only" => (false, true),
        "false" | "no" | "off" | "0" => (true, false),
        _ => (false, false),
    };
    let (no_ff, ff_only, stated) = if args.no_ff {
        (true, false, true)
    } else if args.ff_only {
        (false, true, true)
    } else if args.ff {
        (false, false, true)
    } else if let Some(v) = git.config().get("pull.ff") {
        let (n, o) = by(v);
        (n, o, true)
    } else if let Some(v) = git.config().get("merge.ff") {
        let (n, o) = by(v);
        (n, o, false)
    } else {
        (false, false, false)
    };
    Ok(Integration {
        no_ff,
        ff_only,
        stated,
    })
}

fn die(message: &str) -> ! {
    eprintln!("fatal: {message}");
    std::process::exit(128);
}

/// Git's explanation for a fetch that left nothing to merge, exit status 1.
fn no_candidates(git: &GitHome, args: &PullArgs, branch: Option<&str>) -> ! {
    let config = git.config();
    let t = branch.map(|b| tracking(config, b));
    let repo = args.repository.as_deref();
    if !args.refspecs.is_empty() {
        eprintln!("There are no candidates for merging among the refs that you just fetched.");
        eprintln!(
            "Generally this means that you provided a wildcard refspec which had no matches on the remote end."
        );
    } else if let (Some(repo), Some(t)) = (repo, &t)
        && t.remote.as_deref() != Some(repo)
    {
        eprintln!(
            "You asked to pull from the remote '{repo}', but did not specify\na branch. Because this is not the default configured remote\nfor your current branch, you must specify a branch on the command line."
        );
    } else if branch.is_none() {
        eprintln!("You are not currently on a branch.");
        eprintln!("Please specify which branch you want to merge with.");
        eprintln!("See pyn pull --help for details.\n\n    pyn pull <remote> <branch>\n");
    } else if t.as_ref().is_some_and(|t| t.merge.is_empty()) {
        let names = remotes_in_order(git);
        let remote = match names.as_slice() {
            [only] => only.as_str(),
            _ => "<remote>",
        };
        eprintln!("There is no tracking information for the current branch.");
        eprintln!("Please specify which branch you want to merge with.");
        eprintln!("See pyn pull --help for details.\n\n    pyn pull <remote> <branch>\n");
        eprintln!(
            "If you wish to set tracking information for this branch you can do so with:\n\n    pyn branch --set-upstream-to={remote}/<branch> {}\n",
            branch.unwrap_or_default()
        );
    } else if let Some(t) = &t {
        eprintln!(
            "Your configuration specifies to merge with the ref '{}'\nfrom the remote, but no such ref was fetched.",
            t.merge.first().map_or("", String::as_str)
        );
    }
    std::process::exit(1);
}

fn refuse_unfinished(git: &GitHome) -> Result<()> {
    if git.open_index()?.index.entries.iter().any(|e| e.stage > 0) {
        eprintln!("error: Pulling is not possible because you have unmerged files.");
        eprintln!("hint: Fix them up in the work tree, and then use 'pyn add <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
        die("Exiting because of an unresolved conflict.");
    }
    if git.repo.git().git_dir().join("MERGE_HEAD").exists() {
        eprintln!("hint: Please, commit your changes before you merge.");
        die("You have not concluded your merge (MERGE_HEAD exists).");
    }
    Ok(())
}

fn say_divergent() -> ! {
    eprintln!("hint: You have divergent branches and need to specify how to reconcile them.");
    eprintln!("hint: Run one of these on the command line, or set `pull.rebase` or `pull.ff` in");
    eprintln!("hint: the repository's configuration to make the choice for every pull:");
    eprintln!("hint:");
    eprintln!("hint:   pyn pull --no-rebase    # merge");
    eprintln!("hint:   pyn pull --ff-only      # fast-forward only");
    die("Need to specify how to reconcile divergent branches.");
}

pub fn pull(git: &GitHome, args: PullArgs) -> Result<()> {
    use super::submodule::{Fetching, fetch_submodules, follow_head};
    let flag = args.recurse_submodules.as_ref().map(Option::as_deref);
    let mode = Fetching::resolve(git, flag, args.no_recurse_submodules)?;
    let (quiet, dry_run) = (args.quiet > 0, args.dry_run);
    pull_inner(git, args)?;
    if dry_run {
        return Ok(());
    }
    let git = &git.reopen()?;
    fetch_submodules(git, mode, quiet)?;
    if mode == Fetching::Yes {
        follow_head(git, false)?;
    }
    Ok(())
}

fn pull_inner(git: &GitHome, args: PullArgs) -> Result<()> {
    args.refuse_unsupported();
    BLOCKED_STATUS.store(1, Ordering::Relaxed);
    let head = git.repo.head()?;
    let branch = match &head {
        Head::Branch { name, .. } => Some(name.as_str()),
        Head::Detached(_) => None,
    };
    let rebase = rebase_setting(git, &args, branch)?;
    let rebase_on = rebase.on();
    let rebase_unset = matches!(rebase, Rebase::Unset);
    if args.autostash && !rebase_on {
        crate::usage_error(
            "`pyn pull --autostash`: only a rebase stashes first; run `pyn stash`, or pull with --rebase",
        );
    }
    if rebase_on && (!args.strategy.is_empty() || !args.strategy_option.is_empty()) {
        crate::usage_error("`pyn pull --rebase -s` and `-X`: rebase strategies are not built");
    }
    if !rebase_on {
        super::strategy::Strategy::parse(&args.strategy, &args.strategy_option);
    }
    let how = integration(git, &args)?;
    if !args.dry_run {
        refuse_unfinished(git)?;
        if rebase_on {
            super::rebase::require_clean_for_pull(git, args.autostash)?;
        }
    }

    let typed: Vec<String> = std::env::args()
        .skip_while(|a| a != "pull")
        .skip(1)
        .collect();
    let action = std::iter::once("pull".to_owned())
        .chain(typed)
        .collect::<Vec<_>>()
        .join(" ");
    match super::fetch::run(git, &args.fetch_args()?, &action) {
        Ok(false) => {}
        Ok(true) => std::process::exit(1),
        Err(e) => {
            eprintln!("fatal: {}", crate::sanitize::text(&format!("{e:#}")));
            std::process::exit(1);
        }
    }
    if args.dry_run {
        return Ok(());
    }

    let git = &git.reopen()?;
    let found = candidates(git)?;
    if found.is_empty() {
        no_candidates(git, &args, branch);
    }
    let Some(ours) = git.repo.head_commit()? else {
        if found.len() > 1 {
            die("Cannot merge multiple branches into empty head.");
        }
        let theirs = peel(git, &found[0])?;
        QUIET.store(true, Ordering::Relaxed);
        return initial_checkout(git, &theirs, "pull: initial pull");
    };
    if found.len() > 1 {
        die("octopus merges are not built; name one branch to pull");
    }
    let candidate = &found[0];
    let theirs = peel(git, candidate)?;
    if let Rebase::On(mode) = rebase {
        let interactive = mode == PullMode::Interactive;
        if !interactive && git.is_ancestor(&theirs, &ours)? {
            if args.quiet == 0 {
                match branch {
                    Some(b) => println!("Current branch {b} is up to date."),
                    None => println!("HEAD is up to date."),
                }
            }
            return Ok(());
        }
        if !git.is_ancestor(&ours, &theirs)? {
            let fork = match fork_ref(git, &args, branch) {
                Some(name) => super::rebase::fork_point(git, &name, &ours)?,
                None => None,
            };
            return super::rebase::pull_rebase(
                git,
                &theirs,
                fork,
                mode,
                action,
                super::rebase::PullOpts {
                    autostash: args.autostash.then_some(true),
                    quiet: args.quiet > 0,
                    sign: args.sign.clone(),
                },
            );
        }
    }
    if rebase_unset
        && !how.stated
        && !git.is_ancestor(&theirs, &ours)?
        && !git.is_ancestor(&ours, &theirs)?
    {
        say_divergent();
    }
    if args.quiet > 0 {
        QUIET.store(true, Ordering::Relaxed);
    }
    let message = default_message(git, &candidate.describe())?;
    let _ = merge::ACTION.set(action);
    merge::merge(
        git,
        MergeArgs {
            source: Some(theirs.to_hex()),
            message: vec![message],
            no_ff: how.no_ff && !rebase_on,
            ff_only: how.ff_only || rebase_on,
            no_commit: args.no_commit,
            abort: false,
            continue_merge: false,
            preview: false,
            stat: args.stat,
            no_stat: args.no_stat,
            strategy: args.strategy,
            strategy_option: args.strategy_option,
            no_verify: false,
            verify: false,
            sign: args.sign,
        },
    )
}

fn peel(git: &GitHome, c: &Candidate) -> Result<ObjectId> {
    match commit_of(git, &c.id) {
        Some(id) => Ok(id),
        None => bail!("{} is not a commit and cannot be merged", c.id),
    }
}

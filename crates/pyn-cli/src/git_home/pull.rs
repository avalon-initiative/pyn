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
    /// Take the merge message without opening the editor.
    #[arg(long, overrides_with = "edit")]
    no_edit: bool,
    #[arg(long, overrides_with = "no_edit")]
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
    /// Merge the result into the index and work tree without a commit.
    #[arg(long, overrides_with = "no_squash")]
    squash: bool,
    #[arg(long, overrides_with = "squash")]
    no_squash: bool,
    /// The merge strategy: ort, recursive, resolve, octopus, ours or subtree.
    #[arg(short = 's', long, value_name = "STRATEGY")]
    strategy: Vec<String>,
    /// Accepted; git keeps the strategies named before it.
    #[arg(long)]
    no_strategy: bool,
    /// An option for the merge strategy, such as `ours` or `theirs`.
    #[arg(
        short = 'X',
        long,
        value_name = "OPTION",
        overrides_with = "no_strategy_option"
    )]
    strategy_option: Vec<String>,
    #[arg(long, overrides_with = "strategy_option")]
    no_strategy_option: bool,
    /// Stash local changes before the merge or rebase and apply them after.
    #[arg(long, overrides_with = "no_autostash")]
    autostash: bool,
    #[arg(long, overrides_with = "autostash")]
    no_autostash: bool,
    #[command(flatten)]
    sign: SignFlag,
    /// Add a `Signed-off-by` trailer to the merge commit.
    #[arg(long, overrides_with = "no_signoff")]
    signoff: bool,
    #[arg(long, overrides_with = "signoff")]
    no_signoff: bool,
    /// Add at most N shortlog entries of the merged commits to the message.
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "", overrides_with = "no_log")]
    log: Option<String>,
    #[arg(long, overrides_with = "log")]
    no_log: bool,
    /// How the message is cleaned up: strip, whitespace, verbatim, scissors or default.
    #[arg(long, value_name = "MODE", overrides_with = "no_cleanup")]
    cleanup: Option<String>,
    #[arg(long, overrides_with = "cleanup")]
    no_cleanup: bool,
    /// Require a valid signature on the merged commit.
    #[arg(long, overrides_with = "no_verify_signatures")]
    verify_signatures: bool,
    #[arg(long, overrides_with = "verify_signatures")]
    no_verify_signatures: bool,
    /// Skip the `pre-merge-commit` and `commit-msg` hooks of the merge.
    #[arg(long, overrides_with = "verify")]
    no_verify: bool,
    #[arg(long, overrides_with = "no_verify")]
    verify: bool,
    #[arg(long, overrides_with = "no_stat")]
    stat: bool,
    #[arg(short = 'n', long, overrides_with = "stat")]
    no_stat: bool,
    /// Fetch in the checked-out submodules (`yes`, `on-demand` or `no`), and with `yes` update them to the merged result.
    #[arg(long, num_args = 0..=1, require_equals = true, value_name = "MODE", overrides_with = "no_recurse_submodules")]
    recurse_submodules: Option<Option<String>>,
    #[arg(long, overrides_with = "recurse_submodules")]
    no_recurse_submodules: bool,
    /// Merge histories that share no commit.
    #[arg(long, overrides_with = "no_allow_unrelated_histories")]
    allow_unrelated_histories: bool,
    #[arg(long, overrides_with = "allow_unrelated_histories")]
    no_allow_unrelated_histories: bool,
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

    fn autostash_flag(&self) -> Option<bool> {
        match (self.autostash, self.no_autostash) {
            (true, _) => Some(true),
            (_, true) => Some(false),
            _ => None,
        }
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
            (
                "--all",
                self.all,
                "pull one remote; run `pyn fetch --all` to fetch every remote",
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

    /// The shortlog heading: `'x' of <url>` for a branch, `tag 'x' of <url>` for a tag, the bare `<url>` for `HEAD`.
    fn log_label(&self) -> Option<String> {
        let origin = match (self.kind.as_str(), &self.what) {
            ("branch" | "remote-tracking branch", Some(what)) => format!("'{what}'"),
            ("tag", Some(what)) => format!("tag '{what}'"),
            (_, None) => return Some(self.url.clone()),
            _ => return None,
        };
        Some(match self.url.as_str() {
            "." => origin
                .strip_prefix('\'')
                .and_then(|o| o.strip_suffix('\''))
                .map_or(origin.clone(), str::to_owned),
            url => format!("{origin} of {url}"),
        })
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
    if let Some(mode) = args.cleanup.as_deref()
        && !matches!(
            mode,
            "default" | "strip" | "whitespace" | "verbatim" | "scissors"
        )
    {
        die(&format!("Invalid cleanup mode {mode}"));
    }
    BLOCKED_STATUS.store(1, Ordering::Relaxed);
    let head = git.repo.head()?;
    let branch = match &head {
        Head::Branch { name, .. } => Some(name.as_str()),
        Head::Detached(_) => None,
    };
    let rebase = rebase_setting(git, &args, branch)?;
    let rebase_on = rebase.on();
    let rebase_unset = matches!(rebase, Rebase::Unset);
    let replay = if rebase_on {
        super::strategy::Replay::parse(
            "pull --rebase",
            args.strategy.last().map(String::as_str),
            &args.strategy_option,
        )
    } else {
        super::strategy::Strategy::parse_all(&args.strategy, &args.strategy_option);
        Default::default()
    };
    let how = integration(git, &args)?;
    if !args.dry_run {
        refuse_unfinished(git)?;
        if rebase_on {
            super::rebase::require_clean_for_pull(git, args.autostash_flag())?;
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
                    autostash: args.autostash_flag(),
                    quiet: args.quiet > 0,
                    sign: args.sign.clone(),
                    replay: replay.clone(),
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
    let mut message = default_message(git, &candidate.describe())?;
    if candidate.kind == "tag"
        && let Some(tag) = merge::tag_object_message(git, &candidate.id)
    {
        message = format!("{message}\n\n{tag}");
    }
    let _ = merge::ACTION.set(action);
    let cfg = if rebase_on {
        MergeArgs::default()
    } else {
        super::merge_options::configured_merge_args(git)
    };
    let both = |on: bool, off: bool, set: (bool, bool)| if on || off { (on, off) } else { set };
    let (ff, no_ff, ff_only) = if !how.stated && !rebase_on && (cfg.ff || cfg.no_ff || cfg.ff_only)
    {
        (cfg.ff, cfg.no_ff, cfg.ff_only)
    } else {
        (
            !how.no_ff && !how.ff_only && !rebase_on,
            how.no_ff && !rebase_on,
            how.ff_only || rebase_on,
        )
    };
    let (commit, no_commit) = both(args.commit, args.no_commit, (cfg.commit, cfg.no_commit));
    let (squash, no_squash) = both(args.squash, args.no_squash, (cfg.squash, cfg.no_squash));
    let (edit, no_edit) = both(args.edit, args.no_edit, (cfg.edit, cfg.no_edit));
    let (signoff, no_signoff) = both(args.signoff, args.no_signoff, (cfg.signoff, cfg.no_signoff));
    let (stat, no_stat) = both(args.stat, args.no_stat, (cfg.stat, cfg.no_stat));
    let (autostash, no_autostash) = both(
        args.autostash,
        args.no_autostash,
        (cfg.autostash, cfg.no_autostash),
    );
    let (verify_signatures, no_verify_signatures) = both(
        args.verify_signatures,
        args.no_verify_signatures,
        (cfg.verify_signatures, cfg.no_verify_signatures),
    );
    let (verify, no_verify) = both(args.verify, args.no_verify, (cfg.verify, cfg.no_verify));
    let (allow_unrelated_histories, no_allow_unrelated_histories) = both(
        args.allow_unrelated_histories,
        args.no_allow_unrelated_histories,
        (
            cfg.allow_unrelated_histories,
            cfg.no_allow_unrelated_histories,
        ),
    );
    let (log, no_log) = if args.log.is_some() || args.no_log {
        (args.log, args.no_log)
    } else {
        (cfg.log.clone(), cfg.no_log)
    };
    // git's pull keeps only the mode of `--cleanup`; `--no-cleanup` is never passed to the merge
    let (cleanup, no_cleanup) = if args.cleanup.is_some() {
        (args.cleanup, false)
    } else {
        (cfg.cleanup.clone(), cfg.no_cleanup)
    };
    let sign = if args.sign.gpg_sign.is_some() || args.sign.no_gpg_sign {
        args.sign
    } else {
        cfg.sign.clone()
    };
    let strategy: Vec<String> = cfg.strategy.iter().cloned().chain(args.strategy).collect();
    let reset_options = std::env::args().any(|a| a == "--no-strategy-option");
    let strategy_option: Vec<String> = if reset_options {
        args.strategy_option
    } else {
        cfg.strategy_option
            .iter()
            .cloned()
            .chain(args.strategy_option)
            .collect()
    };
    let own_message = !cfg.message.is_empty() || !cfg.file.is_empty() || cfg.message_emptied;
    merge::merge(
        git,
        MergeArgs {
            source: Some(theirs.to_hex()),
            message: if own_message {
                cfg.message.clone()
            } else {
                vec![message]
            },
            generated_message: !own_message,
            ff,
            no_ff,
            ff_only,
            commit,
            no_commit,
            squash,
            no_squash,
            signoff,
            no_signoff,
            log,
            no_log,
            cleanup,
            no_cleanup,
            verify_signatures,
            no_verify_signatures,
            no_verify,
            verify,
            allow_unrelated_histories,
            no_allow_unrelated_histories,
            autostash,
            no_autostash,
            stat,
            no_stat,
            strategy,
            strategy_option,
            edit,
            no_edit,
            log_label: candidate.log_label(),
            sign,
            ..cfg
        },
    )
}

fn peel(git: &GitHome, c: &Candidate) -> Result<ObjectId> {
    match commit_of(git, &c.id) {
        Some(id) => Ok(id),
        None => bail!("{} is not a commit and cannot be merged", c.id),
    }
}

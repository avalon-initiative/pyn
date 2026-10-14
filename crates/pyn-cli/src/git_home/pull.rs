//! `pyn pull` on a git repository: `pyn fetch`, then the fetched merge candidate through `pyn merge`.

use std::sync::atomic::Ordering;

use anyhow::{Result, anyhow, bail};
use clap::{ArgAction, Args, Parser};
use pyn_git::tracking;
use pyn_gitwork::Head;
use pyn_object::ObjectId;

use super::fetch::{FetchArgs, remotes_in_order};
use super::fetch_apply::commit_of;
use super::merge::{BLOCKED_STATUS, QUIET, default_message, initial_checkout};
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
    #[arg(long, hide = true)]
    squash: bool,
    #[arg(short = 's', long, hide = true, value_name = "STRATEGY")]
    strategy: Option<String>,
    #[arg(short = 'X', long, hide = true, value_name = "OPTION")]
    strategy_option: Option<String>,
    #[arg(long, hide = true)]
    autostash: bool,
    #[arg(short = 'S', long, hide = true, num_args = 0..=1, require_equals = true)]
    gpg_sign: Option<Option<String>>,
    #[arg(long, hide = true)]
    signoff: bool,
    #[arg(long, hide = true, value_name = "MODE")]
    cleanup: Option<String>,
    #[arg(long, hide = true)]
    stat: bool,
    #[arg(short = 'n', long, hide = true)]
    no_stat: bool,
    #[arg(long, hide = true, num_args = 0..=1, value_name = "MODE")]
    recurse_submodules: Option<Option<String>>,
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
    fn refuse_unsupported(&self) {
        let given = [
            ("--squash", self.squash, "a squash merge is not built"),
            (
                "--strategy",
                self.strategy.is_some(),
                "merge strategies are not built",
            ),
            (
                "--strategy-option",
                self.strategy_option.is_some(),
                "merge strategies are not built",
            ),
            (
                "--autostash",
                self.autostash,
                "stash the changes with `pyn stash`",
            ),
            (
                "--gpg-sign",
                self.gpg_sign.is_some(),
                "signing is not built",
            ),
            ("--signoff", self.signoff, "trailers are not added"),
            (
                "--cleanup",
                self.cleanup.is_some(),
                "the message is always cleaned up",
            ),
            ("--stat", self.stat, "the merge prints a file count only"),
            (
                "--no-stat",
                self.no_stat,
                "the merge prints a file count only",
            ),
            (
                "--recurse-submodules",
                self.recurse_submodules
                    .as_ref()
                    .is_some_and(|m| m.as_deref().is_none_or(|v| v != "no")),
                "submodules are not supported",
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
    On,
    Unset,
}

fn rebase_setting(git: &GitHome, args: &PullArgs, branch: Option<&str>) -> Result<Rebase> {
    if args.no_rebase {
        return Ok(Rebase::Off);
    }
    let from = |v: &str| match v.to_ascii_lowercase().as_str() {
        "false" | "no" | "off" | "0" | "" => Rebase::Off,
        _ => Rebase::On,
    };
    if let Some(given) = &args.rebase {
        return Ok(given.as_deref().map_or(Rebase::On, from));
    }
    let config = git.config();
    let keys = branch
        .map(|b| format!("branch.{b}.rebase"))
        .into_iter()
        .chain(["pull.rebase".to_owned()]);
    for key in keys {
        if let Some(v) = config.get(&key) {
            return Ok(from(v));
        }
    }
    Ok(Rebase::Unset)
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
    args.refuse_unsupported();
    BLOCKED_STATUS.store(1, Ordering::Relaxed);
    let head = git.repo.head()?;
    let branch = match &head {
        Head::Branch { name, .. } => Some(name.as_str()),
        Head::Detached(_) => None,
    };
    if matches!(rebase_setting(git, &args, branch)?, Rebase::On) {
        die("rebase is not built; use --no-rebase");
    }
    let rebase_unset = matches!(rebase_setting(git, &args, branch)?, Rebase::Unset);
    let how = integration(git, &args)?;
    if !args.dry_run {
        refuse_unfinished(git)?;
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
            eprintln!("fatal: {e:#}");
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
    merge::merge(
        git,
        MergeArgs {
            source: Some(theirs.to_hex()),
            message: vec![message],
            no_ff: how.no_ff,
            ff_only: how.ff_only,
            no_commit: args.no_commit,
            abort: false,
            continue_merge: false,
            preview: false,
        },
    )
}

fn peel(git: &GitHome, c: &Candidate) -> Result<ObjectId> {
    match commit_of(git, &c.id) {
        Some(id) => Ok(id),
        None => bail!("{} is not a commit and cannot be merged", c.id),
    }
}

//! `pyn ls-remote`: the references of a git remote, in git's output format.

use std::io::Write;

use anyhow::{Result, bail};
use clap::Args;
use globset::Glob;
use pyn_git::Remote;
use pyn_gittransport::exchange::list_remote_refs;
use pyn_gittransport::{Connector, Env, user_config};
use pyn_gitwire::LsRefsOptions;

use crate::git_home::GitHome;

/// List references in a remote repository.
#[derive(Debug, Args)]
pub struct LsRemoteArgs {
    /// Show only branches.
    #[arg(long, visible_alias = "branches")]
    heads: bool,
    /// Show only tags.
    #[arg(short, long)]
    tags: bool,
    /// Leave out `HEAD` and peeled tags (`<tag>^{}`).
    #[arg(long)]
    refs: bool,
    /// Show the branch each symbolic reference points to.
    #[arg(long)]
    symref: bool,
    /// Exit with status 2 when no reference matches.
    #[arg(long)]
    exit_code: bool,
    /// A remote name or a URL; the default is `origin`.
    repository: Option<String>,
    /// Show only references whose name ends with one of these (`*` and `?` match).
    patterns: Vec<String>,
}

/// The URL a remote name stands for in `config`, else `name` itself when it is not a configured remote.
fn target(git: Option<&GitHome>, name: Option<&str>) -> Result<(String, bool)> {
    let configured = git.and_then(|g| Remote::read(g.config(), name.unwrap_or("origin")));
    match (configured, name) {
        (Some(r), _) => match r.urls.first() {
            Some(url) => Ok((url.clone(), name.is_none())),
            None => bail!("no URL configured for remote '{}'", r.name),
        },
        (None, Some(url)) => Ok((url.to_owned(), false)),
        (None, None) => bail!("no remote configured to list refs from"),
    }
}

fn matchers(patterns: &[String]) -> Result<Vec<globset::GlobMatcher>> {
    patterns
        .iter()
        .map(|p| Ok(Glob::new(&format!("*/{p}"))?.compile_matcher()))
        .collect()
}

pub fn run(args: &LsRemoteArgs, git: Option<&GitHome>) -> Result<()> {
    let env = Env::process();
    let config = match git {
        Some(g) => g.config().clone(),
        None => user_config(&env)?,
    };
    let (url, default_remote) = target(git, args.repository.as_deref())?;
    let patterns = matchers(&args.patterns)?;
    let conn = Connector::new(config, env).connect(&url, false)?;
    if default_remote {
        eprintln!("From {}", conn.display);
    }
    let mut prefixes = Vec::new();
    if args.heads {
        prefixes.push("refs/heads/".to_owned());
    }
    if args.tags {
        prefixes.push("refs/tags/".to_owned());
    }
    let opts = LsRefsOptions {
        prefixes,
        symrefs: args.symref,
        peel: !args.refs,
        unborn: false,
    };
    let mut transport = conn.transport;
    let (_, refs) = list_remote_refs(&mut *transport, git.map(GitHome::algorithm), &opts)?;
    let mut out = String::new();
    let mut shown = 0;
    for r in refs
        .iter()
        .filter(|r| !args.refs || r.name.starts_with("refs/"))
    {
        let tail = format!("/{}", r.name);
        if !patterns.is_empty() && !patterns.iter().any(|m| m.is_match(&tail)) {
            continue;
        }
        let Some(id) = r.id else { continue };
        if let (true, Some(target)) = (args.symref, &r.symref_target) {
            out.push_str(&format!("ref: {target}\t{}\n", r.name));
        }
        out.push_str(&format!("{id}\t{}\n", r.name));
        if let (false, Some(peeled)) = (args.refs, r.peeled) {
            out.push_str(&format!("{peeled}\t{}^{{}}\n", r.name));
        }
        shown += 1;
    }
    let mut stdout = std::io::stdout().lock();
    if let Err(e) = stdout
        .write_all(out.as_bytes())
        .and_then(|()| stdout.flush())
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.into());
    }
    if args.exit_code && shown == 0 {
        std::process::exit(2);
    }
    Ok(())
}

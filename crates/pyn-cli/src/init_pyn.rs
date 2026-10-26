//! `pyn init`: starts a pyn server-home repository in a folder that is not tracked yet (#570, decision #562).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Args;

use crate::client::Api;
use crate::repo_tool::{self, RepoTool};
use crate::workspace::{Settings, Workspace, resolve};
use crate::{RepoCommand, address, credentials, git_home, http_transport, userconfig};

pub(crate) const POLICY: &str = "\
# Collaboration policy: which paths are shared (merged) and which are exclusive (locked).
[meta]
default = \"shared\"

# [exclusive]
# paths = [
#   \"Content/\",
#   \"**/*.uasset\",
# ]
";

#[derive(Debug, Args, PartialEq, Eq)]
pub struct InitArgs {
    /// The folder to start the repository in; created if missing. The current folder if omitted.
    directory: Option<PathBuf>,
    /// Create the repository named by `--repo` on the server without asking.
    #[arg(long)]
    create_on_server: bool,
    /// Never ask to create the repository on the server.
    #[arg(long)]
    no_create_on_server: bool,
}

/// The global options that say which server and repository to act on.
pub struct Remote {
    pub server: Option<String>,
    pub repo: Option<String>,
    pub token: Option<String>,
    pub user: Option<String>,
}

enum Create {
    Yes,
    No,
}

fn decide(args: &InitArgs, server: Option<&str>, repo: Option<&str>) -> Result<Create> {
    if args.create_on_server && args.no_create_on_server {
        crate::usage_error("`--create-on-server` and `--no-create-on-server` conflict");
    }
    if let Some(repo) = repo {
        address::split_repo(repo)?;
    }
    if args.create_on_server {
        return match (server, repo) {
            (Some(_), Some(_)) => Ok(Create::Yes),
            _ => crate::usage_error("`--create-on-server` needs a server and `--repo owner/name`"),
        };
    }
    match (server, repo) {
        (Some(server), Some(repo)) if !args.no_create_on_server && crate::interactive() => ask(
            &format!("Create {repo} on {server}? [y/N] "),
            &mut std::io::stdin().lock(),
            &mut std::io::stderr(),
        ),
        _ => Ok(Create::No),
    }
}

fn ask(question: &str, input: &mut impl BufRead, out: &mut impl Write) -> Result<Create> {
    write!(out, "{question}")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Create::Yes,
        _ => Create::No,
    })
}

pub fn init(args: &InitArgs, cwd: &Path, remote: Remote) -> Result<()> {
    let target = match &args.directory {
        Some(d) => cwd.join(d),
        None => cwd.to_path_buf(),
    };
    let yours = Settings::load(&userconfig::path()?).unwrap_or_default();
    let server = resolve(remote.server, None, yours.server.as_ref(), None);
    let create = decide(args, server.as_deref(), remote.repo.as_deref())?;
    if git_home::is_tracked(&target)? || target.join(".pyn").exists() {
        let hint = if target.ancestors().any(|p| p.join(".git").exists()) {
            "; `pyn convert` adopts a git repository"
        } else {
            ""
        };
        eprintln!(
            "fatal: {} is already a pyn workspace or git repository, or inside one{hint}",
            target.display()
        );
        std::process::exit(128);
    }
    std::fs::create_dir_all(&target)
        .with_context(|| format!("cannot mkdir {}", target.display()))?;
    let root = target.canonicalize()?;
    let ws = Workspace::create(&root)?;
    std::fs::write(ws.dir.join("pyn.toml"), POLICY)
        .with_context(|| format!("writing {}", ws.dir.join("pyn.toml").display()))?;
    repo_tool::set(&ws, RepoTool::Pyn)?;
    println!("Initialized empty pyn repository in {}", ws.dir.display());
    if let (Create::Yes, Some(server), Some(repo)) = (create, server, remote.repo) {
        create_on_server(
            &ws,
            &server,
            &repo,
            remote.token,
            remote.user.or(yours.user),
        )
        .context("the folder was initialized, but the repository was not created on the server")?;
    }
    Ok(())
}

fn create_on_server(
    ws: &Workspace,
    server: &str,
    repo: &str,
    token: Option<String>,
    user: Option<String>,
) -> Result<()> {
    let base = server.trim_end_matches('/').to_string();
    let api = Api {
        transport: Box::new(http_transport::HttpTransport::new(base.clone())),
        token: credentials::token_for(token, &base)?,
        base,
        user,
        repo: Some(repo.to_string()),
        branch: None,
        on_git: false,
    };
    crate::repo_command(
        &api,
        RepoCommand::Create {
            name: repo.to_string(),
            visibility: None,
            lease_hours: None,
            max_locks: None,
            policy: Some(ws.dir.join("pyn.toml")),
            no_policy: false,
        },
    )?;
    Settings {
        server: Some(server.to_string()),
        user: None,
        repo: Some(repo.to_string()),
        local_store: None,
    }
    .save(&ws.config_path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_answers_create() {
        for (text, yes) in [
            ("y\n", true),
            ("YES\n", true),
            ("\n", false),
            ("n\n", false),
            ("", false),
        ] {
            let got = ask("?", &mut text.as_bytes(), &mut Vec::new()).unwrap();
            assert_eq!(matches!(got, Create::Yes), yes, "{text:?}");
        }
    }
}

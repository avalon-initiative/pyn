//! `pyn remote` on a git repository: the remotes in `.git/config`, edited as git does, and the one server of a pyn home.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Subcommand};
use pyn_git::{
    ConfigFile, Error, Expect, Ref, Remote, add_remote, is_valid_remote_name, remote_names,
    remove_remote, rename_remote,
};

use super::remote_query as query;
use super::{GitHome, Refused};

/// List and edit remotes.
#[derive(Debug, Args)]
pub struct RemoteArgs {
    /// Show the URLs of each remote.
    #[arg(short, long)]
    verbose: bool,
    #[command(subcommand)]
    command: Option<RemoteCommand>,
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Define a remote with one URL and the default fetch refspec.
    Add {
        /// Fetch the remote after adding it.
        #[arg(short = 'f', long)]
        fetch: bool,
        /// Mirror the remote: `fetch` stores every ref, `push` makes `pyn push` send every ref.
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "", value_name = "fetch|push")]
        mirror: Option<String>,
        name: String,
        url: String,
    },
    /// Delete a remote, its remote-tracking branches and the tracking set up for it.
    #[command(visible_alias = "rm")]
    Remove { name: String },
    /// Rename a remote, its remote-tracking branches and its tracking configuration.
    Rename { old: String, new: String },
    /// Change, add or delete a remote's URLs.
    SetUrl {
        /// Work on the push URLs.
        #[arg(long)]
        push: bool,
        /// Add `<newurl>` as one more URL.
        #[arg(long, conflicts_with = "old")]
        add: bool,
        /// Delete the URLs that contain `<newurl>`.
        #[arg(long, conflicts_with = "old")]
        delete: bool,
        name: String,
        /// The new URL (with --delete, the URL to delete).
        url: String,
        /// Replace the URLs that contain this one.
        old: Option<String>,
    },
    /// Print a remote's URL.
    GetUrl {
        /// Print the push URLs.
        #[arg(long)]
        push: bool,
        /// Print every URL, not the first.
        #[arg(long)]
        all: bool,
        name: String,
    },
    /// Describe a remote: its HEAD, its branches against the local refs, and the pull and push setup.
    Show {
        /// Do not ask the remote; describe it from the stored refs.
        #[arg(short = 'n')]
        no_query: bool,
        names: Vec<String>,
    },
    /// Fetch the named remotes and groups, or the default set.
    Update {
        /// Delete remote-tracking refs the remote no longer has.
        #[arg(short, long, overrides_with = "no_prune")]
        prune: bool,
        #[arg(long, overrides_with = "prune")]
        no_prune: bool,
        names: Vec<String>,
    },
    /// Delete the remote-tracking refs whose branch the remote no longer has.
    Prune {
        /// Report what would be deleted and change nothing.
        #[arg(short = 'n', long)]
        dry_run: bool,
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Set, find or delete `refs/remotes/<name>/HEAD`.
    SetHead {
        /// Take the branch the remote's HEAD points at.
        #[arg(short, long)]
        auto: bool,
        /// Delete the ref.
        #[arg(short, long)]
        delete: bool,
        name: String,
        branch: Option<String>,
    },
    /// Replace (or with --add, extend) the branches a remote fetches.
    SetBranches {
        /// Keep the fetch refspecs already there.
        #[arg(long)]
        add: bool,
        name: String,
        #[arg(required = true)]
        branches: Vec<String>,
    },
}

/// Prints `message` on standard error and exits with git's status for it.
pub(super) fn fail(code: i32, message: String) -> ! {
    eprintln!("{message}");
    std::process::exit(code)
}

fn no_such(name: &str) -> ! {
    fail(2, format!("error: No such remote: '{name}'"))
}

/// `get-url` and `set-url` word the same refusal without the colon.
fn no_such_url(name: &str) -> ! {
    fail(2, format!("error: No such remote '{name}'"))
}

fn config_path(git: &GitHome) -> PathBuf {
    git.repo.git().common_dir().join("config")
}

fn edit<T>(git: &GitHome, change: impl FnOnce(&mut ConfigFile) -> pyn_git::Result<T>) -> Result<T> {
    let path = config_path(git);
    let mut file = ConfigFile::read(&path)?;
    let out = change(&mut file)?;
    file.write(&path)?;
    Ok(out)
}

fn existing(git: &GitHome, name: &str) -> Remote {
    Remote::read(git.repo.config(), name).unwrap_or_else(|| no_such(name))
}

fn existing_for_url(git: &GitHome, name: &str) -> Remote {
    Remote::read(git.repo.config(), name).unwrap_or_else(|| no_such_url(name))
}

fn list(git: &GitHome, verbose: bool) {
    let config = git.repo.config();
    for name in remote_names(config) {
        if !verbose {
            println!("{name}");
            continue;
        }
        let remote = Remote::read(config, &name).expect("a listed remote exists");
        let Some(first) = remote.urls.first() else {
            println!("{name}\t");
            continue;
        };
        println!("{name}\t{first} (fetch)");
        for url in remote.push_targets() {
            println!("{name}\t{url} (push)");
        }
    }
}

fn add(git: &GitHome, fetch: bool, mirror: Option<&str>, name: &str, url: &str) -> Result<()> {
    let (mirror_fetch, mirror_push) = match mirror {
        None => (false, false),
        Some("") => {
            eprintln!(
                "warning: --mirror is dangerous and deprecated; please\n\t use --mirror=fetch or --mirror=push instead"
            );
            (true, true)
        }
        Some("fetch") => (true, false),
        Some("push") => (false, true),
        Some(other) => fail(129, format!("error: unknown mirror argument: {other}")),
    };
    if !is_valid_remote_name(name) {
        fail(128, format!("fatal: '{name}' is not a valid remote name"));
    }
    if remote_names(git.repo.config()).iter().any(|n| n == name) {
        fail(3, format!("error: remote {name} already exists."));
    }
    edit(git, |file| {
        if mirror.is_none() {
            return add_remote(file, name, url);
        }
        file.add("remote", Some(name), "url", url)?;
        if mirror_fetch {
            file.add("remote", Some(name), "fetch", "+refs/*:refs/*")?;
        }
        if mirror_push {
            file.add("remote", Some(name), "mirror", "true")?;
        }
        Ok(())
    })?;
    if fetch {
        super::fetch::after_add(git, name)?;
    }
    Ok(())
}

/// Every ref under `refs/remotes/<remote>/`, including a symbolic `HEAD` whose target is gone.
fn remote_refs(git: &GitHome, remote: &str) -> Result<Vec<String>> {
    let refs = git.repo.refs();
    let prefix = format!("refs/remotes/{remote}/");
    let mut names: Vec<String> = refs.list(&prefix)?.into_iter().map(|(n, _)| n).collect();
    let head = format!("{prefix}HEAD");
    if !names.contains(&head) && refs.read(&head)?.is_some() {
        names.push(head);
    }
    Ok(names)
}

fn remove(git: &GitHome, name: &str) -> Result<()> {
    existing(git, name);
    let refs = git.repo.refs();
    for ref_name in remote_refs(git, name)? {
        refs.delete(&ref_name, Expect::Any)?;
        refs.write_reflog(&ref_name, &[])?;
    }
    edit(git, |file| {
        remove_remote(file, name);
        Ok(())
    })
}

fn message(old: &str, new: &str) -> String {
    format!("remote: renamed {old} to {new}")
}

fn move_refs(git: &GitHome, old: &str, new: &str) -> Result<()> {
    let refs = git.repo.refs();
    let (from, to) = (
        format!("refs/remotes/{old}/"),
        format!("refs/remotes/{new}/"),
    );
    let mut found = Vec::new();
    for name in remote_refs(git, old)? {
        found.push((refs.read(&name)?, refs.resolve(&name)?, name));
    }
    for (current, resolved, name) in found {
        let target = format!("{to}{}", &name[from.len()..]);
        match current {
            Some(Ref::Symbolic(t)) => {
                let t = t
                    .strip_prefix(&from)
                    .map_or(t.clone(), |r| format!("{to}{r}"));
                refs.set_symbolic(&target, &t)?;
                refs.write_reflog(&name, &[])?;
                if let Some(id) = resolved {
                    git.log_ref(&target, None, &id, &message(&name, &target), false)?;
                }
            }
            Some(Ref::Direct(id)) => {
                refs.update_checked(&target, Expect::Absent, &id)?;
                refs.rename_reflog(&name, &target)?;
                git.log_ref(&target, Some(&id), &id, &message(&name, &target), false)?;
            }
            None => continue,
        }
        refs.delete(&name, Expect::Any)?;
    }
    Ok(())
}

fn rename(git: &GitHome, old: &str, new: &str) -> Result<()> {
    existing(git, old);
    if !is_valid_remote_name(new) {
        fail(128, format!("fatal: '{new}' is not a valid remote name"));
    }
    if remote_names(git.repo.config()).iter().any(|n| n == new) {
        fail(3, format!("error: remote {new} already exists."));
    }
    let kept = edit(git, |file| rename_remote(file, old, new))?;
    for spec in kept {
        eprintln!(
            "warning: Not updating non-default fetch refspec\n\t{spec}\n\tPlease update the configuration manually if necessary."
        );
    }
    move_refs(git, old, new)
}

struct UrlEdit<'a> {
    name: &'a str,
    push: bool,
    add: bool,
    delete: bool,
    url: &'a str,
    old: Option<&'a str>,
}

fn set_url(git: &GitHome, e: UrlEdit) -> Result<()> {
    let remote = existing_for_url(git, e.name);
    if e.add && e.delete {
        fail(128, "fatal: --add --delete doesn't make sense".into());
    }
    let key = if e.push { "pushurl" } else { "url" };
    let full = format!("remote.{}.{key}", e.name);
    let name = Some(e.name);
    if (e.old.is_none() && !e.delete) || e.add {
        let done = edit(git, |file| match e.add {
            true => file.add("remote", name, key, e.url),
            false => file.set("remote", name, key, e.url),
        });
        return match done {
            Err(err) if matches!(err.downcast_ref::<Error>(), Some(Error::MultipleValues(_))) => {
                eprintln!("warning: {err}");
                fail(128, format!("fatal: could not set '{full}' to '{}'", e.url))
            }
            other => other,
        };
    }
    let pattern = e.old.unwrap_or(e.url);
    let urls = if e.push {
        &remote.push_urls
    } else {
        &remote.urls
    };
    let matches = urls.iter().filter(|u| u.contains(pattern)).count();
    if !e.delete && matches == 0 {
        fail(128, format!("fatal: No such URL found: {pattern}"));
    }
    if e.delete && matches == urls.len() && !e.push {
        fail(128, "fatal: Will not delete all non-push URLs".into());
    }
    let changed = edit(git, |file| {
        if e.delete {
            Ok(file.unset_where("remote", name, key, |v| v.contains(pattern)))
        } else {
            file.replace_where("remote", name, key, e.url, |v| v.contains(pattern))
        }
    })?;
    if e.delete && changed == 0 {
        fail(128, format!("fatal: could not unset '{full}'"));
    }
    Ok(())
}

fn get_url(git: &GitHome, name: &str, push: bool, all: bool) {
    let remote = existing_for_url(git, name);
    let urls = if push {
        remote.push_targets()
    } else {
        &remote.urls
    };
    match urls {
        [] => println!("{name}"),
        _ if all => urls.iter().for_each(|u| println!("{u}")),
        [first, ..] => println!("{first}"),
    }
}

fn set_branches(git: &GitHome, add: bool, name: &str, branches: &[String]) -> Result<()> {
    existing_for_url(git, name);
    edit(git, |file| {
        if !add {
            file.unset_all("remote", Some(name), "fetch");
        }
        for b in branches {
            let spec = format!("+refs/heads/{b}:refs/remotes/{name}/{b}");
            file.add("remote", Some(name), "fetch", &spec)?;
        }
        Ok(())
    })
}

pub fn remote(git: &GitHome, args: RemoteArgs) -> Result<()> {
    match args.command {
        None => {
            list(git, args.verbose);
            Ok(())
        }
        Some(RemoteCommand::Add {
            fetch,
            mirror,
            name,
            url,
        }) => add(git, fetch, mirror.as_deref(), &name, &url),
        Some(RemoteCommand::Remove { name }) => remove(git, &name),
        Some(RemoteCommand::Rename { old, new }) => rename(git, &old, &new),
        Some(RemoteCommand::SetUrl {
            push,
            add,
            delete,
            name,
            url,
            old,
        }) => set_url(
            git,
            UrlEdit {
                name: &name,
                push,
                add,
                delete,
                url: &url,
                old: old.as_deref(),
            },
        ),
        Some(RemoteCommand::GetUrl { push, all, name }) => {
            get_url(git, &name, push, all);
            Ok(())
        }
        Some(RemoteCommand::Show { no_query, names }) => query::show(git, no_query, &names),
        Some(RemoteCommand::Update {
            prune,
            no_prune,
            names,
        }) => query::update(
            git,
            prune.then_some(true).or(no_prune.then_some(false)),
            args.verbose,
            &names,
        ),
        Some(RemoteCommand::Prune { dry_run, names }) => query::prune(git, dry_run, &names),
        Some(RemoteCommand::SetHead {
            auto,
            delete,
            name,
            branch,
        }) => {
            let action = match (auto, delete, branch.as_deref()) {
                (true, false, None) => query::HeadAction::Auto,
                (false, true, None) => query::HeadAction::Delete,
                (false, false, Some(b)) => query::HeadAction::Set(b),
                _ => crate::usage_error(
                    "`pyn remote set-head` takes <name> and exactly one of <branch>, `-a` or `-d`",
                ),
            };
            query::set_head(git, &name, action)
        }
        Some(RemoteCommand::SetBranches {
            add,
            name,
            branches,
        }) => set_branches(git, add, &name, &branches),
    }
}

/// A pyn server home has the one remote, `origin`: the server and repository it is set to use.
pub fn on_server(args: &RemoteArgs, server: &str, repo: Option<&str>) -> Result<()> {
    let url = |repo: &str| format!("{}/{repo}", server.trim_end_matches('/'));
    match &args.command {
        Some(RemoteCommand::GetUrl { name, .. }) => {
            if name != "origin" {
                no_such_url(name);
            }
            println!("{}", url(repo.ok_or_else(no_repo)?));
            return Ok(());
        }
        Some(_) => {
            return Err(Refused(
                "a pyn server home has exactly one home, one server and one repository identity, so its remote cannot be added, removed, renamed or changed; set it with `pyn config set server <url>` and `pyn config set repo <owner/name>` (see the one-home rule in docs/concepts/git-compatibility.md)"
                    .into(),
            )
            .into());
        }
        None => {}
    }
    let repo = repo.ok_or_else(no_repo)?;
    if !args.verbose {
        println!("origin");
        return Ok(());
    }
    let url = url(repo);
    println!("origin\t{url} (fetch)\norigin\t{url} (push)");
    Ok(())
}

fn no_repo() -> Refused {
    Refused("no repository is set; set one with `pyn config set repo <owner/name>`".into())
}

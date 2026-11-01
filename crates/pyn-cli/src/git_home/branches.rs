//! `pyn branch` on a git repository: list, create, delete and rename branches as git does, with the reflog and
//! `branch.<name>.*` configuration that go with them.

use anyhow::{Result, bail};
use globset::Glob;
use pyn_git::{ConfigFile, Ref};
use pyn_gitwork::Head;
use pyn_object::ObjectId;

use super::GitHome;
use super::commands::{divergence, track_words, upstream_of};
use super::refwrite::BRANCHES;
use super::switch::subject;
use crate::branch::BranchArgs;

struct Row {
    marker: char,
    name: String,
    id: Option<ObjectId>,
    /// What a symbolic remote-tracking ref points at.
    arrow: Option<String>,
    /// The `[upstream: ahead 1]` part, and the work tree that has the branch, which `-v` and `-vv` show.
    tracking: String,
    tree: Option<std::path::PathBuf>,
}

const REMOTES: &str = "refs/remotes/";

enum Section<'a> {
    Drop,
    Rename(&'a str),
    Copy(&'a str),
}

/// Drops the `[branch "name"]` section from the repository configuration, or renames or copies it.
fn edit_config_section(git: &GitHome, name: &str, how: Section<'_>) -> Result<()> {
    let path = git.repo.git().common_dir().join("config");
    let mut file = ConfigFile::read(&path)?;
    let changed = match how {
        Section::Rename(to) => file.rename_section("branch", name, to)? > 0,
        Section::Copy(to) => file.copy_section("branch", name, to)? > 0,
        Section::Drop => file.remove_section("branch", Some(name)),
    };
    if changed {
        file.write(&path)?;
    }
    Ok(())
}

/// `[origin/main: ahead 1] ` for `-vv`, `[ahead 1] ` for `-v`, empty without an upstream.
fn tracking_text(git: &GitHome, name: &str, id: &ObjectId, named: bool) -> Result<String> {
    let Some(up) = upstream_of(git, name)? else {
        return Ok(String::new());
    };
    let words = match &up.id {
        None => Some("gone".to_string()),
        Some(up_id) => {
            let (ahead, behind) = divergence(git, id, up_id)?;
            track_words(ahead, behind)
        }
    };
    Ok(match (named, words) {
        (true, Some(w)) => format!("[{}: {w}] ", up.shown),
        (true, None) => format!("[{}] ", up.shown),
        (false, Some(w)) => format!("[{w}] "),
        (false, None) => String::new(),
    })
}

fn list(git: &GitHome, args: &BranchArgs) -> Result<()> {
    let head = git.repo.head()?;
    let patterns: Vec<_> = args
        .names
        .iter()
        .map(|p| Glob::new(p).map(|g| g.compile_matcher()))
        .collect::<std::result::Result<_, _>>()?;
    let wanted = |name: &str| patterns.is_empty() || patterns.iter().any(|p| p.is_match(name));
    let remotes_only = args.remotes && !args.all;
    let mut rows: Vec<Row> = Vec::new();
    if !remotes_only
        && patterns.is_empty()
        && let Head::Detached(id) = &head
    {
        rows.push(Row {
            marker: '*',
            name: format!("(HEAD detached at {})", git.repo.abbreviate(id)?),
            id: Some(*id),
            arrow: None,
            tracking: String::new(),
            tree: None,
        });
    }
    if !remotes_only {
        for (full, id) in git.repo.refs().list(BRANCHES)? {
            let name = full[BRANCHES.len()..].to_string();
            if !wanted(&name) {
                continue;
            }
            let elsewhere = match &head {
                Head::Branch { name: n, .. } if *n == name => None,
                _ => git.used_elsewhere(&name)?,
            };
            let marker = match &head {
                Head::Branch { name: n, .. } if *n == name => '*',
                _ if elsewhere.is_some() => '+',
                _ => ' ',
            };
            let tracking = if args.verbose > 0 {
                tracking_text(git, &name, &id, args.verbose > 1)?
            } else {
                String::new()
            };
            rows.push(Row {
                marker,
                name,
                id: Some(id),
                arrow: None,
                tracking,
                tree: elsewhere.filter(|_| args.verbose > 1),
            });
        }
    }
    if args.all || args.remotes {
        for (full, id) in git.repo.refs().list(REMOTES)? {
            let short_name = &full[REMOTES.len()..];
            if !wanted(short_name) {
                continue;
            }
            let arrow = match git.repo.refs().read(&full)? {
                Some(Ref::Symbolic(t)) => Some(t.strip_prefix(REMOTES).unwrap_or(&t).to_string()),
                _ => None,
            };
            let name = if args.all {
                format!("remotes/{short_name}")
            } else {
                short_name.to_string()
            };
            rows.push(Row {
                marker: ' ',
                name,
                id: Some(id),
                arrow,
                tracking: String::new(),
                tree: None,
            });
        }
    }
    let width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    for row in rows {
        if let Some(target) = &row.arrow {
            let pad = if args.verbose > 0 { width } else { 0 };
            println!("{} {:<pad$} -> {target}", row.marker, row.name);
        } else if args.verbose > 0
            && let Some(id) = &row.id
        {
            let tree = row
                .tree
                .as_ref()
                .map_or(String::new(), |t| format!("({}) ", t.display()));
            println!(
                "{} {:<width$} {} {tree}{}{}",
                row.marker,
                row.name,
                git.repo.abbreviate(id)?,
                row.tracking,
                subject(git, id)?
            );
        } else {
            println!("{} {}", row.marker, row.name);
        }
    }
    Ok(())
}

fn create(
    git: &GitHome,
    name: &str,
    start: Option<&str>,
    force: bool,
    want: super::track::Want,
    quiet: bool,
) -> Result<()> {
    GitHome::check_branch_name(name)?;
    let existing = git.branch_commit(name)?;
    match existing {
        Some(_) if !force => bail!("a branch named '{name}' already exists"),
        Some(_) => {
            if let Some(tree) = git.checked_out_path(name)? {
                bail!(
                    "cannot force update the branch '{name}' used by worktree at '{}'",
                    tree.display()
                );
            }
        }
        None => git.check_ref_room(&GitHome::branch_ref(name))?,
    }
    let id = match start {
        Some(s) => git
            .repo
            .resolve(s)
            .map_err(|_| anyhow::anyhow!("not a valid object name: '{s}'"))?,
        None => match git.repo.head_commit()? {
            Some(id) => id,
            None => {
                let on = match git.repo.head()? {
                    Head::Branch { name, .. } => name,
                    Head::Detached(_) => "HEAD".into(),
                };
                bail!("not a valid object name: '{on}'");
            }
        },
    };
    let label = match (start, git.repo.head()?) {
        (Some(s), _) => s.to_string(),
        (None, Head::Branch { name, .. }) => name,
        (None, Head::Detached(_)) => "HEAD".into(),
    };
    let plan = super::track::plan(git, want, start, name)?;
    let full = GitHome::branch_ref(name);
    match existing {
        Some(old) if old == id => {}
        Some(old) => git.reset_ref(&full, Some(&old), &id, &format!("branch: Reset to {label}"))?,
        None => git.create_ref(&full, &id, &format!("branch: Created from {label}"))?,
    }
    match plan {
        Some(p) => super::track::apply(git, name, &p, quiet),
        None => Ok(()),
    }
}

fn delete(git: &GitHome, names: &[String], force: bool, quiet: bool) -> Result<()> {
    if names.is_empty() {
        bail!("branch name required");
    }
    let head = git.repo.head_commit()?;
    let mut failed = false;
    for name in names {
        let full = GitHome::branch_ref(name);
        let Some(id) = git.branch_commit(name)? else {
            eprintln!("error: branch '{name}' not found.");
            failed = true;
            continue;
        };
        if let Some(tree) = git.checked_out_path(name)? {
            eprintln!(
                "error: cannot delete branch '{name}' used by worktree at '{}'",
                tree.display()
            );
            failed = true;
            continue;
        }
        if !force {
            let merged = match &head {
                Some(h) => git.is_ancestor(&id, h)?,
                None => false,
            };
            if !merged {
                eprintln!("error: the branch '{name}' is not fully merged.");
                eprintln!(
                    "hint: If you are sure you want to delete it, run 'pyn branch -D {name}'."
                );
                failed = true;
                continue;
            }
        }
        git.repo.refs().delete(&full, pyn_git::Expect::Id(id))?;
        git.repo.refs().write_reflog(&full, &[])?;
        edit_config_section(git, name, Section::Drop)?;
        if !quiet {
            println!("Deleted branch {name} (was {}).", git.repo.abbreviate(&id)?);
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// The two `HEAD` lines git writes when the branch `HEAD` is on is renamed: away to nothing, then back.
fn log_head_swap(git: &GitHome, id: &ObjectId, message: &str) -> Result<()> {
    git.log_ref("HEAD", Some(id), &git.zero_id(), message, false)?;
    git.log_ref("HEAD", Some(&git.zero_id()), id, message, false)
}

/// `-m`/`-M` (a rename) and `-c`/`-C` (a copy of the ref, its reflog and its `branch.<name>.*` settings).
fn relocate(git: &GitHome, names: &[String], copy: bool, force: bool) -> Result<()> {
    let head = git.repo.head()?;
    let (old, new) = match names {
        [new] => match &head {
            Head::Branch { name, .. } => (name.clone(), new.clone()),
            Head::Detached(_) if copy => bail!("cannot copy the current branch while not on any"),
            Head::Detached(_) => bail!("cannot rename the current branch while not on any"),
        },
        [old, new] => (old.clone(), new.clone()),
        [] => bail!("branch name required"),
        _ if copy => bail!("too many branches for a copy operation"),
        _ => bail!("too many arguments for a rename operation"),
    };
    let on_it = matches!(&head, Head::Branch { name, .. } if *name == old);
    let (old_ref, new_ref) = (GitHome::branch_ref(&old), GitHome::branch_ref(&new));
    let id = git.branch_commit(&old)?;
    if id.is_none() && (copy || !on_it) {
        if copy && on_it {
            bail!("no commit on branch '{old}' yet");
        }
        bail!("no branch named '{old}'");
    }
    if !copy && let Some(tree) = git.used_elsewhere(&old)? {
        bail!(
            "cannot rename branch '{old}' used by worktree at '{}'",
            tree.display()
        );
    }
    let refs = git.repo.refs();
    let message = format!(
        "Branch: {} {old_ref} to {new_ref}",
        if copy { "copied" } else { "renamed" }
    );
    if old == new {
        if let Some(id) = id {
            git.log_ref(&new_ref, Some(&id), &id, &message, false)?;
            if on_it && copy {
                git.log_ref("HEAD", Some(&id), &id, &message, false)?;
            } else if on_it {
                git.log_ref("HEAD", Some(&id), &git.zero_id(), &message, false)?;
                git.log_ref("HEAD", Some(&id), &id, &message, false)?;
            }
        }
        return Ok(());
    }
    GitHome::check_branch_name(&new)?;
    let existing = git.branch_commit(&new)?;
    match existing {
        Some(_) if !force => bail!("a branch named '{new}' already exists"),
        Some(_) => {
            if let Some(tree) = git.checked_out_path(&new)? {
                bail!(
                    "cannot force update the branch '{new}' used by worktree at '{}'",
                    tree.display()
                );
            }
        }
        None => git.check_ref_room(&new_ref)?,
    }
    let Some(id) = id else {
        refs.set_symbolic("HEAD", &new_ref)?;
        return edit_config_section(git, &old, Section::Rename(&new));
    };
    refs.update_checked(
        &new_ref,
        existing.map_or(pyn_git::Expect::Absent, pyn_git::Expect::Id),
        &id,
    )?;
    if copy {
        let entries = refs.read_reflog(&old_ref)?;
        refs.write_reflog(&new_ref, &entries)?;
        git.log_ref(&new_ref, Some(&id), &id, &message, false)?;
        return edit_config_section(git, &old, Section::Copy(&new));
    }
    refs.rename_reflog(&old_ref, &new_ref)?;
    refs.delete(&old_ref, pyn_git::Expect::Id(id))?;
    git.log_ref(&new_ref, Some(&id), &id, &message, false)?;
    if on_it {
        log_head_swap(git, &id, &message)?;
        refs.set_symbolic("HEAD", &new_ref)?;
    }
    edit_config_section(git, &old, Section::Rename(&new))
}

pub fn branch(git: &GitHome, args: BranchArgs) -> Result<()> {
    let listing = args.all || args.remotes;
    let delete = args.delete || args.force_delete;
    let rename = args.rename || args.force_rename;
    let copy = args.copy || args.force_copy;
    let upstream = args.set_upstream_to.is_some();
    let modes = [
        delete,
        rename,
        copy,
        args.list,
        args.show_current,
        upstream,
        args.unset_upstream,
    ];
    if modes.iter().filter(|m| **m).count() > 1 {
        crate::usage_error("choose one of --delete, --move, --copy, --list, --set-upstream-to");
    }
    if listing && (delete || rename || copy || upstream || args.unset_upstream) {
        if args.all && delete {
            bail!("cannot use -a with -d");
        }
        crate::usage_error("`pyn branch -a` and `-r` only list on a git repository");
    }
    if args.show_current {
        if let Head::Branch { name, .. } = git.repo.head()? {
            println!("{name}");
        }
        return Ok(());
    }
    if delete {
        return self::delete(
            git,
            &args.names,
            args.force_delete || args.force,
            args.quiet,
        );
    }
    if rename {
        return relocate(git, &args.names, false, args.force_rename || args.force);
    }
    if copy {
        return relocate(git, &args.names, true, args.force_copy || args.force);
    }
    if let Some(up) = &args.set_upstream_to {
        return super::upstream::set_upstream(git, up, &args.names, args.quiet);
    }
    if args.unset_upstream {
        return super::upstream::unset_upstream(git, &args.names);
    }
    if listing && !args.list && !args.names.is_empty() {
        bail!(
            "the -a, and -r, options to 'git branch' do not take a branch name.\nDid you mean to use: -a|-r --list <pattern>?"
        );
    }
    if args.list || listing || args.names.is_empty() {
        return list(git, &args);
    }
    let want = super::track::Want::of(&args.track);
    match args.names.as_slice() {
        [name] => create(git, name, None, args.force, want, args.quiet),
        [name, start] => create(git, name, Some(start), args.force, want, args.quiet),
        _ => crate::usage_error(
            "too many arguments to create a branch; the usage is `pyn branch <name> [<start>]`",
        ),
    }
}

//! `pyn branch` on a git repository: list, create, delete and rename branches as git does, with the reflog and
//! `branch.<name>.*` configuration that go with them.

use anyhow::{Result, bail};
use globset::Glob;
use pyn_gitwork::Head;
use pyn_object::ObjectId;

use super::GitHome;
use super::refwrite::BRANCHES;
use super::switch::subject;
use crate::branch::BranchArgs;

const ABBREV: usize = 7;

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

struct Row {
    marker: char,
    name: String,
    id: Option<ObjectId>,
}

fn rewrite_config(git: &GitHome, edit: impl Fn(&[&str]) -> Vec<String>) -> Result<()> {
    let path = git.repo.git().common_dir().join("config");
    let text = std::fs::read_to_string(&path)?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let out: String = edit(&lines).concat();
    if out == text {
        return Ok(());
    }
    let lock = path.with_extension("lock");
    std::fs::write(&lock, out)?;
    std::fs::rename(&lock, &path)?;
    Ok(())
}

fn is_branch_header(line: &str, name: &str) -> bool {
    let t = line.trim();
    t.strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .and_then(|r| r.trim().strip_prefix("branch"))
        .map(str::trim)
        .is_some_and(|r| r == format!("\"{name}\""))
}

/// Drops the `[branch "name"]` section from the repository configuration, or renames it to `to`.
fn move_config_section(git: &GitHome, name: &str, to: Option<&str>) -> Result<()> {
    rewrite_config(git, |lines| {
        let mut out = Vec::new();
        let mut inside = false;
        for line in lines {
            if line.trim_start().starts_with('[') {
                inside = is_branch_header(line, name);
                if inside && let Some(to) = to {
                    out.push(format!("[branch \"{to}\"]\n"));
                    continue;
                }
            }
            if inside && to.is_none() {
                continue;
            }
            out.push((*line).to_string());
        }
        out
    })
}

fn list(git: &GitHome, args: &BranchArgs) -> Result<()> {
    let head = git.repo.head()?;
    let patterns: Vec<_> = args
        .names
        .iter()
        .map(|p| Glob::new(p).map(|g| g.compile_matcher()))
        .collect::<std::result::Result<_, _>>()?;
    let mut rows: Vec<Row> = Vec::new();
    if patterns.is_empty()
        && let Head::Detached(id) = &head
    {
        rows.push(Row {
            marker: '*',
            name: format!("(HEAD detached at {})", short(id)),
            id: Some(*id),
        });
    }
    for (full, id) in git.repo.refs().list(BRANCHES)? {
        let name = full[BRANCHES.len()..].to_string();
        if !patterns.is_empty() && !patterns.iter().any(|p| p.is_match(&name)) {
            continue;
        }
        let marker = match &head {
            Head::Branch { name: n, .. } if *n == name => '*',
            _ if git.used_elsewhere(&name)?.is_some() => '+',
            _ => ' ',
        };
        rows.push(Row {
            marker,
            name,
            id: Some(id),
        });
    }
    let width = rows
        .iter()
        .map(|r| r.name.chars().count())
        .max()
        .unwrap_or(0);
    for row in rows {
        if args.verbose
            && let Some(id) = &row.id
        {
            println!(
                "{} {:<width$} {} {}",
                row.marker,
                row.name,
                short(id),
                subject(git, id)?
            );
        } else {
            println!("{} {}", row.marker, row.name);
        }
    }
    Ok(())
}

fn create(git: &GitHome, name: &str, start: Option<&str>) -> Result<()> {
    GitHome::check_branch_name(name)?;
    if git.branch_commit(name)?.is_some() {
        bail!("a branch named '{name}' already exists");
    }
    git.check_ref_room(&GitHome::branch_ref(name))?;
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
    git.create_ref(
        &GitHome::branch_ref(name),
        &id,
        &format!("branch: Created from {label}"),
    )
}

fn delete(git: &GitHome, names: &[String], force: bool) -> Result<()> {
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
        move_config_section(git, name, None)?;
        println!("Deleted branch {name} (was {}).", short(&id));
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

fn rename(git: &GitHome, names: &[String]) -> Result<()> {
    let head = git.repo.head()?;
    let (old, new) = match names {
        [new] => match &head {
            Head::Branch { name, .. } => (name.clone(), new.clone()),
            Head::Detached(_) => bail!("cannot rename the current branch while not on any"),
        },
        [old, new] => (old.clone(), new.clone()),
        _ => bail!("too many arguments for a rename operation"),
    };
    GitHome::check_branch_name(&new)?;
    let on_it = matches!(&head, Head::Branch { name, .. } if *name == old);
    let (old_ref, new_ref) = (GitHome::branch_ref(&old), GitHome::branch_ref(&new));
    let unborn_current = on_it && git.branch_commit(&old)?.is_none();
    let id = git.branch_commit(&old)?;
    if id.is_none() && !unborn_current {
        bail!("branch '{old}' not found");
    }
    if let Some(tree) = git.used_elsewhere(&old)? {
        bail!(
            "cannot rename branch '{old}' used by worktree at '{}'",
            tree.display()
        );
    }
    if old == new {
        return Ok(());
    }
    if git.branch_commit(&new)?.is_some() {
        bail!("a branch named '{new}' already exists");
    }
    git.check_ref_room(&new_ref)?;
    if let Some(id) = id {
        let refs = git.repo.refs();
        refs.update_checked(&new_ref, pyn_git::Expect::Absent, &id)?;
        refs.rename_reflog(&old_ref, &new_ref)?;
        refs.delete(&old_ref, pyn_git::Expect::Id(id))?;
        let message = format!("Branch: renamed {old_ref} to {new_ref}");
        git.log_ref(&new_ref, Some(&id), &id, &message, false)?;
        if on_it {
            git.log_ref("HEAD", Some(&id), &git.zero_id(), &message, false)?;
            git.repo.refs().set_symbolic("HEAD", &new_ref)?;
            git.log_ref("HEAD", Some(&git.zero_id()), &id, &message, false)?;
        }
    } else {
        git.repo.refs().set_symbolic("HEAD", &new_ref)?;
    }
    move_config_section(git, &old, Some(&new))
}

pub fn branch(git: &GitHome, args: BranchArgs) -> Result<()> {
    if args.show_current {
        if let Head::Branch { name, .. } = git.repo.head()? {
            println!("{name}");
        }
        return Ok(());
    }
    if args.delete || args.force_delete {
        return delete(git, &args.names, args.force_delete);
    }
    if args.rename {
        return rename(git, &args.names);
    }
    if args.list || args.names.is_empty() {
        return list(git, &args);
    }
    match args.names.as_slice() {
        [name] => create(git, name, None),
        [name, start] => create(git, name, Some(start)),
        _ => bail!(
            "too many arguments to create a branch; the usage is `pyn branch <name> [<start>]`"
        ),
    }
}

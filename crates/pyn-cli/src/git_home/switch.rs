//! `switch`, `checkout` and the work tree forms of `restore` on a git repository: they move `HEAD`, the index and
//! the work tree as git does, and never overwrite local changes unless `-f` (or the command's purpose) says to.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, bail};
use pyn_git::IndexEntry;
use pyn_gitwork::{Entry, Head, Kind};
use pyn_object::ObjectId;

use super::refwrite::BRANCHES;
use super::stage::{
    Entries, entries_of, index_of, put_entry, raw_mode, refuse_in_progress, remove_entry,
    resolve_specs, under,
};
use super::worktree::{Mode, move_tree, remove_entry_file, write_entry};
use super::{GitHome, Refused, convert, path_string};
use crate::branch::{CheckoutArgs, SwitchArgs};

const ABBREV: usize = 7;

fn abbrev(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

pub(super) fn subject(git: &GitHome, id: &ObjectId) -> Result<String> {
    let commit = git.repo.commit(id)?;
    let text = String::from_utf8_lossy(&commit.message).into_owned();
    Ok(text.lines().next().unwrap_or_default().to_string())
}

/// Where a switch lands.
enum Landing {
    Branch(String),
    NewBranch { name: String, start: Option<String> },
    Detach(String),
}

struct Go {
    landing: Landing,
    force: bool,
}

/// The branch before the current one, from the `HEAD` reflog (`@{-1}`).
fn previous_branch(git: &GitHome) -> Result<String> {
    for entry in git.repo.refs().read_reflog("HEAD")?.iter().rev() {
        if let Some(rest) = entry.message.strip_prefix("checkout: moving from ")
            && let Some((from, _)) = rest.split_once(" to ")
        {
            return Ok(from.to_string());
        }
    }
    bail!("invalid reference: @{{-1}}")
}

/// `M`, `A`, `D` or `T` for each tracked path that differs from `HEAD` in the index or the work tree.
fn local_changes(git: &GitHome) -> Result<Vec<(char, Vec<u8>)>> {
    let head = git.repo.head()?;
    let index = git.repo.read_index()?;
    let staged = git.repo.staged_changes(&head, &index)?;
    let unstaged = git.repo.unstaged_changes(&index)?;
    let mut paths: Vec<&Vec<u8>> = staged.keys().chain(unstaged.keys()).collect();
    paths.sort();
    paths.dedup();
    let mut out = Vec::new();
    for path in paths {
        let (s, u) = (staged.get(path).copied(), unstaged.get(path).copied());
        if u == Some(Kind::Untracked) {
            continue;
        }
        let letter = match (s, u) {
            (Some(Kind::Added), Some(Kind::Deleted)) => continue,
            (_, Some(Kind::Deleted)) | (Some(Kind::Deleted), None) => 'D',
            (Some(Kind::Added), _) => 'A',
            (Some(Kind::TypeChange), _) | (None, Some(Kind::TypeChange)) => 'T',
            _ => 'M',
        };
        out.push((letter, path.clone()));
    }
    Ok(out)
}

fn current_label(git: &GitHome) -> Result<String> {
    Ok(match git.repo.head()? {
        Head::Branch { name, .. } => name,
        Head::Detached(id) => id.to_hex(),
    })
}

fn tree_of(git: &GitHome, commit: Option<&ObjectId>) -> Result<Option<ObjectId>> {
    Ok(match commit {
        Some(c) => Some(git.repo.commit(c)?.tree),
        None => None,
    })
}

fn detach_advice(git: &GitHome, label: &str, id: &ObjectId) -> Result<()> {
    if git
        .repo
        .config()
        .get("advice.detachedhead")
        .is_none_or(|v| v != "false")
    {
        eprintln!(
            "Note: switching to '{label}'.\n\n\
             You are in 'detached HEAD' state. You can look around, make experimental\n\
             changes and commit them, and you can discard any commits you make in this\n\
             state without impacting any branches by switching back to a branch.\n\n\
             If you want to create a new branch to retain commits you create, you may\n\
             do so (now or later) by using -c with the switch command. Example:\n\n  \
             pyn switch -c <new-branch-name>\n\n\
             Or undo this operation with:\n\n  pyn switch -\n\n\
             Turn off this advice by setting config variable advice.detachedHead to false\n"
        );
    }
    eprintln!("HEAD is now at {} {}", abbrev(id), subject(git, id)?);
    Ok(())
}

fn go(git: &GitHome, go: Go) -> Result<()> {
    refuse_in_progress(git)?;
    let head = git.repo.head()?;
    let from = current_label(git)?;
    let old_commit = head.id();
    let (target, branch_ref, label, created): (Option<ObjectId>, Option<String>, String, bool) =
        match &go.landing {
            Landing::Branch(name) => {
                let Some(id) = git.branch_commit(name)? else {
                    bail!("invalid reference: {name}");
                };
                (
                    Some(id),
                    Some(GitHome::branch_ref(name)),
                    name.clone(),
                    false,
                )
            }
            Landing::NewBranch { name, start } => {
                GitHome::check_branch_name(name)?;
                if git.branch_commit(name)?.is_some() {
                    bail!("a branch named '{name}' already exists");
                }
                git.check_ref_room(&GitHome::branch_ref(name))?;
                let id = match start {
                    Some(s) => Some(git.repo.resolve(s)?),
                    None => old_commit,
                };
                (id, Some(GitHome::branch_ref(name)), name.clone(), true)
            }
            Landing::Detach(spec) => (Some(git.repo.resolve(spec)?), None, spec.clone(), false),
        };
    let same_branch = matches!(&head, Head::Branch { name, .. } if Some(format!("{BRANCHES}{name}")) == branch_ref);
    if let Landing::Branch(name) = &go.landing
        && !same_branch
        && let Some(tree) = git.used_elsewhere(name)?
    {
        bail!(
            "'{name}' is already used by worktree at '{}'",
            tree.display()
        );
    }
    let opened = git.open_index()?;
    let (old_tree, new_tree) = (
        tree_of(git, old_commit.as_ref())?,
        tree_of(git, target.as_ref())?,
    );
    let mode = if go.force { Mode::Force } else { Mode::Safe };
    if let Err(blocked) = move_tree(git, &opened, old_tree.as_ref(), new_tree.as_ref(), mode)? {
        blocked.report("checkout", "switch branches", 1);
    }
    let message = format!("checkout: moving from {from} to {label}");
    match (&branch_ref, &target) {
        (Some(r), Some(id)) => {
            if created {
                let start_label = match &go.landing {
                    Landing::NewBranch { start: Some(s), .. } => s.clone(),
                    _ => "HEAD".into(),
                };
                git.create_ref(r, id, &format!("branch: Created from {start_label}"))?;
            }
            git.set_head(Some(r), id, &message)?;
        }
        (Some(r), None) => git.repo.refs().set_symbolic("HEAD", r)?,
        (None, Some(id)) => git.set_head(None, id, &message)?,
        (None, None) => unreachable!("a detach has a commit"),
    }
    if !go.force {
        for (letter, path) in local_changes(git)? {
            println!("{letter}\t{}", path_string(&path));
        }
    }
    match &go.landing {
        Landing::NewBranch { name, .. } => eprintln!("Switched to a new branch '{name}'"),
        Landing::Branch(name) if same_branch => eprintln!("Already on '{name}'"),
        Landing::Branch(name) => eprintln!("Switched to branch '{name}'"),
        Landing::Detach(spec) => detach_advice(git, spec, &target.expect("a detach has a commit"))?,
    }
    Ok(())
}

pub fn switch(git: &GitHome, args: SwitchArgs) -> Result<()> {
    let landing = match (args.create, args.start, args.detach) {
        (Some(name), start, false) => Landing::NewBranch { name, start },
        (Some(_), _, true) => bail!("options '-c' and '--detach' cannot be used together"),
        (None, Some(spec), true) => Landing::Detach(spec),
        (None, None, true) => Landing::Detach("HEAD".into()),
        (None, Some(name), false) => {
            let name = if name == "-" {
                previous_branch(git)?
            } else {
                name
            };
            if git.branch_commit(&name)?.is_none() {
                if git.repo.resolve(&name).is_ok() {
                    bail!(
                        "a branch is expected, got commit '{name}'; use `pyn switch --detach {name}` to check it out"
                    );
                }
                bail!("invalid reference: {name}");
            }
            Landing::Branch(name)
        }
        (None, None, false) => bail!("missing branch or commit argument"),
    };
    go(
        git,
        Go {
            landing,
            force: args.force,
        },
    )
}

enum Source {
    Index,
    Tree(ObjectId),
}

struct Restore {
    source: Source,
    to_index: bool,
    to_worktree: bool,
    /// Tracked paths the source lacks are removed too.
    prune: bool,
    /// Take this stage of an unmerged path: 2 for ours, 3 for theirs.
    stage: Option<u8>,
}

fn restore_paths(git: &GitHome, cwd: &Path, paths: &[String], r: Restore) -> Result<usize> {
    if paths.is_empty() {
        bail!("you must specify path(s) to restore");
    }
    let opened = git.open_index()?;
    let specs = resolve_specs(git, cwd, paths)?;
    let mut entries: Entries = entries_of(&opened.index);
    let in_spec = |p: &[u8]| specs.spec.matches(p);
    let sources: BTreeMap<Vec<u8>, Entry> = match &r.source {
        Source::Tree(commit) => {
            let tree = git.repo.commit(commit)?.tree;
            git.repo.flatten(Some(&tree), &specs.spec)?
        }
        Source::Index => {
            let wanted = r.stage.unwrap_or(0);
            let mut found = BTreeMap::new();
            for ((path, stage), e) in &entries {
                if !in_spec(path) {
                    continue;
                }
                if *stage == wanted {
                    found.insert(
                        path.clone(),
                        Entry {
                            mode: super::stage::leaf_mode(e.mode),
                            id: e.id,
                        },
                    );
                } else if r.stage.is_none()
                    && *stage > 0
                    && !entries.contains_key(&(path.clone(), 0))
                {
                    bail!("path '{}' is unmerged", path_string(path));
                }
            }
            found
        }
    };
    let indexed: Vec<Vec<u8>> = entries.keys().map(|(p, _)| p.clone()).collect();
    for (arg, prefix) in &specs.named {
        let known = sources
            .keys()
            .chain(indexed.iter())
            .any(|p| under(p, prefix.as_bytes()));
        if !known {
            return Err(Refused(format!(
                "pathspec '{arg}' did not match any file(s) known to git"
            ))
            .into());
        }
    }
    let doomed: Vec<Vec<u8>> = if r.prune {
        indexed
            .iter()
            .filter(|p| in_spec(p) && !sources.contains_key(*p))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    if r.to_worktree {
        convert::refuse_conversions(git, sources.keys().map(Vec::as_slice))?;
    }
    for (path, entry) in &sources {
        let written = if r.to_worktree {
            Some(write_entry(git, path, entry)?)
        } else {
            None
        };
        if r.to_index {
            let e = written
                .unwrap_or_else(|| IndexEntry::new(path.clone(), raw_mode(entry.mode), entry.id));
            put_entry(&mut entries, e);
        }
    }
    for path in &doomed {
        if r.to_worktree {
            remove_entry_file(git, path)?;
        }
        if r.to_index {
            remove_entry(&mut entries, path);
        }
    }
    if r.to_index {
        git.save_index(&opened, &index_of(opened.index.version, entries))?;
    }
    Ok(sources.len() + doomed.len())
}

fn commit_of(git: &GitHome, spec: &str) -> Result<ObjectId> {
    Ok(git.repo.resolve(spec)?)
}

/// `pyn restore` with the work tree as a target.
pub fn restore(
    git: &GitHome,
    cwd: &Path,
    paths: &[String],
    source: Option<&str>,
    staged: bool,
    worktree: bool,
) -> Result<()> {
    let to_worktree = worktree || !staged;
    let source = match source {
        Some(s) => Source::Tree(commit_of(git, s)?),
        None if staged => match git.repo.head_commit()? {
            Some(head) => Source::Tree(head),
            None => bail!("could not resolve HEAD"),
        },
        None => Source::Index,
    };
    let prune = matches!(source, Source::Tree(_));
    restore_paths(
        git,
        cwd,
        paths,
        Restore {
            source,
            to_index: staged,
            to_worktree,
            prune,
            stage: None,
        },
    )?;
    Ok(())
}

fn is_path(git: &GitHome, cwd: &Path, name: &str) -> Result<bool> {
    let Ok(prefix) = git.ws.repo_prefix(cwd, name) else {
        return Ok(false);
    };
    let index = git.repo.read_index()?;
    Ok(index
        .entries
        .iter()
        .any(|e| under(&e.path, prefix.as_bytes())))
}

pub fn checkout(git: &GitHome, cwd: &Path, args: CheckoutArgs) -> Result<()> {
    let CheckoutArgs {
        args: names,
        paths,
        create,
        force,
        detach,
        ours,
        theirs,
    } = args;
    let stage = match (ours, theirs) {
        (true, _) => Some(2),
        (_, true) => Some(3),
        _ => None,
    };
    let dashes_paths = !paths.is_empty();
    if dashes_paths || stage.is_some() {
        if create.is_some() || detach {
            bail!("'pyn checkout -- <paths>' cannot be used with switching branches");
        }
        let (tree_ish, paths) = match (names.as_slice(), dashes_paths) {
            ([], true) => (None, paths),
            ([one], true) => (Some(one.clone()), paths),
            _ => bail!("use `pyn checkout --ours` or `--theirs` with `-- <paths>`"),
        };
        let r = match tree_ish {
            Some(t) => Restore {
                source: Source::Tree(commit_of(git, &t)?),
                to_index: true,
                to_worktree: true,
                prune: false,
                stage,
            },
            None => Restore {
                source: Source::Index,
                to_index: false,
                to_worktree: true,
                prune: false,
                stage,
            },
        };
        let label = match &r.source {
            Source::Tree(c) => abbrev(c),
            Source::Index => "the index".into(),
        };
        let n = restore_paths(git, cwd, &paths, r)?;
        eprintln!(
            "Updated {n} path{} from {label}",
            if n == 1 { "" } else { "s" }
        );
        return Ok(());
    }
    if let Some(name) = create {
        if detach {
            bail!("options '-b' and '--detach' cannot be used together");
        }
        let start = names.first().cloned();
        return go(
            git,
            Go {
                landing: Landing::NewBranch { name, start },
                force,
            },
        );
    }
    let Some(first) = names.first() else {
        bail!("no branch or commit given; name one, or use `pyn status` to see where you are");
    };
    if names.len() > 1 {
        let (tree_ish, rest) = (first.clone(), &names[1..]);
        let source = Source::Tree(commit_of(git, &tree_ish)?);
        let label = match &source {
            Source::Tree(c) => abbrev(c),
            Source::Index => unreachable!("a commit was named"),
        };
        let n = restore_paths(
            git,
            cwd,
            rest,
            Restore {
                source,
                to_index: true,
                to_worktree: true,
                prune: false,
                stage: None,
            },
        )?;
        eprintln!(
            "Updated {n} path{} from {label}",
            if n == 1 { "" } else { "s" }
        );
        return Ok(());
    }
    let name = match (first.as_str(), previous_branch(git)) {
        ("-", Ok(previous)) => previous,
        _ => first.clone(),
    };
    if !detach && git.branch_commit(&name)?.is_some() {
        return go(
            git,
            Go {
                landing: Landing::Branch(name),
                force,
            },
        );
    }
    if git.repo.resolve(&name).is_ok() {
        return go(
            git,
            Go {
                landing: Landing::Detach(name),
                force,
            },
        );
    }
    if !detach && is_path(git, cwd, &name)? {
        let n = restore_paths(
            git,
            cwd,
            std::slice::from_ref(&name),
            Restore {
                source: Source::Index,
                to_index: false,
                to_worktree: true,
                prune: false,
                stage: None,
            },
        )?;
        eprintln!(
            "Updated {n} path{} from the index",
            if n == 1 { "" } else { "s" }
        );
        return Ok(());
    }
    Err(Refused(format!(
        "pathspec '{name}' did not match any file(s) known to git"
    ))
    .into())
}

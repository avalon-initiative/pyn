//! `pyn merge` on a git repository: fast-forward, a merge commit with two parents, or a merge left in progress with
//! unmerged index entries and `MERGE_HEAD`, `MERGE_MSG` and `MERGE_MODE` written as git writes them, so either tool
//! can finish or abort it.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use pyn_git::IndexEntry;
use pyn_gitwork::{Entry, Head, Kind, Pathspec};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId};

use super::stage::{
    Entries, clean_message, clear_merge_state, entries_of, identity, index_of, merge_heads,
    put_entry, raw_mode, refuse_in_progress, remove_entry, warn_about_hooks_for, write_tree,
};
use super::treemerge::{Conflicting, Labels, Resolved, Worktree, merge_trees};
use super::worktree::{
    Blocked, Mode, check_writes, in_the_way, move_tree, remove_entry_file, write_bytes, write_entry,
};
use super::{GitHome, Refused, convert, path_string};
use crate::merging::MergeArgs;

const ABBREV: usize = 7;

/// Set by `pyn pull -q`: the informational lines below are not printed.
pub(super) static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The exit status when local changes block a fast-forward: 2 for `merge`, 1 for `pull`.
pub(super) static BLOCKED_STATUS: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(2);

fn blocked_status() -> i32 {
    BLOCKED_STATUS.load(std::sync::atomic::Ordering::Relaxed)
}

fn say(text: &str) {
    if !QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        println!("{text}");
    }
}
const MERGE_HOOKS: [&str; 5] = [
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-merge",
    "post-commit",
];

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

/// How git names what is being merged in a message: `branch 'x'`, `tag 'x'`, `commit 'x'`.
fn describe(git: &GitHome, spec: &str) -> String {
    let refs = git.repo.refs();
    let exists = |name: &str| {
        pyn_git::check_ref_name(name).is_ok() && matches!(refs.resolve(name), Ok(Some(_)))
    };
    for (prefix, what) in [
        ("refs/tags/", "tag"),
        ("refs/heads/", "branch"),
        ("refs/remotes/", "remote-tracking branch"),
    ] {
        if let Some(short) = spec.strip_prefix(prefix)
            && exists(spec)
        {
            return format!("{what} '{short}'");
        }
        if exists(&format!("{prefix}{spec}")) {
            return format!("{what} '{spec}'");
        }
    }
    format!("commit '{spec}'")
}

/// The message of the annotated tag a merge names, which git appends to the merge message.
fn tag_message(git: &GitHome, spec: &str) -> Option<String> {
    let name = format!("refs/tags/{spec}");
    if pyn_git::check_ref_name(&name).is_err() {
        return None;
    }
    let id = git.repo.refs().resolve(&name).ok()??;
    let object = git.repo.objects().read(&id).ok()?;
    if object.kind != ObjectKind::Tag {
        return None;
    }
    let tag = pyn_object::Tag::parse(git.repo.algorithm(), &object.data).ok()?;
    Some(String::from_utf8_lossy(&tag.message).trim_end().to_string())
}

pub(super) fn default_message(git: &GitHome, what: &str) -> Result<String> {
    let into = match git.repo.head()? {
        Head::Branch { name, .. } if name == "main" || name == "master" => String::new(),
        Head::Branch { name, .. } => format!(" into {name}"),
        Head::Detached(_) => " into HEAD".into(),
    };
    Ok(format!("Merge {what}{into}"))
}

fn write_state(git: &GitHome, name: &str, text: &str) -> Result<()> {
    let path = git.repo.git().git_dir().join(name);
    std::fs::write(&path, text).map_err(|e| anyhow!("writing {}: {e}", path.display()))
}

/// `ORIG_HEAD` is where `HEAD` was before a merge, as git leaves it for `git reset --merge`.
fn write_orig_head(git: &GitHome, id: &ObjectId) -> Result<()> {
    write_state(git, "ORIG_HEAD", &format!("{id}\n"))
}

pub fn merge(git: &GitHome, args: MergeArgs) -> Result<()> {
    if args.preview {
        bail!("`pyn merge --preview` needs a pyn server; a git repository has no merge preview");
    }
    if args.abort {
        return abort(git);
    }
    if args.continue_merge {
        if merge_heads(git)?.is_empty() {
            bail!("There is no merge in progress (MERGE_HEAD missing).");
        }
        return super::commit::commit(
            git,
            &super::commit::CommitOptions {
                messages: args.message.clone(),
                ..Default::default()
            },
        );
    }
    let Some(spec) = args.source.clone() else {
        bail!("no merge target given; name a branch or a commit");
    };
    refuse_in_progress(git)?;
    let head = git.repo.head()?;
    let opened = git.open_index()?;
    if opened.index.entries.iter().any(|e| e.stage > 0) {
        bail!("Merging is not possible because you have unmerged files.");
    }
    let theirs = git
        .repo
        .resolve(&spec)
        .map_err(|_| Refused(format!("{spec} - not something we can merge")))?;
    let what = describe(git, &spec);
    let Some(ours) = head.id() else {
        return fast_forward(git, &opened, None, &theirs, &spec);
    };
    if git.is_ancestor(&theirs, &ours)? {
        write_orig_head(git, &ours)?;
        say("Already up to date.");
        return Ok(());
    }
    let can_ff = git.is_ancestor(&ours, &theirs)?;
    if can_ff && !args.no_ff {
        return fast_forward(git, &opened, Some(ours), &theirs, &spec);
    }
    if args.ff_only {
        write_orig_head(git, &ours)?;
        bail!("Not possible to fast-forward, aborting.");
    }
    true_merge(git, &args, &opened, ours, theirs, &spec, &what)
}

/// Checks `theirs` out onto an unborn branch and points the branch at it, without a word, as `git pull` does.
pub(super) fn initial_checkout(git: &GitHome, theirs: &ObjectId, action: &str) -> Result<()> {
    let opened = git.open_index()?;
    let tree = git.repo.commit(theirs)?.tree;
    if let Err(blocked) = move_tree(git, &opened, None, Some(&tree), Mode::Safe)? {
        blocked.report("merge", "merge", blocked_status());
    }
    git.move_head(theirs, action)
}

fn fast_forward(
    git: &GitHome,
    opened: &super::stage::OpenIndex,
    ours: Option<ObjectId>,
    theirs: &ObjectId,
    spec: &str,
) -> Result<()> {
    let old_tree = match &ours {
        Some(c) => Some(git.repo.commit(c)?.tree),
        None => None,
    };
    let new_tree = git.repo.commit(theirs)?.tree;
    if let Err(blocked) = move_tree(git, opened, old_tree.as_ref(), Some(&new_tree), Mode::Safe)? {
        if let Some(ours) = &ours {
            write_orig_head(git, ours)?;
        }
        blocked.report("merge", "merge", blocked_status());
    }
    if let Some(ours) = &ours {
        write_orig_head(git, ours)?;
        say(&format!("Updating {}..{}", short(ours), short(theirs)));
    }
    git.move_head(theirs, &format!("merge {spec}: Fast-forward"))?;
    say("Fast-forward");
    let changed = git
        .repo
        .diff_trees(old_tree.as_ref(), Some(&new_tree), &Pathspec::default())?
        .len();
    say(&format!("{changed} file(s) changed"));
    warn_about_hooks_for(git, &MERGE_HOOKS);
    Ok(())
}

fn true_merge(
    git: &GitHome,
    args: &MergeArgs,
    opened: &super::stage::OpenIndex,
    ours: ObjectId,
    theirs: ObjectId,
    spec: &str,
    what: &str,
) -> Result<()> {
    let bases = git.merge_bases(&ours, &theirs)?;
    let base = match bases.as_slice() {
        [] => bail!("refusing to merge unrelated histories"),
        [one] => *one,
        _ => bail!(
            "the two histories have more than one merge base (a criss-cross merge); pyn cannot merge them yet, \
             and nothing was changed"
        ),
    };
    let tree_of = |c: &ObjectId| -> Result<ObjectId> { Ok(git.repo.commit(c)?.tree) };
    let (base_tree, ours_tree, theirs_tree) = (tree_of(&base)?, tree_of(&ours)?, tree_of(&theirs)?);
    let base_label = short(&base);
    let incoming =
        git.repo
            .diff_trees(Some(&base_tree), Some(&theirs_tree), &Pathspec::default())?;
    convert::refuse_conversions(
        git,
        incoming
            .iter()
            .filter_map(|c| c.new.map(|e| (c.path.as_slice(), e.mode))),
    )?;
    let merged = merge_trees(
        git,
        Some(&base_tree),
        Some(&ours_tree),
        Some(&theirs_tree),
        &Labels {
            ours: "HEAD",
            theirs: spec,
            base: &base_label,
        },
    )?;
    check_writes(merged.paths.keys())?;
    let head_leaves = git.repo.flatten(Some(&ours_tree), &Pathspec::default())?;

    let mut blocked = Blocked::default();
    let index_leaves = git.repo.index_leaves(&opened.index);
    for path in head_leaves.keys().chain(index_leaves.keys()) {
        if head_leaves.get(path) != index_leaves.get(path) && !blocked.local.contains(path) {
            blocked.local.push(path.clone());
        }
    }
    let index_differs = !blocked.local.is_empty();
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let ignores = super::stage::Ignores {
        ignore: git.repo.ignore()?,
    };
    let none = std::collections::HashSet::new();
    for (path, resolved) in &merged.paths {
        let here = head_leaves.get(path);
        let touches_tree = match resolved {
            Resolved::Take(e) => e.as_ref() != here,
            Resolved::Conflict(c) => !matches!(c.worktree, Worktree::Keep),
        };
        if !touches_tree {
            continue;
        }
        match here {
            Some(_) if matches!(dirty.get(path), Some(Kind::Modified | Kind::TypeChange)) => {
                blocked.local.push(path.clone());
            }
            None if in_the_way(git, &ignores, path, &none)? => blocked.untracked.push(path.clone()),
            _ => {}
        }
    }
    if !blocked.local.is_empty() || !blocked.untracked.is_empty() {
        blocked.local.sort();
        blocked.local.dedup();
        write_orig_head(git, &ours)?;
        if index_differs {
            git.log_ref(
                "HEAD",
                Some(&ours),
                &ours,
                &format!("merge {spec}: updating HEAD"),
                false,
            )?;
        }
        blocked.report("merge", "merge", 2);
    }
    write_orig_head(git, &ours)?;
    let mut entries: Entries = entries_of(&opened.index);
    let mut conflicted: Vec<Vec<u8>> = Vec::new();
    for (path, resolved) in &merged.paths {
        if merged.line_merged.contains(path) {
            say(&format!("Auto-merging {}", path_string(path)));
        }
        match resolved {
            Resolved::Take(Some(entry)) => {
                if head_leaves.get(path) != Some(entry) {
                    put_entry(&mut entries, write_entry(git, path, entry)?);
                }
            }
            Resolved::Take(None) => {
                if head_leaves.contains_key(path) {
                    remove_entry_file(git, path)?;
                    remove_entry(&mut entries, path);
                }
            }
            Resolved::Conflict(c) => {
                conflicted.push(path.clone());
                report_conflict(path, c.kind, spec, c.ours.is_some() && c.theirs.is_some())?;
                if let Worktree::Write { mode, bytes } = &c.worktree {
                    write_bytes(git, path, *mode, bytes)?;
                }
                remove_entry(&mut entries, path);
                for (stage, side) in [(1u8, &c.base), (2, &c.ours), (3, &c.theirs)] {
                    if let Some(e) = side {
                        let mut entry = IndexEntry::new(path.clone(), raw_mode(e.mode), e.id);
                        entry.stage = stage;
                        entries.insert((path.clone(), stage), entry);
                    }
                }
            }
        }
    }
    let index = index_of(opened.index.version, entries);
    git.save_index(opened, &index)?;

    let message = match args.message.is_empty() {
        true => match tag_message(git, spec) {
            Some(tag) if what.starts_with("tag ") => {
                format!("{}\n\n{tag}\n", default_message(git, what)?)
            }
            _ => format!("{}\n", default_message(git, what)?),
        },
        false => String::from_utf8_lossy(&clean_message(&args.message)).into_owned(),
    };
    let mode = if args.no_ff { "no-ff" } else { "" };
    if !conflicted.is_empty() {
        let mut text = format!("{message}\n# Conflicts:\n");
        for p in &conflicted {
            text.push_str(&format!("#\t{}\n", path_string(p)));
        }
        write_state(git, "MERGE_HEAD", &format!("{theirs}\n"))?;
        write_state(git, "MERGE_MSG", &text)?;
        write_state(git, "MERGE_MODE", mode)?;
        println!("Automatic merge failed; fix conflicts and then commit the result.");
        std::process::exit(1);
    }
    if args.no_commit {
        write_state(git, "MERGE_HEAD", &format!("{theirs}\n"))?;
        write_state(git, "MERGE_MSG", &message)?;
        write_state(git, "MERGE_MODE", mode)?;
        println!("Automatic merge went well; stopped before committing as requested");
        return Ok(());
    }
    let tree = write_tree(git, &index)?;
    let record = Commit {
        tree,
        parents: vec![ours, theirs],
        author: identity(git, "author")?,
        committer: identity(git, "committer")?,
        extra_headers: Vec::new(),
        message: message.into_bytes(),
    };
    let id = git
        .repo
        .objects()
        .write(ObjectKind::Commit, &record.encode(git.algorithm())?)?;
    git.move_head(
        &id,
        &format!("merge {spec}: Merge made by the 'ort' strategy."),
    )?;
    say("Merge made by the 'ort' strategy.");
    let changed = git
        .repo
        .diff_trees(Some(&ours_tree), Some(&tree), &Pathspec::default())?
        .len();
    say(&format!("{changed} file(s) changed"));
    warn_about_hooks_for(git, &MERGE_HOOKS);
    Ok(())
}

fn report_conflict(path: &[u8], kind: Conflicting, theirs: &str, both: bool) -> Result<()> {
    let name = path_string(path);
    match kind {
        Conflicting::Content | Conflicting::Binary if both => {
            if kind == Conflicting::Binary {
                println!("warning: Cannot merge binary files: {name} (HEAD vs. {theirs})");
            }
            println!("CONFLICT (content): Merge conflict in {name}");
        }
        Conflicting::AddAdd => println!("CONFLICT (add/add): Merge conflict in {name}"),
        Conflicting::ModifyDelete { ours_deleted } => {
            let (deleted, kept) = if ours_deleted {
                ("HEAD", theirs)
            } else {
                (theirs, "HEAD")
            };
            println!(
                "CONFLICT (modify/delete): {name} deleted in {deleted} and modified in {kept}.  Version {kept} of {name} left in tree."
            );
        }
        _ => println!("CONFLICT (content): Merge conflict in {name}"),
    }
    Ok(())
}

/// `pyn merge --abort`: puts back what the merge changed and keeps other local changes, as `git merge --abort`.
fn abort(git: &GitHome) -> Result<()> {
    if merge_heads(git)?.is_empty() {
        bail!("There is no merge to abort (MERGE_HEAD missing).");
    }
    let Some(head) = git.repo.head_commit()? else {
        bail!("could not resolve HEAD");
    };
    let tree = git.repo.commit(&head)?.tree;
    let committed: BTreeMap<Vec<u8>, Entry> =
        git.repo.flatten(Some(&tree), &Pathspec::default())?;
    let opened = git.open_index()?;
    let mut entries: Entries = entries_of(&opened.index);
    let leaves = git.repo.index_leaves(&opened.index);
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let unmerged: std::collections::BTreeSet<Vec<u8>> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.stage > 0)
        .map(|e| e.path.clone())
        .collect();
    let mut redo: Vec<Vec<u8>> = Vec::new();
    let mut stale: Vec<Vec<u8>> = Vec::new();
    let paths: std::collections::BTreeSet<&Vec<u8>> = committed
        .keys()
        .chain(leaves.keys())
        .chain(unmerged.iter())
        .collect();
    for path in paths {
        if unmerged.contains(path) {
            redo.push(path.clone());
        } else if committed.get(path) != leaves.get(path) {
            if matches!(dirty.get(path), Some(Kind::Modified | Kind::TypeChange)) {
                stale.push(path.clone());
            } else {
                redo.push(path.clone());
            }
        }
    }
    if !stale.is_empty() {
        let names: Vec<String> = stale.iter().map(|p| path_string(p)).collect();
        bail!(
            "Entry '{}' not uptodate. Cannot merge; could not reset the index to HEAD",
            names.join("', '")
        );
    }
    check_writes(&redo)?;
    convert::refuse_conversions(
        git,
        redo.iter()
            .filter_map(|p| committed.get(p).map(|e| (p.as_slice(), e.mode))),
    )?;
    for path in &redo {
        remove_entry(&mut entries, path);
        match committed.get(path) {
            Some(entry) => put_entry(&mut entries, write_entry(git, path, entry)?),
            None => remove_entry_file(git, path)?,
        }
    }
    git.save_index(&opened, &index_of(opened.index.version, entries))?;
    clear_merge_state(git);
    git.log_ref("HEAD", Some(&head), &head, "reset: moving to HEAD", false)?;
    if let Some(branch) = git.head_target()? {
        git.log_if_present(&branch, &head, &head, "reset: moving to HEAD")?;
    }
    Ok(())
}

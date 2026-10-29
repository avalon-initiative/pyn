//! `pyn submodule summary`: the commits between the recorded and the current commit of each submodule.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use clap::Args;
use pyn_gitwork::{Entry, Pathspec};
use pyn_object::{FileMode, ObjectId};

use super::GitHome;
use super::commands::emit;

/// Show the commits that separate the recorded submodule commits from the current ones.
#[derive(Debug, Args)]
pub struct SummaryArgs {
    /// Compare the commit with the index instead of the work tree.
    #[arg(long, conflicts_with = "files")]
    cached: bool,
    /// Compare the index with the work tree.
    #[arg(long)]
    files: bool,
    /// Show at most this many commits per submodule; 0 shows nothing, a negative number everything.
    #[arg(short = 'n', long, default_value_t = -1, allow_hyphen_values = true)]
    summary_limit: i64,
    /// A commit to compare with (default `HEAD`), then paths.
    rest: Vec<String>,
}

pub(super) fn summary(git: &GitHome, cwd: &Path, args: &SummaryArgs) -> Result<()> {
    if args.summary_limit == 0 {
        return Ok(());
    }
    let (base, paths) = match args.rest.split_first() {
        Some((first, rest)) if !args.files && git.repo.resolve(first).is_ok() => {
            (Some(git.repo.resolve(first)?), rest.to_vec())
        }
        _ => (None, args.rest.clone()),
    };
    let spec = git.pathspec(cwd, &paths, false)?;
    let tree = match base.or(git.repo.head_commit()?) {
        Some(id) => Some(git.repo.commit(&id)?.tree),
        None => None,
    };
    let recorded = git.repo.flatten(tree.as_ref(), &Pathspec::default())?;
    let index = git.repo.index_leaves(&git.repo.read_index()?);
    let root = git.repo.root()?.to_path_buf();
    let current = |path: &[u8]| -> Result<Option<Entry>> {
        let dir = root.join(String::from_utf8_lossy(path).as_ref());
        if !dir.join(".git").exists() {
            return Ok(None);
        }
        let child = GitHome::open_at(&dir)?;
        Ok(child.repo.head_commit()?.map(|id| Entry {
            mode: FileMode::Gitlink,
            id,
        }))
    };
    let names: BTreeSet<&Vec<u8>> = recorded.keys().chain(index.keys()).collect();
    let mut out = String::new();
    for path in names.into_iter().filter(|p| spec.matches(p)) {
        let (src, dst) = if args.cached {
            (recorded.get(path).copied(), index.get(path).copied())
        } else if args.files {
            if !gitlink(index.get(path)) {
                continue;
            }
            (index.get(path).copied(), current(path)?)
        } else {
            if !gitlink(recorded.get(path)) && !gitlink(index.get(path)) {
                continue;
            }
            let now = current(path)?;
            if now.is_none() {
                continue;
            }
            (recorded.get(path).copied(), now)
        };
        if !(gitlink(src.as_ref()) || gitlink(dst.as_ref()))
            || src.as_ref().is_some_and(|e| e.mode != FileMode::Gitlink)
            || dst.as_ref().is_some_and(|e| e.mode != FileMode::Gitlink)
            || src == dst
        {
            continue;
        }
        if !root
            .join(String::from_utf8_lossy(path).as_ref())
            .join(".git")
            .exists()
        {
            continue;
        }
        out.push_str(&block(git, &root, path, src, dst, args.summary_limit)?);
    }
    emit(&out);
    Ok(())
}

fn gitlink(entry: Option<&Entry>) -> bool {
    entry.is_some_and(|e| e.mode == FileMode::Gitlink)
}

fn block(
    git: &GitHome,
    root: &Path,
    path: &[u8],
    src: Option<Entry>,
    dst: Option<Entry>,
    limit: i64,
) -> Result<String> {
    let name = String::from_utf8_lossy(path).into_owned();
    let dir = root.join(&name);
    let child = if dir.join(".git").exists() {
        Some(GitHome::open_at(&dir)?)
    } else {
        None
    };
    let abbreviations = child.as_ref().unwrap_or(git).repo.abbreviations(None)?;
    let short = |e: Option<Entry>| match e {
        Some(e) => abbreviations.short(&e.id),
        None => abbreviations.short_null(),
    };
    let mut text = format!("* {name} {}...{}", short(src), short(dst));
    let has = |e: Option<Entry>| -> Result<bool> {
        match (e, &child) {
            (Some(e), Some(c)) => Ok(c.repo.objects().read_opt(&e.id)?.is_some()),
            (Some(_), None) => Ok(false),
            (None, _) => Ok(true),
        }
    };
    let (have_src, have_dst) = (has(src)?, has(dst)?);
    let missing = |e: Option<Entry>| e.map(|e| e.id.to_hex()).unwrap_or_default();
    let warn = match (have_src, have_dst) {
        (true, true) => None,
        (false, true) => Some(format!("commit {}", missing(src))),
        (true, false) => Some(format!("commit {}", missing(dst))),
        (false, false) => Some(format!("commits {} and {}", missing(src), missing(dst))),
    };
    if let Some(what) = warn {
        text.push_str(&format!(":\n  Warn: {name} doesn't contain {what}\n\n"));
        return Ok(text);
    }
    let Some(child) = child else {
        text.push_str(":\n\n");
        return Ok(text);
    };
    let lines = match (src, dst) {
        (Some(s), Some(d)) => {
            let left = exclusive(&child, &s.id, Some(&d.id))?;
            let right = exclusive(&child, &d.id, Some(&s.id))?;
            let total = left.len() + right.len();
            text.push_str(&format!(" ({total})"));
            ordered(&child, left, right)?
        }
        (None, Some(d)) => {
            let chain = exclusive(&child, &d.id, None)?;
            text.push_str(&format!(" ({})", chain.len()));
            chain.into_iter().take(1).map(|id| ('>', id)).collect()
        }
        _ => Vec::new(),
    };
    text.push_str(":\n");
    for (mark, id) in lines.into_iter().take(if limit < 0 {
        usize::MAX
    } else {
        limit as usize
    }) {
        let message = child.repo.commit(&id)?.message;
        let subject = String::from_utf8_lossy(&message);
        let subject = subject.lines().next().unwrap_or("");
        text.push_str(&format!("  {mark} {subject}\n"));
    }
    text.push('\n');
    Ok(text)
}

/// The first-parent commits from `tip` down to the first one `other` already has.
fn exclusive(child: &GitHome, tip: &ObjectId, other: Option<&ObjectId>) -> Result<Vec<ObjectId>> {
    let mut out = Vec::new();
    let mut at = Some(*tip);
    while let Some(id) = at {
        if let Some(other) = other
            && child.is_ancestor(&id, other)?
        {
            break;
        }
        out.push(id);
        at = child.repo.commit(&id)?.parents.first().copied();
    }
    Ok(out)
}

/// Both sides newest first by commit time (ties in discovery order, as git's walk), the recorded side marked `<`.
fn ordered(
    child: &GitHome,
    left: Vec<ObjectId>,
    right: Vec<ObjectId>,
) -> Result<Vec<(char, ObjectId)>> {
    let sides = [('<', left), ('>', right)];
    let mut next = [0usize; 2];
    let mut queued: Vec<(i64, u64, usize)> = Vec::new();
    let mut seq = 0;
    let time = |side: usize, at: usize| -> Result<i64> {
        Ok(child.repo.commit(&sides[side].1[at])?.committer.time)
    };
    for (side, (_, ids)) in sides.iter().enumerate() {
        if !ids.is_empty() {
            queued.push((time(side, 0)?, seq, side));
            seq += 1;
        }
    }
    let mut out = Vec::new();
    while let Some(best) = queued
        .iter()
        .enumerate()
        .min_by_key(|(_, (t, s, _))| (std::cmp::Reverse(*t), *s))
        .map(|(i, _)| i)
    {
        let (_, _, side) = queued.remove(best);
        out.push((sides[side].0, sides[side].1[next[side]]));
        next[side] += 1;
        if next[side] < sides[side].1.len() {
            queued.push((time(side, next[side])?, seq, side));
            seq += 1;
        }
    }
    Ok(out)
}

//! `pyn shortlog` on a git repository: commit subjects grouped by author.

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, IsTerminal};
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_object::{Kind, ObjectId};

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes, peel, prefix};

/// Summarize the history by author.
#[derive(Debug, Args)]
pub struct ShortlogArgs {
    /// Sort by the number of commits, most first, instead of by name.
    #[arg(short = 'n', long)]
    numbered: bool,
    /// Show only the number of commits per author.
    #[arg(short = 's', long)]
    summary: bool,
    /// Show each author's email address too.
    #[arg(short = 'e', long)]
    email: bool,
    /// Group by committer instead of author.
    #[arg(short = 'c', long)]
    committer: bool,
    /// Revisions to walk (`HEAD` if none; the log on standard input if that is not a terminal): `<rev>`,
    /// `^<rev>`, `<a>..<b>`, `<a>...<b>`.
    revs: Vec<String>,
    /// Count only commits that change these paths.
    #[arg(last = true)]
    paths: Vec<String>,
}

type Groups = BTreeMap<Vec<u8>, Vec<String>>;

pub(super) fn shortlog(git: &GitHome, cwd: &Path, args: ShortlogArgs) -> Result<()> {
    if git.repo.root().is_ok_and(|r| r.join(".mailmap").exists())
        || git.config().get("mailmap.file").is_some()
        || git.config().get("mailmap.blob").is_some()
    {
        bail!("shortlog does not read mailmaps on a git repository yet");
    }
    let mut groups = Groups::new();
    if args.revs.is_empty() && !std::io::stdin().is_terminal() {
        from_stdin(&mut groups, &args)?;
    } else {
        from_history(git, cwd, &args, &mut groups)?;
    }
    emit_bytes(&render(groups, &args));
    Ok(())
}

fn from_history(git: &GitHome, cwd: &Path, args: &ShortlogArgs, groups: &mut Groups) -> Result<()> {
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let commit_of = |text: &str| -> Result<ObjectId> {
        let text = if text.is_empty() { "HEAD" } else { text };
        let found = match resolver.rev(text)? {
            Some(id) => peel(git, &id, Some(Kind::Commit))?,
            None => None,
        };
        match found {
            Some(id) => Ok(id),
            None => bail!(
                "ambiguous argument '{text}': unknown revision or path not in the working tree."
            ),
        }
    };
    let (mut include, mut hidden): (Vec<ObjectId>, HashSet<ObjectId>) =
        (Vec::new(), HashSet::new());
    for rev in &args.revs {
        if let Some(rest) = rev.strip_prefix('^') {
            hidden.extend(git.reachable(&commit_of(rest)?)?);
        } else if let Some((a, b)) = rev.split_once("...") {
            let (a, b) = (commit_of(a)?, commit_of(b)?);
            let (from_a, from_b) = (git.reachable(&a)?, git.reachable(&b)?);
            hidden.extend(from_a.intersection(&from_b).copied());
            include.extend([a, b]);
        } else if let Some((a, b)) = rev.split_once("..") {
            hidden.extend(git.reachable(&commit_of(a)?)?);
            include.push(commit_of(b)?);
        } else {
            include.push(commit_of(rev)?);
        }
    }
    if args.revs.is_empty() {
        let Some(head) = git.repo.head_commit()? else {
            bail!("your current branch does not have any commits yet");
        };
        include.push(head);
    }
    let spec = git.pathspec(cwd, &args.paths, false)?;
    let mut walk = git.repo.log_hiding(&include, hidden, spec)?;
    while let Some(entry) = walk.next_entry()? {
        let who = if args.committer {
            &entry.commit.committer
        } else {
            &entry.commit.author
        };
        let mut key = who.name.clone();
        if args.email {
            key.extend_from_slice(b" <");
            key.extend_from_slice(&who.email);
            key.push(b'>');
        }
        groups
            .entry(key)
            .or_default()
            .push(subject(&entry.commit.message));
    }
    Ok(())
}

/// Reads `git log` text: each `Author:` line, then the first line of its message.
fn from_stdin(groups: &mut Groups, args: &ShortlogArgs) -> Result<()> {
    let mut lines = std::io::stdin().lock().split(b'\n');
    while let Some(line) = lines.next().transpose()? {
        let Some(who) = line
            .strip_prefix(b"Author: ")
            .or_else(|| line.strip_prefix(b"author "))
        else {
            continue;
        };
        let mut message = Vec::new();
        for l in lines.by_ref() {
            if l?.is_empty() {
                break;
            }
        }
        for l in lines.by_ref() {
            let l = l?;
            if !l.is_empty() {
                message = l;
                break;
            }
        }
        let (name, email) = match who.iter().rposition(|b| *b == b'<') {
            Some(at) => (
                who[..at].trim_ascii_end().to_vec(),
                who[at + 1..]
                    .strip_suffix(b">")
                    .unwrap_or(&who[at + 1..])
                    .to_vec(),
            ),
            None => (who.to_vec(), Vec::new()),
        };
        let mut key = name;
        if args.email {
            key.extend_from_slice(b" <");
            key.extend_from_slice(&email);
            key.push(b'>');
        }
        groups.entry(key).or_default().push(subject(&message));
    }
    Ok(())
}

/// The first paragraph as one line, without a leading `[PATCH ...]` tag.
fn subject(message: &[u8]) -> String {
    let text = String::from_utf8_lossy(message);
    let mut line = text
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    if line.starts_with("[PATCH")
        && let Some(end) = line.find(']')
    {
        line = line[end + 1..].trim_start().to_string();
    }
    if line.is_empty() {
        "<none>".to_string()
    } else {
        line
    }
}

fn render(groups: Groups, args: &ShortlogArgs) -> Vec<u8> {
    let mut list: Vec<(Vec<u8>, Vec<String>)> = groups.into_iter().collect();
    if args.numbered {
        list.sort_by_key(|g| std::cmp::Reverse(g.1.len()));
    }
    let mut out = Vec::new();
    for (key, subjects) in list {
        if args.summary {
            out.extend_from_slice(format!("{:6}\t", subjects.len()).as_bytes());
            out.extend_from_slice(&key);
            out.push(b'\n');
            continue;
        }
        out.extend_from_slice(&key);
        out.extend_from_slice(format!(" ({}):\n", subjects.len()).as_bytes());
        for s in subjects.iter().rev() {
            out.extend_from_slice(format!("      {s}\n").as_bytes());
        }
        out.push(b'\n');
    }
    out
}

//! `pyn ls-files` on a git repository: the index, and how the work tree differs from it.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::{Kind, Pathspec, fs_path, hash_worktree_file};

use super::GitHome;
use super::plumbing::{emit_bytes, prefix, quote, usage};
use super::stage::{Ignores, child, dir_entries};
use crate::short_status::relative;

/// Show the files in the index and the work tree.
#[derive(Debug, Args)]
pub struct LsFilesArgs {
    /// Show tracked files (the default).
    #[arg(short = 'c', long)]
    cached: bool,
    /// Show tracked files that are missing from the work tree.
    #[arg(short = 'd', long)]
    deleted: bool,
    /// Show tracked files that differ from the index, deleted ones included.
    #[arg(short = 'm', long)]
    modified: bool,
    /// Show files in the work tree that are not tracked.
    #[arg(short = 'o', long)]
    others: bool,
    /// Show mode, object id and stage number with each file.
    #[arg(short = 's', long)]
    stage: bool,
    /// Show only files with unresolved conflicts, with their stages.
    #[arg(short = 'u', long)]
    unmerged: bool,
    /// Show only the ignored files (needs --exclude-standard with -o or -c).
    #[arg(short = 'i', long)]
    ignored: bool,
    /// Hide files the standard ignore rules hide (with -o), or select them (with -i).
    #[arg(long)]
    exclude_standard: bool,
    /// Show paths from the repository root, not from the current folder.
    #[arg(long)]
    full_name: bool,
    /// Exit with 1 if a path names no tracked file.
    #[arg(long)]
    error_unmatch: bool,
    /// End each line with a NUL and do not quote names.
    #[arg(short = 'z')]
    nul: bool,
    /// Files and folders to limit the listing to; `*` and `?` are patterns.
    paths: Vec<String>,
}

pub(super) fn ls_files(git: &GitHome, cwd: &Path, args: LsFilesArgs) -> Result<()> {
    let root = git.repo.root()?.to_path_buf();
    if args.ignored && !args.exclude_standard {
        bail!("ls-files --ignored needs some exclude pattern");
    }
    if args.ignored && !args.others && !args.cached {
        bail!("ls-files -i must be used with either -o or -c");
    }
    if args.paths.iter().any(|p| p.starts_with(':')) {
        usage("pathspec magic is not supported by ls-files");
    }
    let pre = prefix(git, cwd)?;
    let specs: Vec<String> = args
        .paths
        .iter()
        .map(|p| git.ws.repo_prefix(cwd, p))
        .collect::<Result<_>>()?;
    let spec = if args.paths.is_empty() {
        Pathspec::new([pre.strip_suffix(b"/").unwrap_or(&pre).to_vec()])
    } else if specs.iter().any(String::is_empty) {
        Pathspec::default()
    } else {
        Pathspec::with_globs(specs.iter().map(|s| s.as_bytes().to_vec()))
    };
    let show_stage = args.stage || args.unmerged;
    let any = args.cached || args.deleted || args.modified || args.others || show_stage;
    let show_cached = args.cached || !any || show_stage;
    let index = git.repo.read_index()?;
    let ignores = Ignores {
        ignore: git.repo.ignore()?,
    };
    let mut out: Vec<u8> = Vec::new();
    let name = |out: &mut Vec<u8>, path: &[u8], stage: Option<(u32, String, u8)>| {
        let shown = if args.full_name {
            path.to_vec()
        } else {
            relative(pre.strip_suffix(b"/").unwrap_or(&pre), path)
        };
        if let Some((mode, id, st)) = stage {
            out.extend_from_slice(format!("{mode:06o} {id} {st}\t").as_bytes());
        }
        if args.nul {
            out.extend_from_slice(&shown);
            out.push(0);
        } else {
            out.extend_from_slice(quote(git, &shown).as_bytes());
            out.push(b'\n');
        }
    };
    let tracked: BTreeSet<&[u8]> = index.entries.iter().map(|e| e.path.as_slice()).collect();
    let mut matched: BTreeSet<Vec<u8>> = BTreeSet::new();

    if args.others {
        for path in untracked(git, &root, &index, &spec, &ignores, &args)? {
            name(&mut out, &path, None);
        }
    }
    let stage_of = |e: &pyn_git::IndexEntry| Some((e.mode, e.id.to_hex(), e.stage));
    let unstaged = if args.deleted || args.modified {
        git.repo.unstaged_changes(&index)?
    } else {
        Default::default()
    };
    for e in &index.entries {
        if !spec.matches(&e.path) {
            continue;
        }
        matched.insert(e.path.clone());
        if args.ignored && !ignores.hides(git, &e.path, false)? {
            continue;
        }
        if args.unmerged && e.stage == 0 {
            continue;
        }
        if show_cached {
            name(&mut out, &e.path, show_stage.then(|| stage_of(e)).flatten());
        }
        if args.deleted || args.modified {
            let kind = if e.stage > 0 {
                stage_state(&root, e, git)?
            } else {
                unstaged.get(&e.path).copied()
            };
            let deleted = kind == Some(Kind::Deleted);
            let changed = matches!(kind, Some(Kind::Modified | Kind::TypeChange));
            for shown in [
                args.deleted && deleted,
                args.modified && (changed || deleted),
            ] {
                if shown {
                    name(&mut out, &e.path, show_stage.then(|| stage_of(e)).flatten());
                }
            }
        }
    }
    emit_bytes(&out);
    if args.error_unmatch {
        for (arg, spec_path) in args.paths.iter().zip(&specs) {
            let one = Pathspec::with_globs([spec_path.as_bytes().to_vec()]);
            if !tracked.iter().any(|p| one.matches(p)) {
                eprintln!(
                    "error: pathspec '{arg}' did not match any file(s) known to git\n\
                     Did you forget to 'git add'?"
                );
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

fn stage_state(root: &Path, e: &pyn_git::IndexEntry, git: &GitHome) -> Result<Option<Kind>> {
    let path = fs_path(root, &e.path);
    if std::fs::symlink_metadata(&path).is_err() {
        return Ok(Some(Kind::Deleted));
    }
    let (_, id) = hash_worktree_file(git.algorithm(), &path)?;
    Ok((id != e.id).then_some(Kind::Modified))
}

/// Files in the work tree that the index does not track, by path.
fn untracked(
    git: &GitHome,
    root: &Path,
    index: &pyn_git::Index,
    spec: &Pathspec,
    ignores: &Ignores,
    args: &LsFilesArgs,
) -> Result<Vec<Vec<u8>>> {
    let mut files: BTreeSet<&[u8]> = BTreeSet::new();
    let mut dirs: BTreeSet<Vec<u8>> = BTreeSet::new();
    for e in &index.entries {
        files.insert(&e.path);
        let mut end = 0;
        while let Some(i) = e.path[end..].iter().position(|b| *b == b'/') {
            end += i;
            dirs.insert(e.path[..end].to_vec());
            end += 1;
        }
    }
    let mut found = Vec::new();
    let mut stack = vec![Vec::new()];
    while let Some(dir) = stack.pop() {
        for (name, kind) in dir_entries(&fs_path(root, &dir))? {
            if name == b".git" {
                continue;
            }
            let path = child(&dir, &name);
            if kind.is_dir() {
                if files.contains(path.as_slice()) || !spec.may_contain(&path) {
                    continue;
                }
                let nested = !dirs.contains(&path) && fs_path(root, &path).join(".git").exists();
                if nested {
                    let mut shown = path.clone();
                    shown.push(b'/');
                    if spec.matches(&shown) && visible(git, ignores, &path, true, args)? {
                        found.push(shown);
                    }
                } else if args.ignored
                    || !(args.exclude_standard && ignores.hides(git, &path, true)?)
                {
                    stack.push(path);
                }
            } else if !files.contains(path.as_slice())
                && spec.matches(&path)
                && visible(git, ignores, &path, false, args)?
            {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn visible(
    git: &GitHome,
    ignores: &Ignores,
    path: &[u8],
    is_dir: bool,
    args: &LsFilesArgs,
) -> Result<bool> {
    if !args.exclude_standard {
        return Ok(true);
    }
    Ok(ignores.hides(git, path, is_dir)? == args.ignored)
}

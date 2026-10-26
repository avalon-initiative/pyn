//! `pyn convert --home git`: moves a pyn server workspace to a git home with its history (decision #574).
//! The new `.git` is built beside the target and renamed into place only when complete.

use std::collections::{BTreeMap, HashSet};
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use pyn_git::{ConfigFile, Index, IndexEntry};
use pyn_gitwork::Repo;
use pyn_object::{Commit, FileMode, Kind, ObjectId, Tag, hash_object, parse_tree};
use pyn_proto as api;

use super::GitHome;
use super::stage::{raw_mode, stat_fields};
use crate::backend::Backend;
use crate::branch;
use crate::local_store::{ALGORITHM, LocalStore, parse_id};
use crate::local_sync::verified;
use crate::merging;
use crate::workspace::Workspace;

const TEMP: &str = ".git.pyn-convert";
const MAX_PAGES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Home {
    Git,
    Pyn,
}

/// Options of `pyn convert`.
#[derive(Debug, Args, Default, PartialEq, Eq)]
pub struct ConvertArgs {
    /// Move the repository to this home, keeping its history: `git` from a pyn server workspace, `pyn` from a git
    /// repository to the server named by `--server` and `--repo`.
    #[arg(long, value_enum)]
    pub home: Option<Home>,
    /// Do not ask before `--home git` or `--home pyn` converts.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// With `--home pyn`: make paths matching this glob exclusive in the starter policy (repeatable).
    #[arg(long, value_name = "GLOB", requires = "home")]
    pub exclusive: Vec<String>,
}

/// Removes the temporary directory unless the conversion finished.
struct Cleanup(PathBuf, bool);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.1 {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub(super) fn fatal(message: &str) -> ! {
    eprintln!("fatal: {message}");
    std::process::exit(128);
}

fn confirm(ws: &Workspace, yes: bool) {
    eprintln!(
        "This turns the pyn server workspace at {} into a git repository with the full history of every branch\n\
         (commits, authors, committers, times, parents, messages, signatures, trees and files). Object ids stay\n\
         SHA-256, as on the server, so signatures stay valid. The server is not changed.\n\
         What is lost: locks (a lock held on the server stays there and no longer applies here), revision numbers,\n\
         the shared and exclusive modes (`.pyn/pyn.toml` stays as local state a git repository ignores), changes\n\
         staged with `pyn add`, and the record of which revision each file is at. Files on disk are left as they\n\
         are; changes not yet committed show as git changes.",
        ws.root.display()
    );
    if yes {
        return;
    }
    if !std::io::stdin().is_terminal() {
        fatal(
            "`pyn convert --home git` needs confirmation but there is no terminal to ask on; pass --yes",
        );
    }
    eprint!("Continue? [y/N] ");
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
        eprintln!("aborted; nothing was changed");
        std::process::exit(1);
    }
}

fn reach(repo: &Repo, from: &[ObjectId]) -> Result<HashSet<ObjectId>> {
    let mut known = HashSet::new();
    let mut queue = from.to_vec();
    while let Some(id) = queue.pop() {
        if known.insert(id) {
            queue.extend(repo.commit(&id)?.parents);
        }
    }
    Ok(known)
}

fn read_commit(body: &[u8], id: &ObjectId) -> Result<Commit> {
    Ok(Commit::parse_lenient(ALGORITHM, body)
        .with_context(|| format!("commit {id} cannot be read"))?
        .object)
}

/// Fetches every branch through the fetch route; returns the branch heads.
fn fetch_all(backend: &dyn Backend, target: &Repo) -> Result<BTreeMap<String, ObjectId>> {
    let objects = target.objects();
    let mut heads: BTreeMap<String, ObjectId> = BTreeMap::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let page = backend.fetch_page(&api::FetchRequest {
            branches: Vec::new(),
            haves: Vec::new(),
            cursor: cursor.take(),
            limit: None,
        })?;
        for r in &page.refs {
            if let Some(head) = &r.head {
                heads.entry(r.branch.clone()).or_insert(parse_id(head)?);
            }
        }
        let mut checked = Vec::new();
        for c in &page.commits {
            let (id, body) = verified(Kind::Commit, &c.id, &c.data)?;
            let trees = c
                .trees
                .iter()
                .map(|t| verified(Kind::Tree, &t.id, &t.data))
                .collect::<Result<Vec<_>>>()?;
            checked.push((id, body, trees));
        }
        for (id, body, trees) in checked {
            for (_, bytes) in trees {
                objects.write(Kind::Tree, &bytes)?;
            }
            let record = read_commit(&body, &id)?;
            if !objects.contains(&record.tree)? {
                bail!("commit {id} came without its tree {}", record.tree);
            }
            if let Some(p) = record
                .parents
                .iter()
                .find(|p| !objects.contains(p).unwrap_or(false))
            {
                bail!("commit {id} came without its parent {p}");
            }
            objects.write(Kind::Commit, &body)?;
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => {
                for (branch, head) in &heads {
                    if !objects.contains(head)? {
                        bail!(
                            "the server's head {head} of '{branch}' was not among the commits it sent"
                        );
                    }
                }
                return Ok(heads);
            }
        }
    }
    bail!("the server sent more than {MAX_PAGES} pages in one fetch; refusing to continue")
}

/// Copies a tree and its subtrees from `from` into `to`; blobs are not copied.
fn copy_tree(from: &Repo, to: &Repo, id: &ObjectId) -> Result<()> {
    let mut stack = vec![*id];
    while let Some(id) = stack.pop() {
        if to.objects().contains(&id)? {
            continue;
        }
        let object = from.objects().read(&id)?;
        for e in parse_tree(ALGORITHM, &object.data)? {
            if e.mode.is_tree() {
                stack.push(e.id);
            }
        }
        to.objects().write(Kind::Tree, &object.data)?;
    }
    Ok(())
}

/// Copies a commit and its missing ancestors, with their trees, from the local store.
fn copy_commits(from: &Repo, to: &Repo, tip: &ObjectId) -> Result<usize> {
    let (mut stack, mut copied) = (vec![*tip], 0);
    while let Some(id) = stack.pop() {
        if to.objects().contains(&id)? {
            continue;
        }
        let object = from.objects().read(&id)?;
        let record = read_commit(&object.data, &id)?;
        copy_tree(from, to, &record.tree)?;
        stack.extend(record.parents.iter().copied());
        to.objects().write(Kind::Commit, &object.data)?;
        copied += 1;
    }
    Ok(copied)
}

struct Plan {
    heads: BTreeMap<String, ObjectId>,
    tags: Vec<(String, ObjectId)>,
    unpublished: Vec<String>,
}

/// Decides each branch's tip from the server and the local store, copying local-only commits and tags in.
fn merge_store(
    store: &LocalStore,
    target: &Repo,
    mut heads: BTreeMap<String, ObjectId>,
    current: &str,
) -> Result<Plan> {
    let from = store.repo();
    let mut unpublished = Vec::new();
    for (name, local) in from.refs().list("refs/heads/")? {
        let branch = name.trim_start_matches("refs/heads/").to_string();
        let copied = copy_commits(from, target, &local)?;
        match heads.get(&branch).copied() {
            None => {
                unpublished.push(format!("{branch} (a local branch, {copied} commits)"));
                heads.insert(branch, local);
            }
            Some(server) if server == local => {}
            Some(server) => {
                if reach(target, &[server])?.contains(&local) {
                    if branch == current {
                        eprintln!(
                            "warning: the workspace's branch '{branch}' is behind the server; the files on disk show as changes against the newer head"
                        );
                    }
                } else if reach(target, &[local])?.contains(&server) {
                    unpublished.push(format!("{branch} ({copied} unpublished local commits)"));
                    heads.insert(branch, local);
                } else {
                    bail!(
                        "branch '{branch}' has local commits and the server has others; run `pyn pull` or `pyn rebase` first. Nothing was changed"
                    );
                }
            }
        }
    }
    let mut tags = Vec::new();
    for (name, id) in from.refs().list("refs/tags/")? {
        let mut at = id;
        loop {
            let object = from.objects().read(&at)?;
            match object.kind {
                Kind::Tag => {
                    let tag = Tag::parse_lenient(ALGORITHM, &object.data)
                        .with_context(|| format!("tag {name} cannot be read"))?
                        .object;
                    target.objects().write(Kind::Tag, &object.data)?;
                    at = tag.object;
                }
                Kind::Commit => {
                    copy_commits(from, target, &at)?;
                    break;
                }
                other => bail!("tag {name} points at a {}, which is not kept", other.name()),
            }
        }
        tags.push((name, id));
    }
    Ok(Plan {
        heads,
        tags,
        unpublished,
    })
}

/// Writes every blob the commits reach that the target lacks, from the local store or the server, verified.
fn fill_blobs(
    backend: &dyn Backend,
    target: &Repo,
    store: Option<&LocalStore>,
    tips: &[ObjectId],
) -> Result<usize> {
    let commits = reach(target, tips)?;
    let (mut seen, mut blobs) = (HashSet::new(), HashSet::new());
    for c in &commits {
        let mut stack = vec![target.commit(c)?.tree];
        while let Some(tree) = stack.pop() {
            if !seen.insert(tree) {
                continue;
            }
            for e in target.tree(&tree)? {
                match e.mode {
                    FileMode::Tree => stack.push(e.id),
                    FileMode::Gitlink => bail!("a submodule entry cannot be converted"),
                    _ => {
                        blobs.insert(e.id);
                    }
                }
            }
        }
    }
    let mut fetched = 0;
    for id in &blobs {
        if target.objects().contains(id)? {
            continue;
        }
        let kept = store.and_then(|s| s.repo().objects().read_opt(id).ok().flatten());
        let body = match kept {
            Some(o) if o.kind == Kind::Blob => o.data,
            _ => {
                fetched += 1;
                backend.object(&id.to_hex())?
            }
        };
        if hash_object(ALGORITHM, Kind::Blob, &body)? != *id {
            bail!("file content {id} does not match its id; the server's answer is refused");
        }
        target.objects().write(Kind::Blob, &body)?;
    }
    Ok(fetched)
}

/// Writes the index from the HEAD tree; files whose content matches get their stat data, the rest stay modified.
fn write_index(git: &GitHome, head: &ObjectId) -> Result<usize> {
    let tree = git.repo.commit(head)?.tree;
    let files = git
        .repo
        .flatten(Some(&tree), &pyn_gitwork::Pathspec::default())?;
    let root = git.repo.root()?.to_path_buf();
    let mut index = Index::new(2);
    let mut writable = 0;
    for (path, entry) in &files {
        let mut e = IndexEntry::new(path.clone(), raw_mode(entry.mode), entry.id);
        let file = pyn_gitwork::fs_path(&root, path);
        if let Ok(meta) = std::fs::symlink_metadata(&file) {
            let bytes = if meta.file_type().is_symlink() {
                super::stage::link_target(&file).ok()
            } else {
                std::fs::read(&file).ok()
            };
            if bytes.is_some_and(|b| hash_object(ALGORITHM, Kind::Blob, &b).ok() == Some(entry.id))
            {
                stat_fields(&mut e, &meta);
            }
            #[cfg(unix)]
            if !meta.file_type().is_symlink() {
                use std::os::unix::fs::PermissionsExt;
                let mode = meta.permissions().mode();
                if mode & 0o200 == 0 {
                    let mut perms = meta.permissions();
                    perms.set_mode(mode | 0o200);
                    if std::fs::set_permissions(&file, perms).is_ok() {
                        writable += 1;
                    }
                }
            }
        }
        index.entries.push(e);
    }
    git.repo.git().write_index(&index)?;
    Ok(writable)
}

pub fn to_git(backend: &dyn Backend, ws: &Workspace, args: &ConvertArgs) -> Result<()> {
    let git_dir = ws.root.join(".git");
    if git_dir.exists() {
        fatal(&format!(
            "{} already exists; this folder is a git repository too. Nothing was changed",
            git_dir.display()
        ));
    }
    if merging::in_progress(ws) {
        fatal(
            "a merge is in progress; conclude it with `pyn commit` or give it up with `pyn merge --abort` first",
        );
    }
    confirm(ws, args.yes);

    let temp = ws.root.join(TEMP);
    let _ = std::fs::remove_dir_all(&temp);
    let mut guard = Cleanup(temp.clone(), false);
    let current = branch::name(Some(ws));
    GitHome::check_branch_name(&current)?;
    pyn_git::init(&temp, false, ALGORITHM, &current)?;
    let target = Repo::open(&temp, None, None)?;

    let heads = fetch_all(backend, &target)?;
    let store = LocalStore::open(ws);
    let plan = match &store {
        Some(s) => merge_store(s, &target, heads, &current)?,
        None => Plan {
            heads,
            tags: Vec::new(),
            unpublished: Vec::new(),
        },
    };
    let tips: Vec<ObjectId> = plan.heads.values().copied().collect();
    let commits = reach(&target, &tips)?.len();
    let downloaded = fill_blobs(backend, &target, store.as_ref(), &tips)?;
    let refs = target.refs();
    for (branch, id) in &plan.heads {
        refs.update(&format!("refs/heads/{branch}"), id)?;
    }
    for (name, id) in &plan.tags {
        refs.update(name, id)?;
    }
    refs.set_symbolic("HEAD", &format!("refs/heads/{current}"))?;
    drop(target);

    std::fs::rename(&temp, &git_dir)
        .with_context(|| format!("moving {} into place", temp.display()))?;
    guard.1 = true;
    let finish = || -> Result<usize> {
        let config = git_dir.join("config");
        let mut file = ConfigFile::read(&config)?;
        file.set("core", None, "bare", "false")?;
        file.write(&config)?;
        let git = GitHome::open_at(&ws.root)?;
        let writable = match plan.heads.get(&current) {
            Some(head) => write_index(&git, head)?,
            None => 0,
        };
        super::commands::hide_pyn(&git)?;
        Ok(writable)
    };
    let writable = match finish() {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&git_dir);
            return Err(e.context("the conversion failed; the workspace is as it was"));
        }
    };

    for sub in ["state", "store"] {
        let path = ws.dir.join("local_only").join(sub);
        if path.exists()
            && let Err(e) = std::fs::remove_dir_all(&path)
        {
            eprintln!("warning: could not remove {}: {e}", path.display());
        }
    }
    println!(
        "converted {} to a git repository: {} branches, {commits} commits, {downloaded} files downloaded, HEAD on '{current}'",
        ws.root.display(),
        plan.heads.len()
    );
    for u in &plan.unpublished {
        println!("kept unpublished local work: {u}");
    }
    if writable > 0 {
        println!("made {writable} read-only files writable (no locks apply in a git repository)");
    }
    Ok(())
}

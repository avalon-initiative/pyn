//! `pyn lfs` on a git repository: manage the paths `filter=lfs` applies to, and move and clear Git LFS objects.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use clap::{Args, Subcommand};
use pyn_gitwork::lfs::{Pointer, Store};
use pyn_gitwork::{Attributes, Pathspec, fs_path};
use pyn_object::{FileMode, ObjectId};

use super::GitHome;
use super::lfs_net::{fetch_missing, remote_or_default, upload_for_push};
use super::push::{commits_from, peel_commit};

const ATTRIBUTES: &str = ".gitattributes";
const ATTRS: &str = "filter=lfs diff=lfs merge=lfs -text";
const SPACE: &str = "[[:space:]]";
const DAY: i64 = 86_400;

/// Track paths with Git LFS, and fetch, push, list and prune its objects.
#[derive(Debug, Args)]
pub struct LfsArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Send new paths through Git LFS by adding them to the root `.gitattributes`; without patterns, list the tracked ones.
    Track { patterns: Vec<String> },
    /// Stop sending patterns through Git LFS by removing them from the root `.gitattributes`.
    Untrack {
        #[arg(required = true)]
        patterns: Vec<String>,
    },
    /// List the Git LFS pointers in the index; `*` marks an object in the local store and `-` one that is not.
    LsFiles(LsFilesArgs),
    /// Download the objects a revision (default `HEAD`) needs and the local store lacks.
    Fetch(FetchArgs),
    /// Fetch the objects `HEAD` needs, then replace the pointer files in the work tree with them.
    Pull {
        /// The remote to download from (default `origin`, else the only remote).
        remote: Option<String>,
    },
    /// Offer the objects of every commit reachable from the revisions (default `HEAD`) and upload those the server lacks.
    Push(FetchArgs),
    /// Delete local objects that no recent or unpushed commit, `HEAD` or the index needs.
    Prune(PruneArgs),
}

#[derive(Debug, Args)]
struct LsFilesArgs {
    /// Show the whole object id.
    #[arg(short, long)]
    long: bool,
    /// Show only the paths.
    #[arg(short, long)]
    name_only: bool,
}

#[derive(Debug, Args)]
struct FetchArgs {
    /// The remote (default `origin`, else the only remote).
    remote: Option<String>,
    /// The revisions to use (default `HEAD`).
    revs: Vec<String>,
}

#[derive(Debug, Args)]
struct PruneArgs {
    /// List what would be deleted and delete nothing.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// List each object deleted.
    #[arg(short, long)]
    verbose: bool,
    /// Keep what commits from the last this many days need.
    #[arg(long, default_value_t = 7)]
    days: u32,
}

pub(super) fn lfs(git: &GitHome, args: LfsArgs) -> Result<()> {
    match args.action {
        Action::Track { patterns } if patterns.is_empty() => list_tracked(git),
        Action::Track { patterns } => track(git, &patterns),
        Action::Untrack { patterns } => untrack(git, &patterns),
        Action::LsFiles(a) => ls_files(git, &a),
        Action::Fetch(a) => fetch(git, &a),
        Action::Pull { remote } => pull(git, remote.as_deref()),
        Action::Push(a) => push(git, &a),
        Action::Prune(a) => prune(git, &a),
    }
}

fn attributes_path(git: &GitHome) -> Result<std::path::PathBuf> {
    Ok(git.repo.root()?.join(ATTRIBUTES))
}

fn read_attributes(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The pattern of a `.gitattributes` line that sets `filter=lfs`.
fn lfs_pattern(line: &str) -> Option<String> {
    let mut words = line.split_whitespace();
    let pattern = words.next().filter(|p| !p.starts_with('#'))?;
    words
        .any(|w| w == "filter=lfs")
        .then(|| pattern.replace(SPACE, " "))
}

fn list_tracked(git: &GitHome) -> Result<()> {
    let text = read_attributes(&attributes_path(git)?)?;
    println!("Listing tracked patterns");
    for pattern in text.lines().filter_map(lfs_pattern) {
        println!("    {pattern} ({ATTRIBUTES})");
    }
    Ok(())
}

fn track(git: &GitHome, patterns: &[String]) -> Result<()> {
    let path = attributes_path(git)?;
    let mut text = read_attributes(&path)?;
    let known: HashSet<String> = text.lines().filter_map(lfs_pattern).collect();
    for pattern in patterns {
        if pattern.is_empty() || pattern.contains(['\n', '\r']) {
            bail!("'{pattern}' is not a usable pattern");
        }
        if known.contains(pattern) {
            println!("\"{pattern}\" already supported");
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("{} {ATTRS}\n", pattern.replace(' ', SPACE)));
        println!("Tracking \"{pattern}\"");
    }
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
}

fn untrack(git: &GitHome, patterns: &[String]) -> Result<()> {
    let path = attributes_path(git)?;
    let text = read_attributes(&path)?;
    let mut kept = String::new();
    let mut removed = HashSet::new();
    for line in text.lines() {
        match lfs_pattern(line) {
            Some(p) if patterns.contains(&p) => {
                removed.insert(p);
            }
            _ => {
                kept.push_str(line);
                kept.push('\n');
            }
        }
    }
    for pattern in patterns.iter().filter(|p| removed.contains(*p)) {
        println!("Untracking \"{pattern}\"");
    }
    if removed.is_empty() {
        return Ok(());
    }
    std::fs::write(&path, kept).with_context(|| format!("writing {}", path.display()))
}

/// Finds pointers: a blob is read only when `filter=lfs` applies to its path.
struct Scan {
    attrs: Arc<Attributes>,
    blobs: HashMap<ObjectId, Option<Pointer>>,
    trees: HashSet<ObjectId>,
}

impl Scan {
    fn new(git: &GitHome) -> Result<Self> {
        Ok(Self {
            attrs: git.repo.attributes()?,
            blobs: HashMap::new(),
            trees: HashSet::new(),
        })
    }

    fn pointer(&mut self, git: &GitHome, path: &[u8], id: &ObjectId) -> Result<Option<Pointer>> {
        if self.attrs.for_path(path).get("filter").as_str() != Some("lfs") {
            return Ok(None);
        }
        if let Some(known) = self.blobs.get(id) {
            return Ok(known.clone());
        }
        let found = Pointer::parse(&git.repo.blob(id)?);
        self.blobs.insert(*id, found.clone());
        Ok(found)
    }

    fn tree(&mut self, git: &GitHome, tree: &ObjectId) -> Result<Vec<(Vec<u8>, Pointer)>> {
        let mut out = Vec::new();
        if !self.trees.insert(*tree) {
            return Ok(out);
        }
        for (path, e) in git.repo.flatten(Some(tree), &Pathspec::default())? {
            if matches!(e.mode, FileMode::Regular | FileMode::Executable)
                && let Some(p) = self.pointer(git, &path, &e.id)?
            {
                out.push((path, p));
            }
        }
        Ok(out)
    }

    fn index(&mut self, git: &GitHome) -> Result<Vec<(Vec<u8>, Pointer)>> {
        let mut out = Vec::new();
        for e in git.repo.read_index()?.entries {
            if e.stage == 0
                && e.mode & 0o170000 == 0o100000
                && let Some(p) = self.pointer(git, &e.path, &e.id)?
            {
                out.push((e.path, p));
            }
        }
        Ok(out)
    }
}

fn commit_tree(git: &GitHome, id: &ObjectId) -> Result<ObjectId> {
    let commit = peel_commit(git, id).context("not a commit")?;
    Ok(git.repo.commit(&commit)?.tree)
}

/// The pointers in the trees of `revs`, or of `HEAD` when none is given.
fn wanted(git: &GitHome, revs: &[String]) -> Result<Vec<(Vec<u8>, Pointer)>> {
    let mut scan = Scan::new(git)?;
    let mut out = Vec::new();
    if revs.is_empty()
        && let Some(head) = git.repo.head_commit()?
    {
        out.extend(scan.tree(git, &commit_tree(git, &head)?)?);
    }
    for rev in revs {
        let id = git.repo.resolve(rev)?;
        out.extend(scan.tree(git, &commit_tree(git, &id)?)?);
    }
    Ok(out)
}

fn ls_files(git: &GitHome, args: &LsFilesArgs) -> Result<()> {
    let store = git.repo.filters().lfs();
    for (path, p) in Scan::new(git)?.index(git)? {
        let shown = String::from_utf8_lossy(&path);
        if args.name_only {
            println!("{shown}");
            continue;
        }
        let oid = if args.long { &p.oid } else { &p.oid[..10] };
        let state = if store.has(&p) { '*' } else { '-' };
        println!("{oid} {state} {shown}");
    }
    Ok(())
}

fn fetch(git: &GitHome, args: &FetchArgs) -> Result<()> {
    let remote = remote_or_default(git, args.remote.as_deref())?;
    let n = fetch_missing(git, Some(&remote), &wanted(git, &args.revs)?)?;
    println!("fetched {n} Git LFS object(s) from {remote}");
    Ok(())
}

fn pull(git: &GitHome, remote: Option<&str>) -> Result<()> {
    let remote = remote_or_default(git, remote)?;
    let pointers = wanted(git, &[])?;
    fetch_missing(git, Some(&remote), &pointers)?;
    let store = git.repo.filters().lfs();
    let root = git.repo.root()?;
    let mut written = 0;
    for (path, p) in Scan::new(git)?.index(git)? {
        if !p.extensions.is_empty() || !store.has(&p) {
            continue;
        }
        let file = fs_path(root, &path);
        let Ok(current) = std::fs::read(&file) else {
            continue;
        };
        if Pointer::parse(&current).as_ref() != Some(&p) {
            continue;
        }
        if let Some(content) = store.get(&p)? {
            std::fs::write(&file, content)
                .with_context(|| format!("writing {}", file.display()))?;
            written += 1;
        }
    }
    println!("replaced {written} pointer file(s) with their objects");
    Ok(())
}

fn push(git: &GitHome, args: &FetchArgs) -> Result<()> {
    let remote = remote_or_default(git, args.remote.as_deref())?;
    let config = git.config();
    let url = pyn_git::Remote::read(config, &remote)
        .and_then(|r| r.push_targets().first().cloned())
        .with_context(|| format!("'{remote}' has no URL"))?;
    let mut tips = Vec::new();
    for rev in &args.revs {
        tips.push(peeled_commit(git, rev)?);
    }
    if args.revs.is_empty() {
        tips.extend(git.repo.head_commit()?);
    }
    let sent = upload_for_push(git, Some(&remote), &url, &tips, &[])?;
    println!("uploaded {sent} Git LFS object(s) to {remote}");
    Ok(())
}

fn peeled_commit(git: &GitHome, rev: &str) -> Result<ObjectId> {
    let id = git.repo.resolve(rev)?;
    peel_commit(git, &id).with_context(|| format!("'{rev}' is not a commit"))
}

/// Commits whose objects stay: `HEAD`, those from the last `days` days and those no remote-tracking ref reaches.
fn needed_commits(git: &GitHome, days: u32) -> Result<HashSet<ObjectId>> {
    let refs = git.repo.refs();
    let peeled = |prefix: &str| -> Result<Vec<ObjectId>> {
        Ok(refs
            .list(prefix)?
            .into_iter()
            .filter_map(|(_, id)| peel_commit(git, &id))
            .collect())
    };
    let mut local = peeled("refs/heads/")?;
    local.extend(peeled("refs/tags/")?);
    local.extend(git.repo.head_commit()?);
    let known = commits_from(git, &peeled("refs/remotes/")?, &HashSet::new());
    let mut keep = commits_from(git, &local, &known);
    keep.extend(git.repo.head_commit()?);

    let cutoff = chrono::Utc::now().timestamp() - i64::from(days) * DAY;
    let mut seen = HashSet::new();
    let mut queue = local;
    while let Some(id) = queue.pop() {
        if !seen.insert(id) {
            continue;
        }
        let commit = git.repo.commit(&id)?;
        if commit.committer.time >= cutoff {
            keep.insert(id);
            queue.extend(commit.parents);
        }
    }
    Ok(keep)
}

fn prune(git: &GitHome, args: &PruneArgs) -> Result<()> {
    let store: &Store = git.repo.filters().lfs();
    let mut scan = Scan::new(git)?;
    let mut keep: HashSet<String> = HashSet::new();
    keep.extend(scan.index(git)?.into_iter().map(|(_, p)| p.oid));
    for id in needed_commits(git, args.days)? {
        let tree = git.repo.commit(&id)?.tree;
        keep.extend(scan.tree(git, &tree)?.into_iter().map(|(_, p)| p.oid));
    }
    let (mut gone, mut kept, mut freed) = (0, 0, 0u64);
    for (oid, size) in store.list()? {
        if keep.contains(&oid) {
            kept += 1;
            continue;
        }
        if !args.dry_run {
            store.remove(&oid)?;
        }
        if args.verbose {
            println!("{oid} {size}");
        }
        gone += 1;
        freed += size;
    }
    let verb = if args.dry_run {
        "would prune"
    } else {
        "pruned"
    };
    println!("{verb} {gone} Git LFS object(s) ({freed} bytes), kept {kept}");
    Ok(())
}

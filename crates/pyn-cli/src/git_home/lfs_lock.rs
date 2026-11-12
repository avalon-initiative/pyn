//! Git LFS locking on a git repository: `lockable` files stay read-only until locked, `lock`, `unlock` and `locks`
//! against the LFS server's Locks API, and the pre-push check for files locked by someone else.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Utc};
use pyn_git::Remote;
use pyn_gittransport::credential::Prompt;
use pyn_gittransport::lfs::Client;
use pyn_gittransport::lfs_locks::Lock;
use pyn_gittransport::{Env, Error as TransportError};
use pyn_gitwork::{Head, Pathspec, fs_path};
use pyn_object::ObjectId;

use super::{GitHome, lfs_net};
use crate::policy::{POLICY_PATH, Policy};
use crate::{policy_lint, table, time};

/// A prompt that never asks: hooks run unattended, so only stored credentials count.
struct Silent;

impl Prompt for Silent {
    fn ask(&mut self, _prompt: &str, _secret: bool) -> pyn_gittransport::Result<String> {
        Err(TransportError::Credential("not interactive".into()))
    }
}

/// A ref a push updates and the commit it moves to.
pub(super) struct Pushed {
    pub name: String,
    pub new: ObjectId,
}

fn current_ref(git: &GitHome) -> Option<String> {
    match git.repo.head() {
        Ok(Head::Branch { name, .. }) => Some(format!("refs/heads/{name}")),
        _ => None,
    }
}

fn open_client(
    git: &GitHome,
    remote: Option<&str>,
    url: Option<&str>,
    interactive: bool,
) -> Result<Client> {
    let env = Env::process();
    let config = lfs_net::layered(&env, git.repo.git().work_tree(), git.config())?;
    let (remote, url) = match remote {
        Some(r) => (Some(r.to_owned()), url.map(str::to_owned)),
        None => {
            let name = lfs_net::default_remote(&config);
            let url = name
                .as_deref()
                .and_then(|r| Remote::read(&config, r))
                .and_then(|r| r.urls.first().cloned());
            (name, url)
        }
    };
    let operation = if interactive { "upload" } else { "download" };
    let prompt: Option<Box<dyn Prompt>> = (!interactive).then(|| Box::new(Silent) as _);
    lfs_net::client_for(
        &env,
        &config,
        remote.as_deref(),
        url.as_deref(),
        false,
        operation,
        prompt,
    )
}

/// Tracked paths with whether their `lockable` attribute is set.
fn tracked_lockable(git: &GitHome) -> Vec<(String, bool)> {
    let Ok(opened) = git.open_index() else {
        return Vec::new();
    };
    let Ok(attrs) = git.repo.attributes() else {
        return Vec::new();
    };
    opened
        .index
        .entries
        .iter()
        .filter(|e| e.stage == 0 && !e.skip_worktree)
        .map(|e| {
            (
                String::from_utf8_lossy(&e.path).into_owned(),
                attrs.for_path(&e.path).get("lockable").is_set(),
            )
        })
        .collect()
}

/// Tracked paths whose `lockable` attribute is set.
fn lockable_paths(git: &GitHome) -> Vec<String> {
    let mut all = tracked_lockable(git);
    all.retain(|(_, lockable)| *lockable);
    all.into_iter().map(|(p, _)| p).collect()
}

/// `pyn policy lint`: tracked paths where `lockable` and the policy's `exclusive` disagree. Always exits 0 when the lint runs.
pub fn lint_lockable(git: &GitHome, policy: Option<&Path>) -> Result<()> {
    let file = match policy {
        Some(p) => p.to_path_buf(),
        None => git.repo.root()?.join(POLICY_PATH),
    };
    let text = fs::read_to_string(&file)
        .with_context(|| format!("reading the policy {}", file.display()))?;
    let Some(parsed) = Policy::parse(&text) else {
        bail!("{} is not a valid policy file", file.display());
    };
    git.repo.reset_attributes();
    let tracked = tracked_lockable(git);
    let rows: Vec<Vec<String>> =
        policy_lint::lint(tracked.iter().map(|(p, l)| (p.as_str(), *l)), |p| {
            parsed.mode_for(p)
        })
        .into_iter()
        .map(|(path, d)| vec![d.label().to_owned(), path.to_owned()])
        .collect();
    table::show(&["MISMATCH", "PATH"], &rows, "lockable and exclusive agree");
    Ok(())
}

fn set_writable(root: &Path, path: &str, writable: bool) {
    let file = fs_path(root, path.as_bytes());
    let Ok(meta) = fs::symlink_metadata(&file) else {
        return;
    };
    if !meta.is_file() {
        return;
    }
    let mode = meta.permissions().mode();
    let wanted = if writable {
        mode | 0o200
    } else {
        mode & !0o222
    };
    if wanted != mode {
        let _ = fs::set_permissions(&file, fs::Permissions::from_mode(wanted));
    }
}

/// Makes every `lockable` file read-only, then writable again where the caller holds the lock.
/// Without a reachable server the files stay read-only.
pub(super) fn sync_readonly(git: &GitHome) {
    git.repo.reset_attributes();
    let lockable = lockable_paths(git);
    let Ok(root) = git.repo.root() else {
        return;
    };
    if lockable.is_empty() {
        return;
    }
    for p in &lockable {
        set_writable(root, p, false);
    }
    let ours = open_client(git, None, None, false)
        .and_then(|mut c| Ok(c.verify_locks(current_ref(git).as_deref())?));
    if let Ok(Some(v)) = ours {
        let known: HashSet<&String> = lockable.iter().collect();
        for l in v.ours.iter().filter(|l| known.contains(&l.path)) {
            set_writable(root, &l.path, true);
        }
    }
}

/// `pyn lock <path>`: takes the lock and makes a `lockable` file writable.
pub fn lock(git: &GitHome, cwd: &Path, arg: &str) -> Result<()> {
    let path = git.ws.repo_prefix(cwd, arg)?;
    let mut client = open_client(git, None, None, true)?;
    let held = client.create_lock(&path, current_ref(git).as_deref())?;
    set_writable(git.repo.root()?, &held.path, true);
    println!("locked {}", held.path);
    Ok(())
}

/// `pyn unlock <path>`: releases the lock and makes a `lockable` file read-only again; `force` removes another user's.
pub fn unlock(git: &GitHome, cwd: &Path, arg: &str, force: bool) -> Result<()> {
    let path = git.ws.repo_prefix(cwd, arg)?;
    let refname = current_ref(git);
    let mut client = open_client(git, None, None, true)?;
    let Some(found) = client
        .list_locks(Some(&path), None, refname.as_deref())?
        .into_iter()
        .find(|l| l.path == path)
    else {
        bail!("'{path}' is not locked");
    };
    let gone = client.unlock(&found.id, force, refname.as_deref())?;
    if lockable_paths(git).contains(&path) {
        set_writable(git.repo.root()?, &path, false);
    }
    if force {
        println!("removed {}'s lock on {path}", gone.owner.name);
    } else {
        println!("unlocked {path}");
    }
    Ok(())
}

fn when(locked_at: &str) -> String {
    DateTime::parse_from_rfc3339(locked_at).map_or_else(
        |_| locked_at.to_owned(),
        |t| time::local(t.with_timezone(&Utc)),
    )
}

/// `pyn locks`: every lock, or with `mine` the ones the caller holds.
pub fn locks(git: &GitHome, mine: bool) -> Result<()> {
    let refname = current_ref(git);
    let mut client = open_client(git, None, None, true)?;
    if mine {
        let Some(v) = client.verify_locks(refname.as_deref())? else {
            bail!("the LFS server does not support listing your locks");
        };
        let rows: Vec<Vec<String>> = v
            .ours
            .iter()
            .map(|l| vec![when(&l.locked_at), l.path.clone()])
            .collect();
        table::show(&["LOCKED", "PATH"], &rows, "no locks");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = client
        .list_locks(None, None, refname.as_deref())?
        .iter()
        .map(|l| vec![l.owner.name.clone(), when(&l.locked_at), l.path.clone()])
        .collect();
    table::show(&["OWNER", "LOCKED", "PATH"], &rows, "no locks");
    Ok(())
}

/// Paths a push of `new` adds to what `remote_tips` already have.
fn changed_paths(
    git: &GitHome,
    new: ObjectId,
    remote_tips: &[ObjectId],
) -> Result<HashSet<String>> {
    let db = git.repo.objects();
    let stop: HashSet<ObjectId> = remote_tips
        .iter()
        .filter(|id| db.contains(id).unwrap_or(false))
        .copied()
        .collect();
    let mut out = HashSet::new();
    for id in super::push::commits_from(git, &[new], &stop) {
        let commit = git.repo.commit(&id)?;
        let base = commit
            .parents
            .first()
            .and_then(|p| git.repo.commit(p).ok())
            .map(|c| c.tree);
        for c in git
            .repo
            .diff_trees(base.as_ref(), Some(&commit.tree), &Pathspec::default())?
        {
            out.insert(String::from_utf8_lossy(&c.path).into_owned());
        }
    }
    Ok(out)
}

/// Refuses a push that changes a file someone else has locked. Only repositories that mark files `lockable` are checked,
/// and `lfs.locksverify=false` turns it off. Prints the files and returns true to stop the push.
pub(super) fn push_blocked(
    git: &GitHome,
    remote: Option<&str>,
    url: &str,
    pushed: &[Pushed],
    remote_tips: &[ObjectId],
) -> Result<bool> {
    if pushed.is_empty() || lockable_paths(git).is_empty() {
        return Ok(false);
    }
    let env = Env::process();
    let config = lfs_net::layered(&env, git.repo.git().work_tree(), git.config())?;
    if config.get_bool("lfs.locksverify").ok().flatten() == Some(false) {
        return Ok(false);
    }
    let Ok(mut client) = open_client(git, remote, Some(url), true) else {
        return Ok(false);
    };
    let mut blocked: BTreeMap<String, Lock> = BTreeMap::new();
    for p in pushed {
        let changed = changed_paths(git, p.new, remote_tips)?;
        if changed.is_empty() {
            continue;
        }
        let Some(v) = client
            .verify_locks(Some(&p.name))
            .context("checking Git LFS locks")?
        else {
            return Ok(false);
        };
        for l in v.theirs.into_iter().filter(|l| changed.contains(&l.path)) {
            blocked.insert(l.path.clone(), l);
        }
    }
    if blocked.is_empty() {
        return Ok(false);
    }
    eprintln!("error: Unable to push locked files:");
    for l in blocked.values() {
        eprintln!("error: * {} - {}", l.path, l.owner.name);
    }
    eprintln!("error: Cannot update locked files.");
    Ok(true)
}

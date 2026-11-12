//! Moves Git LFS objects between the local store and an LFS server: downloads for a smudge, uploads before a push.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{Context as _, Result};
use pyn_git::{Config, Remote, Repository, remote_names};
use pyn_gittransport::credential::{Prompt, SystemPrompt};
use pyn_gittransport::lfs::{Client, Object};
use pyn_gittransport::lfs_ssh::resolve;
use pyn_gittransport::{Env, user_config};
use pyn_gitwork::Pathspec;
use pyn_gitwork::lfs::{Context, Pointer, Store, lfsconfig_layer};
use pyn_object::{FileMode, ObjectId};

use super::GitHome;

/// Lets a smudge download absent LFS objects.
pub fn install() {
    pyn_gitwork::lfs::set_downloader(download);
}

fn download(ctx: &Context, pointers: &[Pointer]) -> std::result::Result<(), String> {
    fetch(ctx, pointers).map_err(|e| format!("{e:#}"))
}

/// `.lfsconfig` (lowest), the user's configuration, then the repository's.
pub(super) fn layered(env: &Env, work_tree: Option<&Path>, repo: &Config) -> Result<Config> {
    let mut config = work_tree.map(lfsconfig_layer).unwrap_or_default();
    config.extend(user_config(env)?);
    config.extend(repo.clone());
    Ok(config)
}

/// `origin`, else the only remote.
pub(super) fn default_remote(config: &Config) -> Option<String> {
    if Remote::read(config, "origin").is_some() {
        return Some("origin".into());
    }
    match remote_names(config).as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    }
}

pub(super) fn client(
    env: &Env,
    config: &Config,
    remote: Option<&str>,
    url: Option<&str>,
    push: bool,
) -> Result<Client> {
    let operation = if push { "upload" } else { "download" };
    client_for(env, config, remote, url, push, operation, None)
}

/// `operation` is what an ssh remote's `git-lfs-authenticate` is asked for; `prompt` defaults to the system one.
pub(super) fn client_for(
    env: &Env,
    config: &Config,
    remote: Option<&str>,
    url: Option<&str>,
    push: bool,
    operation: &str,
    prompt: Option<Box<dyn Prompt>>,
) -> Result<Client> {
    let resolved = resolve(config, env, remote, url, push, operation)?;
    let prompt = prompt.unwrap_or_else(|| Box::new(SystemPrompt::new(env, config)));
    Ok(Client::from_resolved(resolved, config, env, prompt)?)
}

fn to_objects<'a>(pointers: impl IntoIterator<Item = &'a Pointer>) -> Vec<Object> {
    pointers
        .into_iter()
        .map(|p| Object {
            oid: p.oid.clone(),
            size: p.size,
        })
        .collect()
}

fn fetch(ctx: &Context, pointers: &[Pointer]) -> Result<()> {
    fetch_from(ctx, None, pointers)
}

fn fetch_from(ctx: &Context, named: Option<&str>, pointers: &[Pointer]) -> Result<()> {
    let env = Env::process();
    let repo = Repository::open(&ctx.common_dir)?;
    let config = layered(&env, ctx.work_tree.as_deref(), repo.config())?;
    let remote = named.map(str::to_owned).or_else(|| default_remote(&config));
    let url = remote
        .as_deref()
        .and_then(|r| Remote::read(&config, r))
        .and_then(|r| r.urls.first().cloned());
    let mut client = client(&env, &config, remote.as_deref(), url.as_deref(), false)?;
    let store = Store::new(
        ctx.work_tree.as_deref(),
        &ctx.common_dir,
        ctx.options.clone(),
    );
    client.download(&to_objects(pointers), &store.tmp_dir(), &mut |o, path| {
        store
            .adopt(&o.oid, path)
            .map_err(|e| pyn_gittransport::Error::Lfs(e.to_string()))
    })?;
    Ok(())
}

/// Downloads the objects of `wanted` that the store lacks and the fetch include and exclude settings allow, from
/// `remote` (default `origin`, else the only remote). Returns how many were requested.
pub(super) fn fetch_missing(
    git: &GitHome,
    remote: Option<&str>,
    wanted: &[(Vec<u8>, Pointer)],
) -> Result<usize> {
    let store = git.repo.filters().lfs();
    let ctx = store.context();
    let mut seen = HashSet::new();
    let missing: Vec<Pointer> = wanted
        .iter()
        .filter(|(path, p)| p.extensions.is_empty() && !store.has(p) && ctx.options.allows(path))
        .filter(|(_, p)| seen.insert(p.oid.clone()))
        .map(|(_, p)| p.clone())
        .collect();
    if !missing.is_empty() {
        fetch_from(ctx, remote, &missing)?;
    }
    Ok(missing.len())
}

/// The remote's name, defaulting like a download; it must be configured.
pub(super) fn remote_or_default(git: &GitHome, named: Option<&str>) -> Result<String> {
    let config = layered(&Env::process(), git.repo.git().work_tree(), git.config())?;
    let name = match named {
        Some(n) => n.to_owned(),
        None => {
            default_remote(&config).context("no remote to use: name one, or set up `origin`")?
        }
    };
    if Remote::read(&config, &name).is_none() {
        anyhow::bail!("'{name}' is not a configured remote");
    }
    Ok(name)
}

/// The pointers a push of `tips` adds to `remote_tips`: those in blobs a new commit changes.
fn new_pointers(
    git: &GitHome,
    tips: &[ObjectId],
    remote_tips: &[ObjectId],
) -> Result<BTreeMap<String, Pointer>> {
    let db = git.repo.objects();
    let stop: HashSet<ObjectId> = remote_tips
        .iter()
        .filter(|id| db.contains(id).unwrap_or(false))
        .copied()
        .collect();
    let mut found = BTreeMap::new();
    for id in super::push::commits_from(git, tips, &stop) {
        let commit = git.repo.commit(&id)?;
        let base = commit
            .parents
            .first()
            .and_then(|p| git.repo.commit(p).ok())
            .map(|c| c.tree);
        let changes =
            git.repo
                .diff_trees(base.as_ref(), Some(&commit.tree), &Pathspec::default())?;
        for entry in changes.into_iter().filter_map(|c| c.new) {
            if !matches!(entry.mode, FileMode::Regular | FileMode::Executable) {
                continue;
            }
            if let Some(p) = Pointer::parse(&git.repo.blob(&entry.id)?) {
                found.insert(p.oid.clone(), p);
            }
        }
    }
    Ok(found)
}

/// Uploads the LFS objects that commits reachable from `tips` and not from `remote_tips` point at, so the server has
/// them before the commits arrive. Returns how many objects were sent.
pub(super) fn upload_for_push(
    git: &GitHome,
    remote: Option<&str>,
    url: &str,
    tips: &[ObjectId],
    remote_tips: &[ObjectId],
) -> Result<usize> {
    let store = git.repo.filters().lfs();
    if !store.exists() {
        return Ok(0);
    }
    let pointers = new_pointers(git, tips, remote_tips)?;
    if pointers.is_empty() {
        return Ok(0);
    }
    let env = Env::process();
    let config = layered(&env, git.repo.git().work_tree(), git.config())?;
    let mut client = client(&env, &config, remote, Some(url), true)?;
    client
        .upload(&to_objects(pointers.values()), &|o| {
            std::fs::File::open(store.object_path(&o.oid))
                .map_err(|e| std::io::Error::new(e.kind(), "the object is not in the local store"))
        })
        .context("uploading Git LFS objects")
}

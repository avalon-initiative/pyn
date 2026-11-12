//! Guards for the conversion pyn does not apply when git would write a work tree file: `working-tree-encoding`
//! (#273). Line endings, `ident` and filters are converted by `Repo::conversion`.

use std::collections::BTreeMap;

use anyhow::{Result, bail};
use pyn_gitwork::{Attributes, Pathspec, ToGit};
use pyn_object::{FileMode, ObjectId};

use super::strategy::Settings;
use super::{GitHome, path_string};

fn skipped(mode: FileMode) -> bool {
    matches!(mode, FileMode::Symlink | FileMode::Gitlink | FileMode::Tree)
}

/// Fails, naming the first path whose encoding git would apply when writing it.
pub(super) fn refuse_unsupported<'a>(
    git: &GitHome,
    files: impl IntoIterator<Item = (&'a [u8], FileMode)>,
) -> Result<()> {
    let mut seen: BTreeMap<Vec<u8>, ()> = BTreeMap::new();
    for (path, mode) in files {
        if skipped(mode) || seen.insert(path.to_vec(), ()).is_some() {
            continue;
        }
        if let Some(u) = git.repo.conversion(path)?.unsupported() {
            bail!(
                "git would convert '{}' when writing it ({u}), and pyn cannot do that here yet (#273); \
                 nothing was changed",
                path_string(path)
            );
        }
    }
    Ok(())
}

/// `-X renormalize`: a side's content as git's merge sees it, converted to the work tree form and back.
pub(super) fn renormalize(
    git: &GitHome,
    settings: &Settings,
    path: &[u8],
    data: &[u8],
) -> Result<Vec<u8>> {
    let conv = match &settings.attrs {
        Some(a) => git.repo.conversion_under(&a.0, path),
        None => git.repo.conversion(path)?,
    };
    if let Some(u) = conv.unsupported() {
        bail!(
            "`-X renormalize` would convert '{}' ({u}), and pyn cannot do that here yet (#273); nothing was changed",
            path_string(path)
        );
    }
    if conv.is_identity() {
        return Ok(data.to_vec());
    }
    let worktree = conv.to_worktree(git.repo.algorithm(), path, data)?;
    let opts = ToGit {
        check_roundtrip: false,
        renormalize: true,
        index_blob: &|| None,
    };
    Ok(conv.to_git(path, &worktree, &opts)?.data.into_owned())
}

/// Warns if `filter=lfs` applies to a path of `commit`'s tree whose pointer names an object that is not stored.
pub(super) fn warn_lfs_in_commit(git: &GitHome, commit: &ObjectId) -> Result<()> {
    if pyn_gitwork::lfs::pointers_warned() || git.repo.objects().is_promisor() {
        return Ok(());
    }
    let Ok(commit) = git.repo.commit(commit) else {
        return Ok(());
    };
    let files = git.repo.flatten(Some(&commit.tree), &Pathspec::default())?;
    let mut attribute_files: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    for (path, entry) in &files {
        let name = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
        if name == b".gitattributes" && !skipped(entry.mode) {
            let dir = path[..path.len().saturating_sub(name.len() + 1)].to_vec();
            attribute_files.insert(dir, git.repo.blob(&entry.id)?);
        }
    }
    if !attribute_files
        .values()
        .any(|text| String::from_utf8_lossy(text).contains("filter=lfs"))
    {
        return Ok(());
    }
    let attrs = Attributes::new(
        b"",
        b"",
        Box::new(move |dir| attribute_files.get(dir).cloned()),
        false,
    );
    for (path, entry) in &files {
        if skipped(entry.mode) || attrs.for_path(path).get("filter").as_str() != Some("lfs") {
            continue;
        }
        let missing = pyn_gitwork::lfs::Pointer::parse(&git.repo.blob(&entry.id)?)
            .is_some_and(|p| !git.repo.filters().lfs().object_path(&p.oid).exists());
        if missing {
            pyn_gitwork::lfs::warn_pointers_kept(None);
            break;
        }
    }
    Ok(())
}

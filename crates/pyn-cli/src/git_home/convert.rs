//! Guards for the conversions pyn does not apply when git would write a work tree file: filter drivers and
//! `working-tree-encoding` (#273). Line endings and `ident` are converted by `Repo::conversion`.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};
use pyn_gitwork::{Attributes, Pathspec};
use pyn_object::{FileMode, ObjectId};

use super::{GitHome, path_string};

fn skipped(mode: FileMode) -> bool {
    matches!(mode, FileMode::Symlink | FileMode::Gitlink | FileMode::Tree)
}

/// Fails, naming the first path whose filter or encoding git would apply when writing it; warns once about paths a
/// Git LFS filter owns.
pub(super) fn refuse_unsupported<'a>(
    git: &GitHome,
    files: impl IntoIterator<Item = (&'a [u8], FileMode)>,
) -> Result<()> {
    check(git, files, false)
}

/// Like [`refuse_unsupported`], and also refuses any path whose line endings or `ident` git would convert, for
/// writers that copy bytes as they are.
pub(super) fn refuse_conversions<'a>(
    git: &GitHome,
    files: impl IntoIterator<Item = (&'a [u8], FileMode)>,
) -> Result<()> {
    check(git, files, true)
}

fn check<'a>(
    git: &GitHome,
    files: impl IntoIterator<Item = (&'a [u8], FileMode)>,
    raw_writer: bool,
) -> Result<()> {
    let mut seen: BTreeMap<Vec<u8>, ()> = BTreeMap::new();
    let mut lfs = false;
    for (path, mode) in files {
        if skipped(mode) || seen.insert(path.to_vec(), ()).is_some() {
            continue;
        }
        let conv = git.repo.conversion(path)?;
        lfs |= conv.is_lfs();
        let reason = match conv.unsupported() {
            Some(u) => Some(u.to_string()),
            None if raw_writer && !conv.is_identity() => {
                Some("line endings or ident would be converted".to_string())
            }
            None => None,
        };
        if let Some(reason) = reason {
            bail!(
                "git would convert '{}' when writing it ({reason}), and pyn cannot do that here yet (#273); \
                 nothing was changed",
                path_string(path)
            );
        }
    }
    if lfs {
        warn_lfs();
    }
    Ok(())
}

/// Fails when `-X renormalize` is asked for: it normalizes line endings and runs filters on the way into the merge,
/// which pyn does not do for the paths that have a conversion.
pub(super) fn refuse_renormalize(git: &GitHome, path: &[u8]) -> Result<()> {
    let conv = git.repo.conversion(path)?;
    if !conv.is_identity() || conv.unsupported().is_some() {
        bail!(
            "`-X renormalize` would convert '{}' on its way into the merge, and pyn does not do that yet (#273); \
             nothing was changed",
            path_string(path)
        );
    }
    Ok(())
}

static LFS_WARNED: AtomicBool = AtomicBool::new(false);

/// Prints the Git LFS warning the first time it is asked for in this process.
fn warn_lfs() {
    if !LFS_WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "warning: some paths use the Git LFS filter; pyn has no LFS client, so pointer files are kept as they are"
        );
    }
}

/// Warns if the `.gitattributes` files in `commit`'s tree apply `filter=lfs` to any of its paths.
pub(super) fn warn_lfs_in_commit(git: &GitHome, commit: &ObjectId) -> Result<()> {
    if LFS_WARNED.load(Ordering::Relaxed) || git.repo.objects().is_promisor() {
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
        if !skipped(entry.mode) && attrs.for_path(path).get("filter").as_str() == Some("lfs") {
            warn_lfs();
            break;
        }
    }
    Ok(())
}

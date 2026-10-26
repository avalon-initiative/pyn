//! The three-way fallback of `am`: a base rebuilt from the blobs the patch's `index` lines name, the patch applied
//! to it in memory, and the two trees merged with the current one.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use pyn_gitwork::Entry;
use pyn_object::{FileMode, Kind};

use super::GitHome;
use super::apply::{Slot, parse_patches, patched};
use super::merge::{Landing, apply_merge};
use super::stage::{leaf_mode, raw_mode, write_leaves, write_tree};
use super::strategy::Settings;
use super::treemerge::{Labels, merge_trees};

pub(super) enum Outcome {
    Clean,
    Conflicted,
    /// The patch cannot be merged (no blobs to rebuild a base); the reasons were printed.
    Unusable,
}

fn unusable(name: &str) -> Outcome {
    eprintln!("error: sha1 information is lacking or useless ({name}).");
    eprintln!("error: could not build fake ancestor");
    Outcome::Unusable
}

pub(super) fn fall_back(git: &GitHome, patch: &[u8], subject: &str) -> Result<Outcome> {
    println!("Using index info to reconstruct a base tree...");
    let Ok(patches) = parse_patches(patch, 1) else {
        return Ok(Outcome::Unusable);
    };
    let mut base: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
    for p in &patches {
        let (true, Some(old)) = (p.is_new <= 0, &p.old_name) else {
            continue;
        };
        let shown = String::from_utf8_lossy(old).into_owned();
        let named = p
            .old_id
            .as_ref()
            .map(|prefix| git.repo.ids_with_prefix(prefix))
            .transpose()?
            .and_then(|found| match found.as_slice() {
                [id] if git.repo.blob(id).is_ok() => Some(*id),
                _ => None,
            });
        let id = match named {
            Some(id) => id,
            None if p.added + p.deleted == 0 => {
                let opened = git.open_index()?;
                let Some(entry) = opened
                    .index
                    .entries
                    .iter()
                    .find(|e| e.stage == 0 && e.path == *old)
                else {
                    eprintln!("error: mode change for {shown}, which is not in current HEAD");
                    eprintln!("error: could not build fake ancestor");
                    return Ok(Outcome::Unusable);
                };
                entry.id
            }
            None => return Ok(unusable(&shown)),
        };
        let mode = if p.old_mode == 0 {
            FileMode::Regular
        } else {
            leaf_mode(p.old_mode)
        };
        base.insert(old.clone(), Entry { mode, id });
    }
    let root = git.repo.root()?;
    for (path, entry) in &base {
        let file = pyn_gitwork::fs_path(root, path);
        let name = String::from_utf8_lossy(path);
        match std::fs::read(&file) {
            Err(_) => println!("D\t{name}"),
            Ok(bytes) if bytes != git.repo.blob(&entry.id)? => println!("M\t{name}"),
            Ok(_) if exec_bit(&file) != (entry.mode == FileMode::Executable) => {
                println!("M\t{name}")
            }
            Ok(_) => {}
        }
    }
    println!("Falling back to patching base and 3-way merge...");
    let mut slots: HashMap<Vec<u8>, Slot> = HashMap::new();
    for (path, entry) in &base {
        slots.insert(
            path.clone(),
            Slot {
                mode: raw_mode(entry.mode),
                bytes: git.repo.blob(&entry.id)?,
                stages: None,
            },
        );
    }
    let Ok(result) = patched(git, slots, &patches) else {
        eprintln!("error: Did you hand edit your patch?");
        eprintln!("It does not apply to blobs recorded in its index.");
        return Ok(Outcome::Unusable);
    };
    let mut theirs = base.clone();
    for (path, slot) in result {
        match slot {
            None => {
                theirs.remove(&path);
            }
            Some(slot) => {
                let id = git.repo.objects().write(Kind::Blob, &slot.bytes)?;
                theirs.insert(
                    path,
                    Entry {
                        mode: leaf_mode(slot.mode),
                        id,
                    },
                );
            }
        }
    }
    let (base_tree, theirs_tree) = (write_leaves(git, &base)?, write_leaves(git, &theirs)?);
    let opened = git.open_index()?;
    let ours_tree = write_tree(git, &opened.index)?;
    let merged = merge_trees(
        git,
        Some(&base_tree),
        Some(&ours_tree),
        Some(&theirs_tree),
        &Labels {
            ours: "HEAD",
            theirs: subject,
            base: "constructed merge base",
        },
        &Settings::default(),
    )?;
    let landing = Landing {
        theirs: subject,
        orig_head: None,
        strict_index: false,
        quiet_when_clean: false,
    };
    let landed = apply_merge(git, &opened, Some(&ours_tree), &merged, &landing)?;
    match landed {
        Ok(applied) if applied.conflicted.is_empty() => Ok(Outcome::Clean),
        Ok(_) => Ok(Outcome::Conflicted),
        Err(refusal) => {
            refusal.blocked.print("merge", "merge");
            Ok(Outcome::Unusable)
        }
    }
}

fn exec_bit(file: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(file).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

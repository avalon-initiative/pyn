//! The objects a repository keeps alive: every ref, `HEAD`, reflog lines and the index.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use pyn_git::{Ref, RefStore};
use pyn_object::ObjectId;

use super::GitHome;

/// One starting point and what names it (a ref, `HEAD`, a reflog or `index`).
pub(super) struct Root {
    pub id: ObjectId,
    pub origin: Origin,
}

#[derive(Clone)]
pub(super) enum Origin {
    Ref(String),
    Reflog(String),
    Index,
    CacheTree(PathBuf),
}

#[derive(Default)]
pub(super) struct Roots {
    pub roots: Vec<Root>,
    /// Refs that hold something other than an id (name and what they hold).
    pub bad_refs: Vec<(String, String)>,
    /// `HEAD` names a branch that does not exist yet.
    pub unborn_head: Option<String>,
}

impl Roots {
    pub fn ids(&self) -> Vec<ObjectId> {
        let mut seen = HashSet::new();
        self.roots
            .iter()
            .map(|r| r.id)
            .filter(|i| seen.insert(*i))
            .collect()
    }
}

fn files_under(dir: &Path, name: &str, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{name}/{file}");
        if entry.path().is_dir() {
            files_under(&entry.path(), &child, out);
        } else if !file.ends_with(".lock") {
            out.push(child);
        }
    }
}

fn zero(git: &GitHome) -> ObjectId {
    git.zero_id()
}

/// Every ref of `store`, loose and packed; loose files that hold garbage go to `roots.bad_refs`.
fn collect_refs(git: &GitHome, store: &RefStore, common: &Path, roots: &mut Roots) -> Result<()> {
    let alg = git.algorithm();
    let mut names = Vec::new();
    files_under(&common.join("refs"), "refs", &mut names);
    names.sort();
    for name in names {
        let Ok(bytes) = fs::read(common.join(&name)) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes).trim_end().to_string();
        if let Some(target) = text.strip_prefix("ref:") {
            match store.resolve(target.trim()) {
                Ok(Some(id)) => roots.roots.push(Root {
                    id,
                    origin: Origin::Ref(name),
                }),
                _ => roots.bad_refs.push((name, zero(git).to_hex())),
            }
        } else if let Ok(id) = ObjectId::from_hex(alg, &text) {
            roots.roots.push(Root {
                id,
                origin: Origin::Ref(name),
            });
        } else {
            roots.bad_refs.push((name, zero(git).to_hex()));
        }
    }
    for p in store.packed()? {
        if !roots
            .roots
            .iter()
            .any(|r| matches!(&r.origin, Origin::Ref(n) if *n == p.name))
        {
            roots.roots.push(Root {
                id: p.id,
                origin: Origin::Ref(p.name),
            });
        }
    }
    Ok(())
}

fn collect_reflogs(
    store: &RefStore,
    logs: &Path,
    with_head: bool,
    roots: &mut Roots,
    zero: ObjectId,
) {
    let mut names = Vec::new();
    files_under(&logs.join("refs"), "refs", &mut names);
    if with_head {
        names.push("HEAD".into());
    }
    for name in names {
        for entry in store.read_reflog(&name).unwrap_or_default() {
            for id in [entry.old, entry.new] {
                if id != zero {
                    roots.roots.push(Root {
                        id,
                        origin: Origin::Reflog(name.clone()),
                    });
                }
            }
        }
    }
}

fn collect_index(git: &GitHome, path: &Path, roots: &mut Roots) {
    let Ok(bytes) = fs::read(path) else {
        return;
    };
    let Ok((index, trees)) = pyn_git::Index::parse_with_cache_tree(&bytes, git.algorithm()) else {
        return;
    };
    for id in trees {
        roots.roots.push(Root {
            id,
            origin: Origin::CacheTree(path.to_path_buf()),
        });
    }
    for e in index.entries {
        if e.mode & 0o170000 != 0o160000 {
            roots.roots.push(Root {
                id: e.id,
                origin: Origin::Index,
            });
        }
    }
}

/// Gathers the roots of the repository, its linked work trees included.
pub(super) fn collect(git: &GitHome, reflogs: bool, index: bool) -> Result<Roots> {
    let repo = git.repo.git();
    let common = repo.common_dir().to_path_buf();
    let alg = git.algorithm();
    let mut roots = Roots::default();
    let main = repo.refs();
    collect_refs(git, &main, &common, &mut roots)?;

    let mut trees: Vec<PathBuf> = vec![repo.git_dir().to_path_buf(), common.clone()];
    if let Ok(rd) = fs::read_dir(common.join("worktrees")) {
        trees.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    trees.sort();
    trees.dedup();
    for dir in trees {
        let store = RefStore::new(dir.clone(), common.clone(), alg);
        match store.head() {
            Ok(Ref::Direct(id)) => roots.roots.push(Root {
                id,
                origin: Origin::Ref("HEAD".into()),
            }),
            Ok(Ref::Symbolic(target)) => match store.resolve(&target) {
                Ok(Some(id)) => roots.roots.push(Root {
                    id,
                    origin: Origin::Ref("HEAD".into()),
                }),
                _ if dir == repo.git_dir() => {
                    roots.unborn_head = Some(
                        target
                            .strip_prefix("refs/heads/")
                            .unwrap_or(&target)
                            .to_string(),
                    );
                }
                _ => {}
            },
            Err(_) => {}
        }
        if reflogs && dir != repo.git_dir() {
            collect_reflogs(&store, &dir.join("logs"), true, &mut roots, zero(git));
        }
        if index {
            collect_index(git, &dir.join("index"), &mut roots);
        }
    }
    if reflogs {
        collect_reflogs(&main, &common.join("logs"), false, &mut roots, zero(git));
        let head = repo.refs();
        for entry in head.read_reflog("HEAD").unwrap_or_default() {
            for id in [entry.old, entry.new] {
                if id != zero(git) {
                    roots.roots.push(Root {
                        id,
                        origin: Origin::Reflog("HEAD".into()),
                    });
                }
            }
        }
    }
    Ok(roots)
}

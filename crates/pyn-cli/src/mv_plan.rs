//! Which paths `mv` renames and where they land, as `git mv` decides it, shared by a git home and a pyn server
//! workspace. Planning reads the work tree and writes nothing.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::fs_path;

#[derive(Args, Debug)]
pub struct MvArgs {
    /// Overwrite a file that is in the way.
    #[arg(short, long)]
    pub force: bool,
    /// Show what would be renamed without renaming it.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Skip a source or destination that cannot be moved instead of stopping.
    #[arg(short = 'k')]
    pub skip: bool,
    #[arg(short, long)]
    pub verbose: bool,
    /// One or more sources, then the destination: a new name, or an existing folder to move them into.
    #[arg(required = true, num_args = 2..)]
    pub paths: Vec<String>,
}

/// One rename in the work tree and the tracked files it carries, as old and new repository paths.
pub struct Pair {
    pub src: Vec<u8>,
    pub dst: Vec<u8>,
    pub files: Vec<(Vec<u8>, Vec<u8>)>,
}

/// A typed path and the repository path it names.
pub type Named = (String, String);

fn meta(root: &Path, path: &[u8]) -> Option<std::fs::Metadata> {
    std::fs::symlink_metadata(fs_path(root, path)).ok()
}

fn is_dir(root: &Path, path: &[u8]) -> bool {
    path.is_empty() || meta(root, path).is_some_and(|m| m.is_dir())
}

fn join(dir: &[u8], name: &[u8]) -> Vec<u8> {
    let mut out = dir.to_vec();
    if !out.is_empty() {
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out
}

fn base(path: &[u8]) -> &[u8] {
    path.rsplit(|b| *b == b'/').next().unwrap_or(path)
}

fn parent(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|b| *b == b'/')
        .map_or(&[][..], |i| &path[..i])
}

fn under(path: &[u8], dir: &[u8]) -> bool {
    path.len() > dir.len() && path.starts_with(dir) && path[dir.len()] == b'/'
}

fn below<'a>(set: &'a BTreeSet<Vec<u8>>, dir: &[u8]) -> impl Iterator<Item = &'a Vec<u8>> {
    let mut start = dir.to_vec();
    start.push(b'/');
    let prefix = start.clone();
    set.range(start..)
        .take_while(move |p| p.starts_with(&prefix))
}

struct World<'a> {
    root: &'a Path,
    tracked: &'a BTreeSet<Vec<u8>>,
    unmerged: &'a BTreeSet<Vec<u8>>,
    force: bool,
}

impl World<'_> {
    fn one(&self, src: &[u8], dst: &[u8]) -> std::result::Result<Pair, &'static str> {
        let Some(found) = meta(self.root, src) else {
            return Err("bad source");
        };
        if src == dst {
            return Err("destination exists");
        }
        let files = if found.is_dir() {
            if under(dst, src) {
                return Err("can not move directory into itself");
            }
            let moving: Vec<_> = below(self.tracked, src)
                .map(|f| (f.clone(), join(dst, &f[src.len() + 1..])))
                .collect();
            if below(self.unmerged, src).next().is_some() {
                return Err("conflicted");
            }
            if moving.is_empty() {
                return Err("source directory is empty");
            }
            if let Some(m) = meta(self.root, dst) {
                return Err(if m.is_dir() {
                    "destination exists"
                } else {
                    "cannot move directory over file"
                });
            }
            moving
        } else {
            if !self.tracked.contains(src) {
                return Err(if self.unmerged.contains(src) {
                    "conflicted"
                } else {
                    "not under version control"
                });
            }
            match meta(self.root, dst) {
                Some(m) if m.is_dir() || !self.force => return Err("destination exists"),
                None if self.tracked.contains(dst) && !self.force => {
                    return Err("destination exists");
                }
                _ => {}
            }
            vec![(src.to_vec(), dst.to_vec())]
        };
        if !is_dir(self.root, parent(dst)) {
            return Err("destination directory does not exist");
        }
        Ok(Pair {
            src: src.to_vec(),
            dst: dst.to_vec(),
            files,
        })
    }
}

/// The renames for `srcs` into `dst`. Stops at the first source that cannot move, or skips it with `skip`.
pub fn plan(
    root: &Path,
    tracked: &BTreeSet<Vec<u8>>,
    unmerged: &BTreeSet<Vec<u8>>,
    args: &MvArgs,
    named: &[Named],
) -> Result<Vec<Pair>> {
    let (dst, srcs) = named.split_last().expect("at least two paths");
    let dst_bytes = dst.1.as_bytes();
    let dst_dir = is_dir(root, dst_bytes);
    let into = srcs.len() > 1 || dst_dir;
    if into && !dst_dir {
        bail!("destination '{}' is not a directory", dst.0);
    }
    if !into && dst.0.ends_with('/') {
        bail!("destination directory '{}' does not exist", dst.0);
    }
    let world = World {
        root,
        tracked,
        unmerged,
        force: args.force,
    };
    let mut pairs: Vec<Pair> = Vec::new();
    for (arg, prefix) in srcs {
        let src = prefix.as_bytes();
        let target = if into {
            join(dst_bytes, base(src))
        } else {
            dst_bytes.to_vec()
        };
        let verdict = if src.is_empty() {
            Err("bad source")
        } else if pairs.iter().any(|p| p.dst == target) {
            Err("multiple sources for the same target")
        } else {
            world.one(src, &target)
        };
        match verdict {
            Ok(pair) => pairs.push(pair),
            Err(_) if args.skip => {}
            Err(why) => bail!(
                "{why}, source={arg}, destination={}",
                String::from_utf8_lossy(&target)
            ),
        }
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_at_the_last_slash() {
        assert_eq!(base(b"a/b/c"), b"c");
        assert_eq!(parent(b"a/b/c"), b"a/b");
        assert_eq!(parent(b"c"), b"");
        assert_eq!(join(b"", b"x"), b"x");
        assert_eq!(join(b"d", b"x"), b"d/x");
    }

    #[test]
    fn a_folder_holds_only_what_follows_its_slash() {
        let set: BTreeSet<Vec<u8>> = [&b"a/x"[..], b"a/y", b"a.txt", b"ab/z"]
            .iter()
            .map(|p| p.to_vec())
            .collect();
        let got: Vec<_> = below(&set, b"a").cloned().collect();
        assert_eq!(got, vec![b"a/x".to_vec(), b"a/y".to_vec()]);
        assert!(under(b"a/x", b"a") && !under(b"ab/x", b"a") && !under(b"a", b"a"));
    }
}

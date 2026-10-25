//! `pyn ls-tree` on a git repository: the entries of a tree object, optionally recursively.

use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::MagicSet;
use pyn_object::{FileMode, Kind, ObjectId, TreeEntry};

use super::GitHome;
use super::plumbing::{Resolver, abbrev, default_abbrev, emit_bytes, peel, prefix, quote};
use crate::short_status::relative;

/// List the contents of a tree object.
#[derive(Debug, Args)]
pub struct LsTreeArgs {
    /// Show only the tree entries, not the files in them.
    #[arg(short = 'd')]
    trees_only: bool,
    /// Go into subtrees.
    #[arg(short = 'r')]
    recursive: bool,
    /// Show tree entries even when going into them.
    #[arg(short = 't')]
    show_trees: bool,
    /// Show the size of each file.
    #[arg(short = 'l', long)]
    long: bool,
    /// End each line with a NUL and do not quote names.
    #[arg(short = 'z')]
    nul: bool,
    /// Show only the names.
    #[arg(long, alias = "name-status")]
    name_only: bool,
    /// Show only the object ids.
    #[arg(long)]
    object_only: bool,
    /// Show paths from the repository root, not from the current folder.
    #[arg(long)]
    full_name: bool,
    /// List from the repository root whatever the current folder, and take paths from the root.
    #[arg(long)]
    full_tree: bool,
    /// Abbreviate object ids to at least this many digits (7 if omitted).
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
    abbrev: Option<String>,
    /// A commit, tag, tree or `<rev>:<path>`.
    tree_ish: String,
    /// Paths to limit the listing to; a trailing `/` lists a folder's contents.
    paths: Vec<String>,
}

struct Walk<'a> {
    git: &'a GitHome,
    args: &'a LsTreeArgs,
    pre: Vec<u8>,
    patterns: Vec<Vec<u8>>,
    show_trees: bool,
    abbrev: Option<usize>,
    out: Vec<u8>,
}

pub(super) fn ls_tree(git: &GitHome, cwd: &Path, args: LsTreeArgs) -> Result<()> {
    let pre = if args.full_tree {
        Vec::new()
    } else {
        prefix(git, cwd)?
    };
    let resolver = Resolver { git, prefix: &pre };
    let Some(id) = resolver.object(&args.tree_ish)? else {
        bail!("Not a valid object name {}", args.tree_ish);
    };
    let Some(tree) = peel(git, &id, Some(Kind::Tree))? else {
        bail!("not a tree object");
    };
    let base = if args.full_tree {
        git.repo.root()?.to_path_buf()
    } else {
        cwd.to_path_buf()
    };
    let refused = MagicSet {
        glob: true,
        icase: true,
        exclude: true,
        attr: true,
        ..MagicSet::default()
    };
    let spec = git.pathspec_without(&base, &args.paths, true, refused)?;
    let patterns: Vec<Vec<u8>> = (0..spec.len())
        .map(|n| spec.item_path(n))
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    let abbrev = args.abbrev.as_deref().map(|v| match v.parse::<usize>() {
        Ok(n) => n.clamp(4, git.algorithm().digest_len() * 2),
        Err(_) => default_abbrev(git),
    });
    let show_trees = args.show_trees || (args.trees_only && args.recursive);
    let mut walk = Walk {
        git,
        args: &args,
        pre,
        patterns,
        show_trees,
        abbrev,
        out: Vec::new(),
    };
    let result = walk.tree(&tree, &mut Vec::new());
    emit_bytes(&walk.out);
    result
}

impl Walk<'_> {
    /// Whether the entry is named by a path, or lies under a folder one names.
    fn selected(&self, path: &[u8]) -> bool {
        self.patterns.is_empty()
            || self.patterns.iter().any(|p| {
                path == p.as_slice()
                    || (path.starts_with(p)
                        && (p.ends_with(b"/") || path.get(p.len()) == Some(&b'/')))
            })
    }

    /// Whether a path names something below this folder.
    fn ancestor(&self, path: &[u8]) -> bool {
        self.patterns
            .iter()
            .any(|p| p.len() > path.len() && p.starts_with(path) && p[path.len()] == b'/')
    }

    fn tree(&mut self, id: &ObjectId, base: &mut Vec<u8>) -> Result<()> {
        for e in self.git.repo.tree(id)? {
            let keep = base.len();
            base.extend_from_slice(&e.name);
            let selected = self.selected(base);
            if !e.mode.is_tree() {
                if selected && !self.args.trees_only {
                    self.entry(&e, base)?;
                }
            } else {
                let ancestor = self.ancestor(base);
                let recurse = (selected || ancestor) && (self.args.recursive || ancestor);
                if (selected || ancestor) && (self.show_trees || !recurse) {
                    self.entry(&e, base)?;
                }
                if recurse {
                    base.push(b'/');
                    self.tree(&e.id, base)?;
                }
            }
            base.truncate(keep);
        }
        Ok(())
    }

    fn entry(&mut self, e: &TreeEntry, path: &[u8]) -> Result<()> {
        let shown = if self.args.full_name || self.args.full_tree {
            path.to_vec()
        } else {
            relative(self.pre.strip_suffix(b"/").unwrap_or(&self.pre), path)
        };
        let name = if self.args.nul {
            shown
        } else {
            quote(self.git, &shown).into_bytes()
        };
        let hash = match self.abbrev {
            Some(n) => abbrev(self.git, &e.id, n),
            None => e.id.to_hex(),
        };
        let out = &mut self.out;
        if self.args.name_only {
            out.extend_from_slice(&name);
        } else if self.args.object_only {
            out.extend_from_slice(hash.as_bytes());
        } else {
            let kind = match e.mode {
                FileMode::Tree => "tree",
                FileMode::Gitlink => "commit",
                _ => "blob",
            };
            let mode = u32::from_str_radix(e.mode.as_str(), 8).unwrap_or(0);
            out.extend_from_slice(format!("{mode:06o} {kind} {hash}").as_bytes());
            if self.args.long {
                let size = match e.mode {
                    FileMode::Tree | FileMode::Gitlink => "-".to_string(),
                    _ => self.git.repo.blob(&e.id)?.len().to_string(),
                };
                out.extend_from_slice(format!(" {size:>7}").as_bytes());
            }
            out.push(b'\t');
            out.extend_from_slice(&name);
        }
        out.push(if self.args.nul { 0 } else { b'\n' });
        Ok(())
    }
}

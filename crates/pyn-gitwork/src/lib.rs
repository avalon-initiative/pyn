//! Read-only views of a git work tree, built on `pyn-git`: ignore rules, revisions, history, tree diffs and
//! status. Nothing here writes to the repository or the work tree, and git is never invoked.

mod attributes;
mod convert;
mod error;
mod history;
mod ignore;
mod mailmap;
mod pathspec;
mod repo;
mod rev;
mod revmark;
mod safepath;
mod status;
mod submodule;
mod tree;
mod wildmatch;

pub use attributes::{AttrValue, Attributes, Loader, PathAttributes};
pub use convert::{Conversion, Converted, SafeCrlf, Settings, ToGit, Unsupported};
pub use error::{Error, Result};
pub use history::{Log, LogEntry, Walked};
pub use ignore::{Ignore, Level, Rule, parse_rules, verdict};
pub use mailmap::Mailmap;
pub use pathspec::{
    MagicSet, Match, Pathspec, PathspecContext, PathspecError, PathspecOptions, Tracker,
    read_pathspec_file,
};
pub use repo::{Head, Repo};
pub use safepath::{
    MAX_DEPTH, MAX_PATH_BYTES, Reason, UnsafePath, check_link_target, check_path, check_path_as,
    check_paths,
};
pub use status::{Conflict, Kind, Status, StatusEntry, fs_path, hash_worktree_file, worktree_mode};
pub use submodule::{SubmoduleIgnore, SubmoduleStatus, Submodules};
pub use tree::{Change, Entry, MAX_TREE_DEPTH};
pub use wildmatch::{matches as wildmatch, matches_fold as wildmatch_fold};

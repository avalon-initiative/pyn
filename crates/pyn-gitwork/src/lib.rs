//! Read-only views of a git work tree, built on `pyn-git`: ignore rules, revisions, history, tree diffs and
//! status. Nothing here writes to the repository or the work tree, and git is never invoked.

mod error;
mod history;
mod ignore;
mod pathspec;
mod repo;
mod rev;
mod safepath;
mod status;
mod tree;
mod wildmatch;

pub use error::{Error, Result};
pub use history::{Log, LogEntry};
pub use ignore::{Ignore, Level, parse_rules};
pub use pathspec::Pathspec;
pub use repo::{Head, Repo};
pub use safepath::{
    MAX_DEPTH, MAX_PATH_BYTES, Reason, UnsafePath, check_link_target, check_path, check_path_as,
    check_paths,
};
pub use status::{Conflict, Kind, Status, StatusEntry, fs_path, hash_worktree_file, worktree_mode};
pub use tree::{Change, Entry, MAX_TREE_DEPTH};

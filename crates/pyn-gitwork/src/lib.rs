//! Read-only views of a git work tree, built on `pyn-git`: ignore rules, revisions, history, tree diffs and
//! status. Apart from the filter drivers (which run the user's own commands) and the local LFS object store
//! written by `filter=lfs`, nothing here writes to the repository or the work tree, and git is never invoked.

mod abbrev;
mod convert;
mod error;
mod filter;
mod history;
mod ignore;
pub mod lfs;
mod mailmap;
mod pathspec;
mod repo;
mod rev;
mod revmark;
mod safepath;
mod status;
mod submodule;
mod tree;

use pyn_gitattr::{attributes, wildmatch};

pub use abbrev::Abbreviations;
pub use convert::{Conversion, Converted, SafeCrlf, Settings, ToGit, Unsupported, eol_info};
pub use error::{Error, Result};
pub use filter::{Driver, External, Filters, Op};
pub use history::{Log, LogEntry, Walked};
pub use ignore::{Ignore, Level, Rule, parse_rules, verdict};
pub use mailmap::Mailmap;
pub use pathspec::{
    MagicSet, Match, Pathspec, PathspecContext, PathspecError, PathspecOptions, Tracker,
    read_pathspec_file,
};
pub use pyn_gitattr::attributes::{
    AttrValue, Attributes, Loader, PathAttributes, unquote as unquote_c, valid_attribute_name,
};
pub use pyn_gitattr::wildmatch::{matches as wildmatch, matches_fold as wildmatch_fold};
pub use repo::{AttrSource, Head, Repo, global_attributes};
pub use safepath::{
    MAX_DEPTH, MAX_PATH_BYTES, Reason, UnsafePath, check_link_target, check_path, check_path_as,
    check_paths,
};
pub use status::{Conflict, Kind, Status, StatusEntry, fs_path, hash_worktree_file, worktree_mode};
pub use submodule::{SubmoduleIgnore, SubmoduleStatus, Submodules};
pub use tree::{Change, Entry, MAX_TREE_DEPTH};

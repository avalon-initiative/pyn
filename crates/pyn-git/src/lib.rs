//! A reader and writer for git's on-disk formats, implemented in house: loose objects, packs (offset and
//! reference deltas, pack index v2), refs and `packed-refs`, config (read and edit), refspecs, remotes, the index
//! (v2 to v4), `.git` files and `init` of an empty repository.
//! Packs are read, ingested (`index_pack`, thin packs included) and written, and `reachable_objects` walks what a fetch needs.
//! No git library or binary is involved. Depends only on `pyn-object` for ids and object encodings.

mod config;
mod config_file;
mod delta;
mod error;
mod index;
mod ingest;
mod init;
mod odb;
mod pack;
mod pack_write;
mod reflog;
mod refs;
mod refspec;
mod remote;
mod repo;
mod util;
mod walk;

pub use config::{Config, Entry as ConfigEntry, Identity};
pub use config_file::{ConfigFile, quote_value};
pub use error::{Error, Result};
pub use index::{Index, IndexEntry, Timestamp};
pub use ingest::{IngestLimits, IngestedPack, MAX_DELTA_DEPTH, index_pack};
pub use init::init;
pub use odb::{Object, ObjectDb};
pub use pack::{EntryKind, Pack};
pub use pack_write::{PackEntry, PackIndexEntry, PackSummary, PackWriter, write_pack};
pub use reflog::ReflogEntry;
pub use refs::{Expect, PackedRef, Ref, RefStore, check_name as check_ref_name};
pub use refspec::{Refspec, map_ref};
pub use remote::{
    Remote, Tracking, add_remote, branches_tracking, is_valid_remote_name, remote_names,
    remove_remote, rename_remote, set_tracking, tracking, unset_tracking,
};
pub use repo::{Repository, resolve_git_dir};
pub use walk::{Reachable, commits_newest_first, existing, reachable_objects};

//! A reader and writer for git's on-disk formats, implemented in house: loose objects, packs (offset and
//! reference deltas, pack index v2), refs and `packed-refs`, config, the index (v2 to v4) and `.git` files.
//! No git library or binary is involved. Depends only on `pyn-object` for ids and object encodings.

mod config;
mod delta;
mod error;
mod index;
mod odb;
mod pack;
mod refs;
mod repo;
mod util;

pub use config::{Config, Entry as ConfigEntry, Identity};
pub use error::{Error, Result};
pub use index::{Index, IndexEntry, Timestamp};
pub use odb::{Object, ObjectDb};
pub use pack::{EntryKind, Pack};
pub use refs::{PackedRef, Ref, RefStore, check_name as check_ref_name};
pub use repo::{Repository, resolve_git_dir};

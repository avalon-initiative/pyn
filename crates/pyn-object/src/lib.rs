//! Git's object ids and encodings (blob, tree, commit, tag) as pure functions, parameterized by hash algorithm.
//! `Commit::parse` and `Tag::parse` accept only canonical objects; `parse_lenient` reads what git tolerates and keeps the
//! original bytes (`Raw`), so an object read from a git repository keeps its id. No I/O; git is never invoked. SHA-1 uses collision detection, as git does.

mod commit;
mod hasher;
mod id;
mod name;
mod tree;

pub use commit::{Commit, Raw, Signature, Tag};
pub use hasher::ObjectHasher;
pub use id::{Algorithm, Kind, ObjectId, hash_object};
pub use name::{fold_name, is_git_dir_name, names_dir};
pub use tree::{FileMode, TreeEntry, encode_tree, parse_tree};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("SHA-1 collision attack detected in object data")]
    Collision,
    #[error("invalid object id: {0}")]
    InvalidId(String),
    #[error("unknown file mode {0:?}")]
    InvalidMode(String),
    #[error("object id uses {actual:?} but the repository uses {expected:?}")]
    AlgorithmMismatch {
        expected: Algorithm,
        actual: Algorithm,
    },
    #[error("invalid tree entry name")]
    InvalidName,
    #[error("duplicate tree entry name")]
    DuplicateName,
    #[error("malformed {0} object: {1}")]
    Malformed(&'static str, &'static str),
    #[error("declared length {declared} but {actual} bytes were supplied")]
    LengthMismatch { declared: u64, actual: u64 },
}

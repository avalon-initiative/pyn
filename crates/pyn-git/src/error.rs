use std::path::PathBuf;

use pyn_object::ObjectId;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Object(#[from] pyn_object::Error),
    #[error("object {0} not found")]
    ObjectNotFound(ObjectId),
    #[error("reference {0} not found")]
    RefNotFound(String),
    #[error("reference {0} changed while it was being updated")]
    RefChanged(String),
    #[error("corrupt {what}: {detail}")]
    Corrupt { what: &'static str, detail: String },
    #[error("{what} checksum mismatch")]
    Checksum { what: &'static str },
    #[error("{0} is truncated")]
    Truncated(&'static str),
    #[error("{what} exceeds the limit of {limit}")]
    Limit { what: &'static str, limit: u64 },
    #[error("reading input: {0}")]
    Read(std::io::Error),
    #[error("writing output: {0}")]
    Write(std::io::Error),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0} has multiple values")]
    MultipleValues(String),
    #[error("invalid {what}: {detail}")]
    Invalid { what: &'static str, detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn corrupt(what: &'static str, detail: impl Into<String>) -> Error {
    Error::Corrupt {
        what,
        detail: detail.into(),
    }
}

pub(crate) fn invalid(what: &'static str, detail: impl Into<String>) -> Error {
    Error::Invalid {
        what,
        detail: detail.into(),
    }
}

pub(crate) fn io_at(path: &std::path::Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

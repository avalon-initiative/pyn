use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Url(String),
    #[error(
        "local-path remotes are not supported yet: '{0}' is a path on this machine; use an ssh:// or https:// URL"
    )]
    LocalPath(String),
    #[error(transparent)]
    Wire(#[from] pyn_gitwire::Error),
    #[error(transparent)]
    Git(#[from] pyn_git::Error),
    #[error("{0}")]
    Credential(String),
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<Error> for pyn_gitwire::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Wire(w) => w,
            Error::Credential(m) => Self::Auth(m),
            other => Self::Connection(other.to_string()),
        }
    }
}

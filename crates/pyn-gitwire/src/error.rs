use std::io;

use pyn_object::Algorithm;

/// Framing errors of the pkt-line layer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PktError {
    #[error("invalid pkt-line length header {0:?}")]
    InvalidLength([u8; 4]),
    #[error("reserved pkt-line length 0003")]
    ReservedLength,
    #[error("pkt-line payload of {0} bytes exceeds the {max} byte limit", max = crate::pkt::MAX_DATA)]
    TooLong(usize),
    #[error("stream ended inside a pkt-line")]
    UnexpectedEof,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(io::Error),
    #[error(transparent)]
    Pkt(#[from] PktError),
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// An `ERR` packet or a sideband channel 3 message.
    #[error("remote error: {0}")]
    Remote(String),
    #[error("server uses object format {server} but the repository uses {local}", server = server.name(), local = local.name())]
    ObjectFormatMismatch { server: Algorithm, local: Algorithm },
    /// The connection failed: ssh exited abnormally, the HTTP request failed, the host was unreachable.
    #[error("{0}")]
    Connection(String),
    /// The server refused the credentials, or none could be found.
    #[error("{0}")]
    Auth(String),
    #[error("{0}")]
    NotFound(String),
    #[error("server does not support {0}")]
    Unsupported(String),
    #[error("invalid request: {0}")]
    Usage(String),
    #[error(transparent)]
    Id(#[from] pyn_object::Error),
}

impl From<io::Error> for Error {
    /// Recovers a typed error that travelled through an `io::Error` (as the sideband reader does).
    fn from(e: io::Error) -> Self {
        match e.downcast::<Error>() {
            Ok(inner) => inner,
            Err(e) => Error::Io(e),
        }
    }
}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(e) => e,
            other => io::Error::new(io::ErrorKind::InvalidData, other),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

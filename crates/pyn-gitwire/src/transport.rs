//! The seam between the protocol client and a connection to a git server.
//!
//! A [`Transport`] opens a [`Session`] for one service. The session first yields the server's
//! advertisement, then answers requests one at a time: `send` delivers a complete body and `response`
//! exposes the response stream. Smart HTTP maps a session to `GET info/refs?service=...` followed by one
//! `POST git-<service>` per request (stateless); an SSH or spawned-process connection keeps one
//! full-duplex pipe whose responses follow the requests in sequence. The client reads each response
//! through its protocol-level terminator before the next request, so both shapes work.

use std::io::Read;

use crate::caps::ProtocolVersion;
use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// The `git-upload-pack` / `git-receive-pack` name used in URLs and `info/refs`.
    pub fn name(self) -> &'static str {
        match self {
            Self::UploadPack => "git-upload-pack",
            Self::ReceivePack => "git-receive-pack",
        }
    }
}

pub trait Transport {
    /// Opens a session. `version` is the protocol the client prefers (`GIT_PROTOCOL` / `Git-Protocol`);
    /// the server may answer with an older one.
    fn open(&mut self, service: Service, version: ProtocolVersion) -> Result<Box<dyn Session>>;
}

pub trait Session {
    /// The server's initial pkt-line advertisement. Called once, before any request.
    fn advertisement(&mut self) -> Result<Box<dyn Read + '_>>;

    /// Sends one request body. The previous response must have been read through its terminator.
    fn send(&mut self, body: &mut dyn Read) -> Result<()>;

    /// The response to the last `send`. Calling it again continues the same stream where the
    /// previous reader stopped, so a reader may be dropped part way and resumed.
    fn response(&mut self) -> Result<Box<dyn Read + '_>>;
}

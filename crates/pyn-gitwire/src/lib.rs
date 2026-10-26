//! Git's wire protocol as a client, with no network or process code of its own.
//!
//! [`pkt`] and [`sideband`] are the framing, [`caps`] parses advertisements and negotiates the object
//! format, [`UploadPack`] fetches (protocol v2, with a v0/v1 fallback) and [`ReceivePack`] pushes
//! (v0/v1 `receive-pack`). Connections come from a [`Transport`]; the SSH and HTTP ones live in `pyn-gittransport`.
//! Push options, `wait-for-done` and v0 multi-round negotiation are not built.

pub mod caps;
mod error;
pub mod pkt;
mod receive;
pub mod sideband;
mod transport;
mod upload;

pub use caps::{AdvertisedRef, Advertisement, Capabilities, ProtocolVersion};
pub use error::{Error, PktError, Result};
pub use receive::{PushOptions, PushReport, ReceivePack, RefResult, RefStatus, RefUpdate};
pub use transport::{Service, Session, Transport};
pub use upload::{
    Deepen, FetchRequest, HaveSource, LsRefsOptions, NoHaves, PackReader, RemoteRef, ShallowInfo,
    UploadPack,
};

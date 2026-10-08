//! Domain model and policy. No database, HTTP or filesystem dependency; infrastructure sits behind traits.

pub mod auth;
pub mod clock;
#[cfg(feature = "contract")]
pub mod contract;
pub mod error;
pub mod memory;
pub mod object;
pub mod rules;
pub mod service;
pub mod store;
pub mod types;

pub use auth::AuthProvider;
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::{PynError, Result};
pub use object::ObjectStore;
pub use rules::{Mode, Rules};
pub use service::{RepoService, ServiceConfig};
pub use store::MetadataStore;
pub use types::{ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId};

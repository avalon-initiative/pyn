//! Domain model and policy for pyn. No database, HTTP, or filesystem dependency: every
//! infrastructure seam is a trait (`MetadataStore`, `ObjectStore`, `AuthProvider`, `Clock`)
//! so implementations can be swapped. The `memory` module holds the reference implementations
//! used by tests and the dev server.

pub mod auth;
pub mod clock;
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

//! The connections `pyn` makes to a git remote, with no git program involved: the system `ssh` client, smart HTTP,
//! git's credential flow, and the glue that moves objects between a repository and a remote.
//! Only `ssh` and the credential helpers a user's own configuration names are ever run.

mod connect;
pub mod credential;
mod env;
mod error;
pub mod exchange;
mod http;
mod local;
mod ssh;
mod url;

#[cfg(all(test, unix))]
#[path = "../tests/common/script.rs"]
mod script;

pub use connect::{Connection, Connector, user_config};
pub use env::Env;
pub use error::{Error, Result};
pub use http::{HttpTransport, USER_AGENT};
pub use local::LocalTransport;
pub use ssh::{SshTransport, Variant, remote_command, shell_quote};
pub use url::{RemoteUrl, SshUrl, redact_text, rewrite};

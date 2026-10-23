//! The interface the working-tree and history commands run against, so a repository's home is replaceable.

use std::path::Path;

use anyhow::Result;
use pyn_proto as api;

use crate::client::Api;

/// A command or feature that only a pyn server can provide, asked of a home that cannot.
#[derive(Debug)]
pub struct NeedsPynServer {
    pub feature: String,
}

impl std::fmt::Display for NeedsPynServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`pyn {}` needs a pyn server, and this repository is a git repository; \
             `pyn convert --home pyn` will move it to one (not built yet, #572)",
            self.feature
        )
    }
}

impl std::error::Error for NeedsPynServer {}

pub trait Backend {
    /// The pyn server connection for a feature only a pyn server provides, such as locks, policy or audit.
    fn pyn_server(&self, feature: &str) -> Result<&Api> {
        Err(NeedsPynServer {
            feature: feature.to_string(),
        }
        .into())
    }

    /// The signed-in user's name.
    fn whoami(&self) -> Result<String>;

    fn list_files(&self) -> Result<Vec<api::FileEntry>>;

    fn tree(&self, path: Option<&str>) -> Result<api::TreeListing>;

    /// The newest commit's id; `None` when the repository has none.
    fn head_commit(&self) -> Result<Option<String>>;

    /// Commits newest first, strictly older than `before` (an id or abbreviation) when given.
    fn commit_page(&self, before: Option<&str>, limit: usize) -> Result<api::CommitPage>;

    /// `commit_page` narrowed by the server's commit filters.
    fn commit_page_filtered(
        &self,
        _before: Option<&str>,
        _limit: usize,
        _filter: &crate::log_filter::CommitFilter,
    ) -> Result<api::CommitPage> {
        anyhow::bail!("this home does not filter commits")
    }

    /// A commit by id or unique abbreviation.
    fn commit(&self, id: &str) -> Result<api::Commit>;

    /// Every path as of `commit` (an id or abbreviation; the newest if `None`), and the full id used.
    fn snapshot(&self, commit: Option<&str>) -> Result<(Option<String>, Vec<api::SnapshotEntry>)>;

    /// A stored object's content by its id (a git blob); `probe` asks for only the first bytes when the home can.
    fn blob(&self, _id: &str, _probe: Option<usize>) -> Result<Vec<u8>> {
        anyhow::bail!("this home does not serve objects by id")
    }

    /// One page of commits and trees a client lacks (`POST .../fetch`).
    fn fetch_page(&self, _request: &api::FetchRequest) -> Result<api::FetchResponse> {
        anyhow::bail!("this home does not serve fetches")
    }

    /// A blob's bytes by its full id (`GET .../objects/{id}`).
    fn object(&self, _id: &str) -> Result<Vec<u8>> {
        anyhow::bail!("this home does not serve objects by id")
    }

    /// Appends client-built commits to a branch (`POST .../push`), all or nothing.
    fn push_commits(&self, _request: &api::PushRequest) -> Result<api::PushResponse> {
        anyhow::bail!("this home does not accept pushed commits")
    }

    /// A file's content at a revision; `probe` asks for only the first bytes when the home can.
    fn content(&self, path: &str, revision: u64, probe: Option<usize>) -> Result<Vec<u8>>;

    /// Streams the head (or `revision`) of `path` to `out`.
    fn read(&self, path: &str, revision: Option<u64>, out: &mut dyn std::io::Write) -> Result<()>;

    /// Downloads to `dest`; returns the revision and the content hash.
    fn download(&self, path: &str, revision: Option<u64>, dest: &Path) -> Result<(u64, String)>;

    /// Stores a local file's content and returns its content id.
    fn upload(&self, file: &Path) -> Result<String>;

    /// Stores `bytes` as one object and returns its content id.
    fn upload_bytes(&self, bytes: Vec<u8>) -> Result<String>;

    fn commit_file(&self, request: &api::CheckinRequest) -> Result<api::Revision>;

    fn commit_changeset(&self, request: &api::ChangesetRequest) -> Result<api::ChangesetResponse>;

    /// A path's revisions, oldest first.
    fn path_history(&self, path: &str) -> Result<Vec<api::Revision>>;

    /// Newest-first revisions across the repository, up to `limit`.
    fn repo_history(&self, filter: Option<&str>, limit: usize) -> Result<Vec<api::Revision>>;

    fn restore(&self, request: &api::RestoreRequest) -> Result<api::Revision>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoServer;

    impl Backend for NoServer {
        fn whoami(&self) -> Result<String> {
            unimplemented!()
        }
        fn list_files(&self) -> Result<Vec<api::FileEntry>> {
            unimplemented!()
        }
        fn tree(&self, _: Option<&str>) -> Result<api::TreeListing> {
            unimplemented!()
        }
        fn head_commit(&self) -> Result<Option<String>> {
            unimplemented!()
        }
        fn commit_page(&self, _: Option<&str>, _: usize) -> Result<api::CommitPage> {
            unimplemented!()
        }
        fn commit(&self, _: &str) -> Result<api::Commit> {
            unimplemented!()
        }
        fn snapshot(&self, _: Option<&str>) -> Result<(Option<String>, Vec<api::SnapshotEntry>)> {
            unimplemented!()
        }
        fn content(&self, _: &str, _: u64, _: Option<usize>) -> Result<Vec<u8>> {
            unimplemented!()
        }
        fn read(&self, _: &str, _: Option<u64>, _: &mut dyn std::io::Write) -> Result<()> {
            unimplemented!()
        }
        fn download(&self, _: &str, _: Option<u64>, _: &Path) -> Result<(u64, String)> {
            unimplemented!()
        }
        fn upload(&self, _: &Path) -> Result<String> {
            unimplemented!()
        }
        fn upload_bytes(&self, _: Vec<u8>) -> Result<String> {
            unimplemented!()
        }
        fn commit_file(&self, _: &api::CheckinRequest) -> Result<api::Revision> {
            unimplemented!()
        }
        fn commit_changeset(&self, _: &api::ChangesetRequest) -> Result<api::ChangesetResponse> {
            unimplemented!()
        }
        fn path_history(&self, _: &str) -> Result<Vec<api::Revision>> {
            unimplemented!()
        }
        fn repo_history(&self, _: Option<&str>, _: usize) -> Result<Vec<api::Revision>> {
            unimplemented!()
        }
        fn restore(&self, _: &api::RestoreRequest) -> Result<api::Revision> {
            unimplemented!()
        }
    }

    #[test]
    fn a_home_without_a_pyn_server_refuses_server_features() {
        let err = NoServer.pyn_server("lock").map(|_| ()).unwrap_err();
        assert!(err.downcast_ref::<NeedsPynServer>().is_some());
        assert_eq!(
            err.to_string(),
            "`pyn lock` needs a pyn server, and this repository is a git repository; \
             `pyn convert --home pyn` will move it to one (not built yet, #572)"
        );
    }
}

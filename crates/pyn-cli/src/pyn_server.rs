//! The backend for a repository hosted on a pyn server, over its HTTP API.

use std::path::Path;

use crate::transport::Method;
use anyhow::Result;
use pyn_proto as api;

use crate::backend::Backend;
use crate::client::Api;
use crate::local_store::LocalStore;
use crate::transfer;

pub struct PynServer {
    api: Api,
    store: Option<LocalStore>,
}

impl PynServer {
    pub fn new(api: Api) -> Self {
        Self { api, store: None }
    }

    pub fn with_store(mut self, store: Option<LocalStore>) -> Self {
        self.store = store;
        self
    }
}

impl Backend for PynServer {
    fn pyn_server(&self, _feature: &str) -> Result<&Api> {
        Ok(&self.api)
    }

    fn whoami(&self) -> Result<String> {
        let api = &self.api;
        let me: api::Account = api.send(api.get("/v1/me"))?.json()?;
        Ok(me.user)
    }

    fn list_files(&self) -> Result<Vec<api::FileEntry>> {
        self.api.list_files()
    }

    fn tree(&self, path: Option<&str>) -> Result<api::TreeListing> {
        let api = &self.api;
        let mut req = api.on_branch(api.get(&api.repo_route("/tree")?));
        if let Some(p) = path {
            req = req.query(&[("path", p)]);
        }
        api.send(req)?.json()
    }

    fn head_commit(&self) -> Result<Option<String>> {
        let api = &self.api;
        let page: api::CommitPage = api
            .send(api.on_branch(api.get(&api.repo_route("/commits")?).query(&[("limit", 1)])))?
            .json()?;
        Ok(page.commits.into_iter().next().map(|c| c.id))
    }

    fn commit_page(&self, before: Option<&str>, limit: usize) -> Result<api::CommitPage> {
        let api = &self.api;
        let mut req = api.on_branch(
            api.get(&api.repo_route("/commits")?)
                .query(&[("limit", limit)]),
        );
        if let Some(b) = before {
            req = req.query(&[("before", b)]);
        }
        api.send(req)?.json()
    }

    fn commit(&self, id: &str) -> Result<api::Commit> {
        let api = &self.api;
        api.send(api.get(&api.repo_route(&format!("/commits/{id}"))?))?
            .json()
    }

    fn snapshot(&self, commit: Option<&str>) -> Result<(Option<String>, Vec<api::SnapshotEntry>)> {
        let api = &self.api;
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut req = api.get(&api.repo_route("/snapshot")?);
            req = match commit {
                Some(c) => req.query(&[("commit", c)]),
                None => api.on_branch(req),
            };
            if let Some(a) = &after {
                req = req.query(&[("after", a)]);
            }
            let page: api::SnapshotPage = api.send(req)?.json()?;
            all.extend(page.entries);
            match page.next_after {
                Some(next) => after = Some(next),
                None => return Ok((page.commit, all)),
            }
        }
    }

    fn blob(&self, id: &str, probe: Option<usize>) -> Result<Vec<u8>> {
        let Some(store) = &self.store else {
            anyhow::bail!("this home does not serve objects by id");
        };
        let mut bytes = store.blob(self, id)?;
        if let Some(len) = probe {
            bytes.truncate(len);
        }
        Ok(bytes)
    }

    fn content(&self, path: &str, revision: u64, probe: Option<usize>) -> Result<Vec<u8>> {
        let api = &self.api;
        let route = api.repo_route("/content")?;
        let get = || {
            api.get(&route)
                .query(&[("path", path)])
                .query(&[("revision", revision)])
        };
        if let Some(len) = probe
            && let Ok(resp) = api.send(get().header("range", format!("bytes=0-{}", len.max(1) - 1)))
        {
            return Ok(resp.bytes()?.to_vec());
        }
        Ok(api.send(get())?.bytes()?.to_vec())
    }

    fn read(&self, path: &str, revision: Option<u64>, out: &mut dyn std::io::Write) -> Result<()> {
        let api = &self.api;
        let mut req = api
            .get(&api.repo_route("/content")?)
            .query(&[("path", path)]);
        req = match revision {
            Some(r) => req.query(&[("revision", r)]),
            None => api.on_branch(req),
        };
        api.send(req)?.copy_to(out)?;
        Ok(())
    }

    fn download(&self, path: &str, revision: Option<u64>, dest: &Path) -> Result<(u64, String)> {
        transfer::download(&self.api, path, revision, dest)
    }

    fn upload(&self, file: &Path) -> Result<String> {
        transfer::upload_file(&self.api, file)
    }

    fn upload_bytes(&self, bytes: Vec<u8>) -> Result<String> {
        transfer::upload_bytes(&self.api, bytes)
    }

    fn commit_file(&self, request: &api::CheckinRequest) -> Result<api::Revision> {
        let api = &self.api;
        let request = api::CheckinRequest {
            branch: api.branch.clone(),
            ..request.clone()
        };
        api.send(
            api.request(Method::POST, &api.repo_route("/checkin")?)
                .json(&request),
        )?
        .json()
    }

    fn commit_changeset(&self, request: &api::ChangesetRequest) -> Result<api::ChangesetResponse> {
        let api = &self.api;
        let request = api::ChangesetRequest {
            branch: api.branch.clone(),
            ..request.clone()
        };
        api.send(
            api.request(Method::POST, &api.repo_route("/changesets")?)
                .json(&request),
        )?
        .json()
    }

    fn path_history(&self, path: &str) -> Result<Vec<api::Revision>> {
        let api = &self.api;
        let page: api::HistoryPage = api
            .send(
                api.on_branch(
                    api.get(&api.repo_route("/history")?)
                        .query(&[("path", path)]),
                ),
            )?
            .json()?;
        Ok(page.revisions)
    }

    fn repo_history(&self, filter: Option<&str>, limit: usize) -> Result<Vec<api::Revision>> {
        let api = &self.api;
        let mut revs: Vec<api::Revision> = Vec::new();
        let mut before: Option<String> = None;
        while revs.len() < limit {
            let mut query = vec![("limit", (limit - revs.len()).min(200).to_string())];
            query.extend(filter.map(|f| ("filter", f.to_string())));
            query.extend(before.take().map(|b| ("before", b)));
            let page: api::HistoryPage = api
                .send(api.on_branch(api.get(&api.repo_route("/history")?).query(&query)))?
                .json()?;
            revs.extend(page.revisions);
            before = page.next_cursor;
            if before.is_none() {
                break;
            }
        }
        Ok(revs)
    }

    fn restore(&self, request: &api::RestoreRequest) -> Result<api::Revision> {
        let api = &self.api;
        let request = api::RestoreRequest {
            branch: api.branch.clone(),
            ..request.clone()
        };
        api.send(
            api.request(Method::POST, &api.repo_route("/restore")?)
                .json(&request),
        )?
        .json()
    }
}

//! Commit references and whole-tree snapshots as the server reports them.

use anyhow::{Result, bail};
use pyn_proto as api;

use crate::client::Api;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitRef {
    Number(u64),
    /// `HEAD~n`, counting back from the newest commit.
    Head(u64),
}

/// `HEAD`, `HEAD~n` or `cN`; with `bare`, also a plain number.
pub fn parse_commit(s: &str, bare: bool) -> Option<CommitRef> {
    if s == "HEAD" {
        return Some(CommitRef::Head(0));
    }
    if let Some(n) = s.strip_prefix("HEAD~") {
        return n.parse().ok().map(CommitRef::Head);
    }
    let digits = match s.strip_prefix('c') {
        Some(rest) => rest,
        None if bare => s,
        None => return None,
    };
    digits.parse().ok().map(CommitRef::Number)
}

/// The newest commit number; 0 when the repository has none.
pub fn head_commit(api: &Api) -> Result<u64> {
    let page: api::CommitPage = api
        .send(api.get(&api.repo_route("/commits")?).query(&[("limit", 1)]))?
        .json()?;
    Ok(page.commits.first().map_or(0, |c| c.id))
}

pub fn resolve(api: &Api, text: &str) -> Result<u64> {
    let Some(commit) = parse_commit(text, true) else {
        bail!("{text} is not a commit; use HEAD, HEAD~N or cN");
    };
    match commit {
        CommitRef::Number(n) => Ok(n),
        CommitRef::Head(back) => {
            let head = head_commit(api)?;
            if head == 0 {
                bail!("the repository has no commits yet");
            }
            if back >= head {
                bail!("{text} is before the first commit (HEAD is c{head})");
            }
            Ok(head - back)
        }
    }
}

/// Every path as of `commit` (the newest if `None`), and the commit number the server used.
pub fn fetch(api: &Api, commit: Option<u64>) -> Result<(u64, Vec<api::SnapshotEntry>)> {
    let mut all = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let mut req = api.get(&api.repo_route("/snapshot")?);
        if let Some(c) = commit {
            req = req.query(&[("commit", c)]);
        }
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

pub fn commit_page(api: &Api, before: Option<u64>, limit: usize) -> Result<api::CommitPage> {
    let mut req = api
        .get(&api.repo_route("/commits")?)
        .query(&[("limit", limit)]);
    if let Some(b) = before {
        req = req.query(&[("before", b)]);
    }
    Ok(api.send(req)?.json()?)
}

pub fn commit(api: &Api, id: u64) -> Result<api::Commit> {
    Ok(api
        .send(api.get(&api.repo_route(&format!("/commits/{id}"))?))?
        .json()?)
}

/// A file's content at a revision; `probe` fetches only the first `len` bytes when the server can.
pub fn content(api: &Api, path: &str, revision: u64, probe: Option<usize>) -> Result<Vec<u8>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_references_parse_like_git_ones() {
        assert_eq!(parse_commit("HEAD", false), Some(CommitRef::Head(0)));
        assert_eq!(parse_commit("HEAD~3", false), Some(CommitRef::Head(3)));
        assert_eq!(parse_commit("c12", false), Some(CommitRef::Number(12)));
        assert_eq!(parse_commit("12", false), None);
        assert_eq!(parse_commit("12", true), Some(CommitRef::Number(12)));
        assert_eq!(parse_commit("HEAD~x", true), None);
        assert_eq!(parse_commit("src/a.txt", true), None);
    }
}

//! The Git LFS Locks API on the Batch client's endpoint and credentials: create, list, verify, unlock and force unlock.

use std::io;

use reqwest::Url;
use reqwest::blocking::RequestBuilder;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::Deserialize;

use crate::error::Result;
use crate::lfs::{Client, MEDIA, lfs, status_error};
use crate::url::redact_text;

const MAX_PAGES: usize = 10_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Owner {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Lock {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub locked_at: String,
    #[serde(default)]
    pub owner: Owner,
}

/// The locks `verify` splits by holder: those the caller holds and those others hold.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Verified {
    pub ours: Vec<Lock>,
    pub theirs: Vec<Lock>,
}

#[derive(Deserialize)]
struct One {
    lock: Lock,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    locks: Vec<Lock>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct VerifyPage {
    #[serde(default)]
    ours: Vec<Lock>,
    #[serde(default)]
    theirs: Vec<Lock>,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct Conflict {
    lock: Option<Lock>,
    message: Option<String>,
}

fn with_ref(mut body: serde_json::Value, refname: Option<&str>) -> serde_json::Value {
    if let Some(name) = refname {
        body["ref"] = serde_json::json!({ "name": name });
    }
    body
}

impl Client {
    fn locks_url(&self, tail: &str) -> Url {
        let mut url = self.base.clone();
        let path = format!("{}/locks{tail}", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url
    }

    /// Sends one Locks API request and returns the status with the body text.
    fn locks_call(&mut self, url: &Url, body: Option<String>) -> Result<(u16, String)> {
        let resp = self.send(url, &[], &|c| -> io::Result<RequestBuilder> {
            let req = match &body {
                Some(b) => c.post(url.clone()).body(b.clone()),
                None => c.get(url.clone()),
            };
            Ok(req.header(ACCEPT, MEDIA).header(CONTENT_TYPE, MEDIA))
        })?;
        let code = resp.status().as_u16();
        Ok((code, resp.text().unwrap_or_default()))
    }

    fn fail(&self, code: u16, text: &str) -> crate::error::Error {
        status_error(code, text, &self.shown())
    }

    /// Takes the lock on `path`. A path that is already locked fails, naming the holder.
    pub fn create_lock(&mut self, path: &str, refname: Option<&str>) -> Result<Lock> {
        let url = self.locks_url("");
        let body = with_ref(serde_json::json!({ "path": path }), refname).to_string();
        let (code, text) = self.locks_call(&url, Some(body))?;
        match code {
            200 | 201 => Ok(parse::<One>(&text)?.lock),
            409 => {
                let c: Conflict = serde_json::from_str(&text).unwrap_or(Conflict {
                    lock: None,
                    message: None,
                });
                Err(lfs(match c.lock {
                    Some(l) => format!(
                        "'{path}' is already locked by {}",
                        redact_text(&l.owner.name)
                    ),
                    None => format!(
                        "'{path}' could not be locked: {}",
                        redact_text(c.message.as_deref().unwrap_or("conflict"))
                    ),
                }))
            }
            _ => Err(self.fail(code, &text)),
        }
    }

    /// Every lock, optionally narrowed to one `path` or `id`.
    pub fn list_locks(
        &mut self,
        path: Option<&str>,
        id: Option<&str>,
        refname: Option<&str>,
    ) -> Result<Vec<Lock>> {
        let (mut all, mut cursor) = (Vec::new(), None::<String>);
        for _ in 0..MAX_PAGES {
            let mut url = self.locks_url("");
            {
                let mut q = url.query_pairs_mut();
                for (k, v) in [
                    ("path", path),
                    ("id", id),
                    ("refspec", refname),
                    ("cursor", cursor.as_deref()),
                ] {
                    if let Some(v) = v {
                        q.append_pair(k, v);
                    }
                }
            }
            let (code, text) = self.locks_call(&url, None)?;
            if code != 200 {
                return Err(self.fail(code, &text));
            }
            let page: Page = parse(&text)?;
            all.extend(page.locks);
            match page.next_cursor.filter(|c| !c.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(all)
    }

    /// The locks split into ours and theirs; `None` when the server does not implement `verify`.
    pub fn verify_locks(&mut self, refname: Option<&str>) -> Result<Option<Verified>> {
        let url = self.locks_url("/verify");
        let (mut out, mut cursor) = (Verified::default(), None::<String>);
        for _ in 0..MAX_PAGES {
            let mut body = with_ref(serde_json::json!({}), refname);
            if let Some(c) = &cursor {
                body["cursor"] = serde_json::json!(c);
            }
            let (code, text) = self.locks_call(&url, Some(body.to_string()))?;
            match code {
                200 => {}
                404 | 501 => return Ok(None),
                _ => return Err(self.fail(code, &text)),
            }
            let page: VerifyPage = parse(&text)?;
            out.ours.extend(page.ours);
            out.theirs.extend(page.theirs);
            match page.next_cursor.filter(|c| !c.is_empty()) {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(Some(out))
    }

    /// Removes the lock `id`; `force` removes another user's.
    pub fn unlock(&mut self, id: &str, force: bool, refname: Option<&str>) -> Result<Lock> {
        let mut url = self.locks_url("");
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty().push(id).push("unlock");
        }
        let body = with_ref(serde_json::json!({ "force": force }), refname).to_string();
        let (code, text) = self.locks_call(&url, Some(body))?;
        match code {
            200 => Ok(parse::<One>(&text)?.lock),
            _ => Err(self.fail(code, &text)),
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| {
        lfs(format!(
            "the LFS server sent an unreadable locks response: {e}"
        ))
    })
}

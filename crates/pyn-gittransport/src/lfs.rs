//! A Git LFS Batch API client with the `basic` transfer: endpoint resolution, credentials through git's credential
//! flow, verified resumable downloads and uploads. No LFS program is involved.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, mpsc};

use pyn_git::Config;
use reqwest::Url;
use reqwest::blocking::{Body, Client as Http, RequestBuilder, Response};
use reqwest::header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, RANGE};
use reqwest::redirect::Policy;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::credential::{Flow, Offer, Prompt};
use crate::env::Env;
use crate::error::{Error, Result};
use crate::http::build_client;
use crate::url::{RemoteUrl, redact_text, rewrite, scoped_values};

pub(crate) const MEDIA: &str = "application/vnd.git-lfs+json";
const BATCH_SIZE: usize = 100;
const MAX_REDIRECTS: usize = 10;
const DEFAULT_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub oid: String,
    pub size: u64,
}

pub(crate) fn lfs(msg: impl Into<String>) -> Error {
    Error::Lfs(msg.into())
}

/// The LFS endpoint for `remote`: `remote.<name>.lfspushurl` and `lfs.pushurl` (pushing only), `remote.<name>.lfsurl`,
/// `lfs.url`, else derived from the remote's own URL by appending `.git/info/lfs`.
pub fn endpoint(
    config: &Config,
    remote: Option<&str>,
    remote_url: Option<&str>,
    push: bool,
) -> Result<Url> {
    let keyed = |k: &str| remote.and_then(|r| config.get(&format!("remote.{r}.{k}")));
    let explicit = push
        .then(|| keyed("lfspushurl").or_else(|| config.get("lfs.pushurl")))
        .flatten()
        .or_else(|| keyed("lfsurl"))
        .or_else(|| config.get("lfs.url"))
        .filter(|v| !v.is_empty());
    let url = match explicit {
        Some(raw) => Url::parse(raw).map_err(|_| lfs("lfs.url is not a valid URL"))?,
        None => {
            let remote_url = remote_url.ok_or_else(|| {
                lfs("no LFS endpoint: the repository has no remote and lfs.url is not set")
            })?;
            let RemoteUrl::Http(mut url) = RemoteUrl::parse(&rewrite(config, remote_url, push))?
            else {
                return Err(lfs(format!(
                    "no LFS endpoint can be derived from '{}': set lfs.url",
                    redact_text(remote_url)
                )));
            };
            let path = url.path().trim_end_matches('/').to_owned();
            let git = if path.ends_with(".git") { "" } else { ".git" };
            url.set_path(&format!("{path}{git}/info/lfs"));
            url.set_query(None);
            url.set_fragment(None);
            url
        }
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Err(lfs(format!(
            "the LFS endpoint must be http or https, not {}",
            url.scheme()
        )));
    }
    Ok(url)
}

#[derive(Deserialize)]
struct BatchResponse {
    transfer: Option<String>,
    #[serde(default)]
    objects: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    oid: String,
    #[serde(default)]
    actions: HashMap<String, Action>,
    error: Option<ObjectError>,
}

#[derive(Deserialize)]
struct Action {
    href: String,
    #[serde(default)]
    header: HashMap<String, String>,
}

#[derive(Deserialize)]
struct ObjectError {
    code: Option<u16>,
    message: Option<String>,
}

type Origin = (String, String, Option<u16>);

fn origin(u: &Url) -> Origin {
    (
        u.scheme().to_owned(),
        u.host_str().unwrap_or("").to_ascii_lowercase(),
        u.port_or_known_default(),
    )
}

/// Credential state shared by the transfer threads; `generation` counts credential changes.
struct Session {
    flow: Flow,
    auth: Option<Offer>,
    unconfirmed: bool,
    generation: u64,
}

pub struct Client {
    http: Http,
    pub(crate) base: Url,
    session: Mutex<Session>,
    extra_headers: Vec<(String, String)>,
    concurrency: usize,
}

/// `lfs.concurrenttransfers`: a positive integer, 8 when unset.
fn concurrency(config: &Config) -> Result<usize> {
    match config.get("lfs.concurrenttransfers") {
        None => Ok(DEFAULT_CONCURRENCY),
        Some(v) => v
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                lfs(format!(
                    "lfs.concurrenttransfers must be a positive integer, not '{v}'"
                ))
            }),
    }
}

/// Runs `work` over `jobs` on up to `workers` scoped threads, handing each result to `done` on the calling thread
/// as it arrives. One worker runs inline.
fn pool<T: Sync, R: Send>(
    jobs: &[T],
    workers: usize,
    work: &(dyn Fn(&T) -> R + Sync),
    mut done: impl FnMut(usize, R),
) {
    if workers <= 1 || jobs.len() <= 1 {
        for (i, job) in jobs.iter().enumerate() {
            done(i, work(job));
        }
        return;
    }
    let next = &AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        for _ in 0..workers.min(jobs.len()) {
            let tx = tx.clone();
            s.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(job) = jobs.get(i) else { break };
                    if tx.send((i, work(job))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        for (i, r) in rx {
            done(i, r);
        }
    });
}

impl Client {
    /// `base` is the endpoint (ending in `info/lfs`); a user name and password in it are offered first.
    pub fn new(base: Url, config: &Config, env: &Env, prompt: Box<dyn Prompt>) -> Result<Self> {
        let http = build_client(config, env, &base, Policy::limited(MAX_REDIRECTS))?;
        let extra_headers = scoped_values(config, "http", "extraheader", &base)
            .into_iter()
            .filter_map(|h| h.split_once(':'))
            .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
            .collect();
        let concurrency = concurrency(config)?;
        let flow = Flow::new(config.clone(), env.clone(), prompt, &base);
        let mut base = base;
        let _ = base.set_username("");
        let _ = base.set_password(None);
        Ok(Self {
            http,
            base,
            session: Mutex::new(Session {
                flow,
                auth: None,
                unconfirmed: false,
                generation: 0,
            }),
            extra_headers,
            concurrency,
        })
    }

    fn session(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn shown(&self) -> String {
        RemoteUrl::Http(self.base.clone()).display()
    }

    /// Sends the request `make` builds; credentials go only to the endpoint's host, and a 401 asks the next source.
    pub(crate) fn send(
        &self,
        url: &Url,
        headers: &[(String, String)],
        make: &dyn Fn(&Http) -> io::Result<RequestBuilder>,
    ) -> Result<Response> {
        let own_auth = headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("authorization"));
        let home = origin(url) == origin(&self.base);
        loop {
            let (auth, generation) = {
                let st = self.session();
                (st.auth.clone(), st.generation)
            };
            let mut req = make(&self.http)?;
            for (n, v) in headers {
                req = req.header(n.as_str(), v.as_str());
            }
            if home {
                for (n, v) in &self.extra_headers {
                    req = req.header(n.as_str(), v.as_str());
                }
                if let (false, Some(a)) = (own_auth, &auth) {
                    req = req.basic_auth(
                        a.cred.username.as_deref().unwrap_or(""),
                        a.cred.password.as_deref(),
                    );
                }
            }
            let resp = req.send().map_err(|e| {
                lfs(format!(
                    "{}: {}",
                    RemoteUrl::Http(url.clone()).display(),
                    redact_text(&e.without_url().to_string())
                ))
            })?;
            let status = resp.status();
            if status.as_u16() == 401 && home && !own_auth {
                let mut st = self.session();
                if st.generation == generation {
                    if let Some(a) = st.auth.take() {
                        st.flow.reject(&a, &self.base);
                    }
                    match st.flow.next(&self.base)? {
                        Some(offer) => {
                            st.auth = Some(offer);
                            st.unconfirmed = true;
                            st.generation += 1;
                        }
                        None => {
                            return Err(lfs(format!(
                                "Authentication failed for '{}'",
                                self.shown()
                            )));
                        }
                    }
                }
                continue;
            }
            if status.is_success() {
                let mut st = self.session();
                if st.unconfirmed && st.generation == generation {
                    st.unconfirmed = false;
                    if let Some(a) = st.auth.clone() {
                        st.flow.approve(&a, &self.base);
                    }
                }
            }
            return Ok(resp);
        }
    }

    fn batch(&self, operation: &str, objects: &[Object]) -> Result<Vec<Entry>> {
        let mut url = self.base.clone();
        let path = format!("{}/objects/batch", url.path().trim_end_matches('/'));
        url.set_path(&path);
        let body = serde_json::json!({
            "operation": operation,
            "transfers": ["basic"],
            "hash_algo": "sha256",
            "objects": objects.iter().map(|o| serde_json::json!({"oid": o.oid, "size": o.size})).collect::<Vec<_>>(),
        })
        .to_string();
        let resp = self.send(&url, &[], &|c| {
            Ok(c.post(url.clone())
                .header(ACCEPT, MEDIA)
                .header(CONTENT_TYPE, MEDIA)
                .body(body.clone()))
        })?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().unwrap_or_default();
            return Err(status_error(status.as_u16(), &text, &self.shown()));
        }
        let parsed: BatchResponse = serde_json::from_reader(resp).map_err(|e| {
            lfs(format!(
                "the LFS server sent an unreadable batch response: {e}"
            ))
        })?;
        if parsed.transfer.as_deref().is_some_and(|t| t != "basic") {
            return Err(lfs(
                "the LFS server chose a transfer adapter other than basic",
            ));
        }
        Ok(parsed.objects)
    }

    fn action_url(&self, action: &Action) -> Result<Url> {
        let url =
            Url::parse(&action.href).map_err(|_| lfs("the LFS server sent an invalid URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || (self.base.scheme() == "https" && url.scheme() != "https")
        {
            return Err(lfs(format!(
                "the LFS server sent a {} URL, which pyn does not use",
                url.scheme()
            )));
        }
        Ok(url)
    }

    /// Downloads `objects` into `tmp`, verifies size and sha256 and hands each to `install`. Every object that can be
    /// had is installed; the error names the ones that could not.
    pub fn download(
        &mut self,
        objects: &[Object],
        tmp: &Path,
        install: &mut dyn FnMut(&Object, &Path) -> Result<()>,
    ) -> Result<()> {
        std::fs::create_dir_all(tmp)?;
        let mut failures = Vec::new();
        for chunk in unique(objects).chunks(BATCH_SIZE) {
            let entries = self.batch("download", chunk)?;
            let mut outcomes: Vec<Option<Error>> = chunk.iter().map(|_| None).collect();
            let mut jobs = Vec::new();
            for (i, object) in chunk.iter().enumerate() {
                match entries.iter().find(|e| e.oid == object.oid) {
                    None => outcomes[i] = Some(lfs("the LFS server did not answer for it")),
                    Some(e) => match (&e.error, e.actions.get("download")) {
                        (Some(err), _) => outcomes[i] = Some(object_error(err)),
                        (None, None) => {
                            outcomes[i] = Some(lfs("the LFS server offers no download for it"));
                        }
                        (None, Some(action)) => jobs.push((i, object, action)),
                    },
                }
            }
            pool(
                &jobs,
                self.concurrency,
                &|(_, object, action)| self.fetch_one(object, action, tmp),
                |n, fetched| {
                    let (i, object, _) = jobs[n];
                    outcomes[i] = fetched.and_then(|path| install(object, &path)).err();
                },
            );
            for (object, outcome) in chunk.iter().zip(outcomes) {
                if let Some(e) = outcome {
                    failures.push(format!("{}: {e}", object.oid));
                }
            }
        }
        summarize(failures)
    }

    fn fetch_one(&self, object: &Object, action: &Action, tmp: &Path) -> Result<PathBuf> {
        let url = self.action_url(action)?;
        let headers: Vec<_> = action.header.clone().into_iter().collect();
        let part = tmp.join(format!("{}.part", object.oid));
        for attempt in 0..2 {
            let mut have = std::fs::metadata(&part).map_or(0, |m| m.len());
            if have > object.size {
                let _ = std::fs::remove_file(&part);
                have = 0;
            }
            if have < object.size || object.size == 0 {
                let resp = self.send(&url, &headers, &|c| {
                    let r = c.get(url.clone());
                    Ok(if have > 0 {
                        r.header(RANGE, format!("bytes={have}-"))
                    } else {
                        r
                    })
                })?;
                match resp.status().as_u16() {
                    200 => write_body(resp, &part, false)?,
                    206 if have > 0 => write_body(resp, &part, true)?,
                    416 if attempt == 0 => {
                        let _ = std::fs::remove_file(&part);
                        continue;
                    }
                    code => return Err(status_error(code, "", &RemoteUrl::Http(url).display())),
                }
            }
            if verify(&part, object)? {
                return Ok(part);
            }
            let _ = std::fs::remove_file(&part);
            if attempt == 1 {
                return Err(lfs(
                    "the downloaded content does not match its size and sha256",
                ));
            }
        }
        Err(lfs("the download could not be completed"))
    }

    /// Offers `objects` to the server and uploads those it lacks, reading each through `open`. Returns how many were
    /// sent.
    pub fn upload(
        &mut self,
        objects: &[Object],
        open: &(dyn Fn(&Object) -> io::Result<File> + Sync),
    ) -> Result<usize> {
        let (mut failures, mut sent) = (Vec::new(), 0);
        for chunk in unique(objects).chunks(BATCH_SIZE) {
            let entries = self.batch("upload", chunk)?;
            let mut outcomes: Vec<Option<Error>> = chunk.iter().map(|_| None).collect();
            let mut jobs = Vec::new();
            for (i, object) in chunk.iter().enumerate() {
                let Some(entry) = entries.iter().find(|e| e.oid == object.oid) else {
                    outcomes[i] = Some(lfs("the LFS server did not answer for it"));
                    continue;
                };
                if let Some(err) = &entry.error {
                    outcomes[i] = Some(object_error(err));
                } else if let Some(put) = entry.actions.get("upload") {
                    jobs.push((i, object, put, entry.actions.get("verify")));
                }
            }
            pool(
                &jobs,
                self.concurrency,
                &|(_, object, put, verify)| self.put_one(object, put, *verify, open),
                |n, put| {
                    match put {
                        Ok(()) => sent += 1,
                        Err(e) => outcomes[jobs[n].0] = Some(e),
                    };
                },
            );
            for (object, outcome) in chunk.iter().zip(outcomes) {
                if let Some(e) = outcome {
                    failures.push(format!("{}: {e}", object.oid));
                }
            }
        }
        summarize(failures).map(|()| sent)
    }

    fn put_one(
        &self,
        object: &Object,
        put: &Action,
        verify: Option<&Action>,
        open: &(dyn Fn(&Object) -> io::Result<File> + Sync),
    ) -> Result<()> {
        let url = self.action_url(put)?;
        let headers: Vec<_> = put.header.clone().into_iter().collect();
        let resp = self.send(&url, &headers, &|c| {
            Ok(c.put(url.clone())
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_LENGTH, object.size)
                .body(Body::sized(open(object)?, object.size)))
        })?;
        if !resp.status().is_success() {
            let code = resp.status().as_u16();
            let text = resp.text().unwrap_or_default();
            return Err(status_error(code, &text, &RemoteUrl::Http(url).display()));
        }
        if let Some(action) = verify {
            let url = self.action_url(action)?;
            let headers: Vec<_> = action.header.clone().into_iter().collect();
            let body = serde_json::json!({"oid": object.oid, "size": object.size}).to_string();
            let resp = self.send(&url, &headers, &|c| {
                Ok(c.post(url.clone())
                    .header(ACCEPT, MEDIA)
                    .header(CONTENT_TYPE, MEDIA)
                    .body(body.clone()))
            })?;
            if !resp.status().is_success() {
                return Err(lfs(format!(
                    "the LFS server could not verify the upload (HTTP {})",
                    resp.status().as_u16()
                )));
            }
        }
        Ok(())
    }
}

/// `objects` without repeated ids, in order.
fn unique(objects: &[Object]) -> Vec<Object> {
    let mut seen = std::collections::HashSet::new();
    objects
        .iter()
        .filter(|o| seen.insert(o.oid.clone()))
        .cloned()
        .collect()
}

fn write_body(mut resp: Response, part: &Path, append: bool) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(part)?;
    io::copy(&mut resp, &mut file)?;
    file.flush()?;
    Ok(())
}

fn verify(path: &Path, object: &Object) -> Result<bool> {
    let mut file = File::open(path)?;
    let (mut hash, mut len, mut buf) = (Sha256::new(), 0u64, vec![0u8; 64 * 1024]);
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        len += n as u64;
    }
    let hex: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Ok(len == object.size && hex == object.oid)
}

fn object_error(e: &ObjectError) -> Error {
    lfs(format!(
        "{} (code {})",
        redact_text(e.message.as_deref().unwrap_or("the LFS server refused it")),
        e.code.unwrap_or(0)
    ))
}

pub(crate) fn status_error(code: u16, body: &str, what: &str) -> Error {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_owned))
        .map(|m| format!(": {}", redact_text(&m)))
        .unwrap_or_default();
    lfs(match code {
        401 => format!("Authentication failed for '{what}'"),
        403 => format!("access to '{what}' was denied (HTTP 403){message}"),
        404 => format!("'{what}' was not found (HTTP 404){message}"),
        code => format!("unexpected HTTP status {code} from '{what}'{message}"),
    })
}

fn summarize(failures: Vec<String>) -> Result<()> {
    match failures.as_slice() {
        [] => Ok(()),
        [one] => Err(lfs(one.clone())),
        [first, rest @ ..] => Err(lfs(format!("{first} (and {} more failed)", rest.len()))),
    }
}

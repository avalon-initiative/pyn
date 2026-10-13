//! Git's smart HTTP: `GET info/refs`, then one `POST git-<service>` per request. Dumb HTTP is not supported.

use std::io::{self, Cursor, Read};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pyn_git::Config;
use pyn_gitwire::{Error as WireError, ProtocolVersion, Result, Service, Session, Transport};
use reqwest::Url;
use reqwest::blocking::{Body, Client, RequestBuilder, Response};
use reqwest::header::{ACCEPT, CONTENT_ENCODING, CONTENT_TYPE, LOCATION, WWW_AUTHENTICATE};
use reqwest::redirect::Policy;

use crate::credential::{Flow, Offer, Prompt};
use crate::env::Env;
use crate::error::{Error, Result as CrateResult};
use crate::url::{RemoteUrl, redact_text, scoped_value, scoped_values};

/// Sent as `User-Agent`. The `git/` prefix is what hosts that gate smart HTTP on a git client look for.
pub const USER_AGENT: &str = concat!("git/2.0 (pyn/", env!("CARGO_PKG_VERSION"), ")");

/// Bodies up to this size are sent whole (and may be retried after a 401); larger ones stream.
const POST_BUFFER: usize = 1 << 20;
const GZIP_ABOVE: usize = 1024;
const MAX_REDIRECTS: usize = 20;
const CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
    Never,
    Initial,
    Always,
}

type Origin = (String, String, Option<u16>);

fn origin(u: &Url) -> Origin {
    (
        u.scheme().to_owned(),
        u.host_str().unwrap_or("").to_ascii_lowercase(),
        u.port_or_known_default(),
    )
}

struct Shared {
    client: Client,
    flow: Mutex<Flow>,
    extra_headers: Vec<(String, String)>,
    follow: Follow,
    home: Origin,
    /// The credential the server last accepted, tried first by the next session.
    accepted: Mutex<Option<Auth>>,
}

pub struct HttpTransport {
    shared: Arc<Shared>,
    url: Url,
}

impl HttpTransport {
    pub fn new(url: Url, config: &Config, env: &Env, prompt: Box<dyn Prompt>) -> CrateResult<Self> {
        let client = build_client(config, env, &url)?;
        let extra_headers = scoped_values(config, "http", "extraheader", &url)
            .into_iter()
            .map(|h| {
                h.split_once(':')
                    .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
                    .ok_or_else(|| {
                        Error::Url("http.extraHeader needs the form 'Name: value'".into())
                    })
            })
            .collect::<CrateResult<Vec<_>>>()?;
        let follow = match scoped_value(config, "http", "followredirects", &url) {
            Some(v) if v.eq_ignore_ascii_case("initial") => Follow::Initial,
            Some(v) if crate::url::is_false(v) => Follow::Never,
            Some(_) => Follow::Always,
            None => Follow::Initial,
        };
        let flow = Flow::new(config.clone(), env.clone(), prompt, &url);
        let mut url = url;
        let _ = url.set_username("");
        let _ = url.set_password(None);
        Ok(Self {
            shared: Arc::new(Shared {
                client,
                flow: Mutex::new(flow),
                extra_headers,
                follow,
                home: origin(&url),
                accepted: Mutex::new(None),
            }),
            url,
        })
    }
}

fn build_client(config: &Config, env: &Env, url: &Url) -> CrateResult<Client> {
    let mut b = Client::builder()
        .redirect(Policy::none())
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .timeout(None);
    if let Some(p) = scoped_value(config, "http", "proxy", url) {
        b = if p.is_empty() {
            b.no_proxy()
        } else {
            b.proxy(reqwest::Proxy::all(p).map_err(|_| Error::Url("invalid http.proxy".into()))?)
        };
    }
    let verify = env.get("GIT_SSL_NO_VERIFY").is_none()
        && scoped_value(config, "http", "sslverify", url).is_none_or(|v| !crate::url::is_false(v));
    if !verify {
        b = b.danger_accept_invalid_certs(true);
    }
    let ca = env
        .non_empty("GIT_SSL_CAINFO")
        .or_else(|| scoped_value(config, "http", "sslcainfo", url));
    if let Some(path) = ca {
        let pem = std::fs::read(path)
            .map_err(|e| Error::Url(format!("cannot read http.sslCAInfo '{path}': {e}")))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|_| Error::Url(format!("'{path}' holds no usable certificates")))?;
        for c in certs {
            b = b.add_root_certificate(c);
        }
    }
    b.build()
        .map_err(|e| Error::Url(redact_text(&e.without_url().to_string())))
}

/// An accepted credential and the host it is for; it is attached to no other host.
#[derive(Clone)]
struct Auth {
    offer: Offer,
    origin: Origin,
}

fn net_error(e: reqwest::Error, url: &Url) -> WireError {
    let mut msg = String::new();
    let mut cur: Option<&dyn std::error::Error> = Some(&e);
    while let Some(err) = cur {
        if !msg.is_empty() {
            msg.push_str(": ");
        }
        msg.push_str(&err.to_string());
        cur = err.source();
    }
    WireError::Connection(format!(
        "{}: {}",
        RemoteUrl::Http(url.clone()).display(),
        redact_text(&msg)
    ))
}

fn status_error(status: reqwest::StatusCode, url: &Url) -> WireError {
    let shown = RemoteUrl::Http(url.clone()).display();
    match status.as_u16() {
        401 => WireError::Auth(format!("Authentication failed for '{shown}'")),
        403 => WireError::Auth(format!("access to '{shown}' was denied (HTTP 403)")),
        404 => WireError::NotFound(format!("repository '{shown}' not found")),
        code => WireError::Connection(format!("unexpected HTTP status {code} from '{shown}'")),
    }
}

impl Shared {
    fn request(
        &self,
        method: reqwest::Method,
        url: &Url,
        auth: Option<&Auth>,
        version: ProtocolVersion,
    ) -> RequestBuilder {
        let mut req = self.client.request(method, url.clone());
        match version {
            ProtocolVersion::V0 => {}
            ProtocolVersion::V1 => req = req.header("Git-Protocol", "version=1"),
            ProtocolVersion::V2 => req = req.header("Git-Protocol", "version=2"),
        }
        if origin(url) == self.home {
            for (n, v) in &self.extra_headers {
                req = req.header(n.as_str(), v.as_str());
            }
        }
        match auth {
            Some(a) if a.origin == origin(url) => req.basic_auth(
                a.offer.cred.username.as_deref().unwrap_or(""),
                a.offer.cred.password.as_deref(),
            ),
            _ => req,
        }
    }

    /// The next credential to try after a 401 from `url`, rejecting the one that just failed.
    fn after_401(&self, resp: &Response, base: &Url, failed: Option<Auth>) -> Result<Auth> {
        if let Some(a) = &failed {
            self.flow.lock().expect("flow").reject(&a.offer, base);
        }
        let challenge = resp
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>();
        if !challenge.is_empty()
            && !challenge
                .iter()
                .any(|c| c.trim_start().to_ascii_lowercase().starts_with("basic"))
        {
            return Err(WireError::Auth(format!(
                "'{}' asks for an authentication scheme pyn does not support (only Basic): {}",
                RemoteUrl::Http(base.clone()).display(),
                redact_text(&challenge.join(", "))
            )));
        }
        let next = self.flow.lock().expect("flow").next(base)?;
        match next {
            Some(offer) => Ok(Auth {
                offer,
                origin: origin(base),
            }),
            None => Err(status_error(reqwest::StatusCode::UNAUTHORIZED, base)),
        }
    }

    fn approve(&self, auth: &Option<Auth>, url: &Url) {
        *self.accepted.lock().expect("accepted") = auth.clone();
        if let Some(a) = auth {
            self.flow.lock().expect("flow").approve(&a.offer, url);
        }
    }
}

impl Transport for HttpTransport {
    fn open(&mut self, service: Service, version: ProtocolVersion) -> Result<Box<dyn Session>> {
        let sh = &self.shared;
        let mut base = self.url.clone();
        let mut auth: Option<Auth> = sh
            .accepted
            .lock()
            .expect("accepted")
            .clone()
            .filter(|a| a.origin == origin(&base));
        let mut redirects = 0;
        loop {
            let url = info_refs_url(&base, service);
            let resp = sh
                .request(reqwest::Method::GET, &url, auth.as_ref(), version)
                .header(ACCEPT, "*/*")
                .send()
                .map_err(|e| net_error(e.without_url(), &url))?;
            let status = resp.status();
            if status.is_redirection() {
                if sh.follow == Follow::Never {
                    return Err(WireError::Connection(format!(
                        "'{}' redirects, and http.followRedirects is off",
                        RemoteUrl::Http(url).display()
                    )));
                }
                redirects += 1;
                let target = redirect_target(&resp, &url, redirects)?;
                base = rebase(&target, &base)?;
                if auth.as_ref().is_some_and(|a| a.origin != origin(&base)) {
                    auth = None;
                }
                continue;
            }
            if status.as_u16() == 401 {
                auth = Some(sh.after_401(&resp, &base, auth.take())?);
                continue;
            }
            if !status.is_success() {
                return Err(status_error(status, &url));
            }
            check_advertisement_type(&resp, &url, service)?;
            sh.approve(&auth, &base);
            let adv = strip_preamble(resp, service)?;
            return Ok(Box::new(HttpSession {
                shared: sh.clone(),
                base,
                service,
                version,
                auth,
                advert: Some(adv),
                current: None,
            }));
        }
    }
}

fn info_refs_url(base: &Url, service: Service) -> Url {
    let mut u = base.clone();
    let path = format!("{}/info/refs", u.path().trim_end_matches('/'));
    u.set_path(&path);
    u.set_query(Some(&format!("service={}", service.name())));
    u.set_fragment(None);
    u
}

fn redirect_target(resp: &Response, from: &Url, count: usize) -> Result<Url> {
    if count > MAX_REDIRECTS {
        return Err(WireError::Connection("too many redirects".into()));
    }
    let loc = resp
        .headers()
        .get(LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| WireError::Connection("a redirect without a Location".into()))?;
    let target = from
        .join(loc)
        .map_err(|_| WireError::Connection("a redirect to an invalid URL".into()))?;
    if from.scheme() == "https" && target.scheme() != "https" {
        return Err(WireError::Connection(format!(
            "refusing a redirect from https to {} ({})",
            target.scheme(),
            RemoteUrl::Http(from.clone()).display()
        )));
    }
    Ok(target)
}

/// The repository URL a redirected `info/refs` request implies: the target minus `/info/refs` and its query.
fn rebase(target: &Url, old: &Url) -> Result<Url> {
    let path = target.path();
    let Some(prefix) = path.strip_suffix("/info/refs") else {
        return Err(WireError::Connection(format!(
            "'{}' redirects to a URL that does not end in /info/refs; unable to update the URL base",
            RemoteUrl::Http(old.clone()).display()
        )));
    };
    let mut u = target.clone();
    u.set_path(prefix);
    u.set_query(None);
    Ok(u)
}

fn content_type(resp: &Response) -> String {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn check_advertisement_type(resp: &Response, url: &Url, service: Service) -> Result<()> {
    let want = format!("application/x-{}-advertisement", service.name());
    let got = content_type(resp);
    if got == want {
        return Ok(());
    }
    Err(WireError::Unsupported(format!(
        "smart HTTP at '{}' (it answered with content type '{}'; dumb HTTP remotes are not supported)",
        RemoteUrl::Http(url.clone()).display(),
        redact_text(&got)
    )))
}

/// Drops the `# service=<name>` line and its flush, which v0/v1 servers put before the advertisement.
fn strip_preamble(mut resp: Response, service: Service) -> Result<Box<dyn Read>> {
    let mut head = [0u8; 4];
    resp.read_exact(&mut head)
        .map_err(|e| WireError::Protocol(format!("the server sent no advertisement: {e}")))?;
    let len = std::str::from_utf8(&head)
        .ok()
        .and_then(|h| usize::from_str_radix(h, 16).ok())
        .unwrap_or(0);
    if len <= 4 {
        return Ok(Box::new(Cursor::new(head.to_vec()).chain(resp)));
    }
    let mut payload = vec![0u8; len - 4];
    resp.read_exact(&mut payload)?;
    let Some(name) = payload.strip_prefix(b"# service=") else {
        let mut seen = head.to_vec();
        seen.extend(payload);
        return Ok(Box::new(Cursor::new(seen).chain(resp)));
    };
    if String::from_utf8_lossy(name).trim_end() != service.name() {
        return Err(WireError::Protocol(
            "the server advertised a different service than asked for".into(),
        ));
    }
    let mut flush = [0u8; 4];
    resp.read_exact(&mut flush)?;
    if &flush != b"0000" {
        return Err(WireError::Protocol(
            "the service line is not followed by a flush".into(),
        ));
    }
    Ok(Box::new(resp))
}

struct HttpSession {
    shared: Arc<Shared>,
    base: Url,
    service: Service,
    version: ProtocolVersion,
    auth: Option<Auth>,
    advert: Option<Box<dyn Read>>,
    current: Option<Response>,
}

impl HttpSession {
    fn post_url(&self) -> Url {
        let mut u = self.base.clone();
        let path = format!("{}/{}", u.path().trim_end_matches('/'), self.service.name());
        u.set_path(&path);
        u.set_query(None);
        u
    }

    fn post(&self, url: &Url, auth: Option<&Auth>) -> RequestBuilder {
        let name = self.service.name();
        self.shared
            .request(reqwest::Method::POST, url, auth, self.version)
            .header(CONTENT_TYPE, format!("application/x-{name}-request"))
            .header(ACCEPT, format!("application/x-{name}-result"))
    }

    fn check_result(&self, resp: &Response, url: &Url) -> Result<()> {
        let want = format!("application/x-{}-result", self.service.name());
        let got = content_type(resp);
        if got != want {
            return Err(WireError::Protocol(format!(
                "'{}' answered with content type '{}' where '{want}' was expected",
                RemoteUrl::Http(url.clone()).display(),
                redact_text(&got)
            )));
        }
        Ok(())
    }

    fn post_buffered(&mut self, body: Vec<u8>) -> Result<Response> {
        let url = self.post_url();
        let (body, gzipped) = if self.service == Service::UploadPack && body.len() > GZIP_ABOVE {
            (gzip(&body)?, true)
        } else {
            (body, false)
        };
        loop {
            let mut req = self.post(&url, self.auth.as_ref()).body(body.clone());
            if gzipped {
                req = req.header(CONTENT_ENCODING, "gzip");
            }
            let resp = req.send().map_err(|e| net_error(e.without_url(), &url))?;
            let status = resp.status();
            if status.as_u16() == 401 {
                self.auth = Some(self.shared.after_401(&resp, &self.base, self.auth.take())?);
                continue;
            }
            if !status.is_success() {
                return Err(self.failure(status, &url));
            }
            self.check_result(&resp, &url)?;
            self.shared.approve(&self.auth, &self.base);
            return Ok(resp);
        }
    }

    fn post_streamed(&mut self, head: Vec<u8>, rest: &mut dyn Read) -> Result<Response> {
        let url = self.post_url();
        let (tx, rx) = sync_channel::<io::Result<Vec<u8>>>(8);
        let req = self.post(&url, self.auth.as_ref()).body(Body::new(Chunks {
            rx,
            cur: Cursor::new(Vec::new()),
        }));
        let handle = std::thread::spawn(move || req.send());
        feed(&tx, head, rest);
        drop(tx);
        let resp = handle
            .join()
            .map_err(|_| WireError::Connection("the upload thread panicked".into()))?
            .map_err(|e| net_error(e.without_url(), &url))?;
        let status = resp.status();
        if status.as_u16() == 401 {
            if let Some(a) = self.auth.take() {
                self.shared
                    .flow
                    .lock()
                    .expect("flow")
                    .reject(&a.offer, &url);
            }
            return Err(WireError::Auth(format!(
                "the server refused the credentials while receiving a large upload to '{}'; run the command again",
                RemoteUrl::Http(url).display()
            )));
        }
        if !status.is_success() {
            return Err(self.failure(status, &url));
        }
        self.check_result(&resp, &url)?;
        self.shared.approve(&self.auth, &self.base);
        Ok(resp)
    }

    fn failure(&self, status: reqwest::StatusCode, url: &Url) -> WireError {
        if status.is_redirection() {
            return WireError::Connection(format!(
                "'{}' redirected a request that carries a body; use the final URL",
                RemoteUrl::Http(url.clone()).display()
            ));
        }
        status_error(status, url)
    }
}

fn gzip(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(data)?;
    Ok(z.finish()?)
}

/// Sends `head`, then the rest of `body`, as chunks; a read error is passed on so the request fails.
fn feed(tx: &SyncSender<io::Result<Vec<u8>>>, head: Vec<u8>, body: &mut dyn Read) {
    if tx.send(Ok(head)).is_err() {
        return;
    }
    loop {
        let mut buf = vec![0u8; CHUNK];
        match body.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                buf.truncate(n);
                if tx.send(Ok(buf)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        }
    }
}

struct Chunks {
    rx: Receiver<io::Result<Vec<u8>>>,
    cur: Cursor<Vec<u8>>,
}

impl Read for Chunks {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let n = self.cur.read(buf)?;
            if n > 0 || buf.is_empty() {
                return Ok(n);
            }
            match self.rx.recv() {
                Ok(chunk) => self.cur = Cursor::new(chunk?),
                Err(_) => return Ok(0),
            }
        }
    }
}

impl Session for HttpSession {
    fn advertisement(&mut self) -> Result<Box<dyn Read + '_>> {
        self.advert
            .take()
            .ok_or_else(|| WireError::Usage("the advertisement was already read".into()))
    }

    fn send(&mut self, body: &mut dyn Read) -> Result<()> {
        self.current = None;
        let mut head = Vec::new();
        body.take(POST_BUFFER as u64 + 1).read_to_end(&mut head)?;
        let resp = if head.len() <= POST_BUFFER {
            self.post_buffered(head)?
        } else {
            self.post_streamed(head, body)?
        };
        self.current = Some(resp);
        Ok(())
    }

    fn response(&mut self) -> Result<Box<dyn Read + '_>> {
        match self.current.as_mut() {
            Some(r) => Ok(Box::new(r)),
            None => Err(WireError::Usage("no request sent".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_refs_urls_keep_the_base_path() {
        let base = Url::parse("https://h.example/team/r.git/").unwrap();
        assert_eq!(
            info_refs_url(&base, Service::ReceivePack).as_str(),
            "https://h.example/team/r.git/info/refs?service=git-receive-pack"
        );
    }

    #[test]
    fn redirect_bases_drop_the_suffix_or_fail() {
        let old = Url::parse("https://a.example/r.git").unwrap();
        let t = Url::parse("https://b.example/x/r.git/info/refs?service=git-upload-pack").unwrap();
        assert_eq!(
            rebase(&t, &old).unwrap().as_str(),
            "https://b.example/x/r.git"
        );
        let t = Url::parse("https://b.example/login").unwrap();
        assert!(rebase(&t, &old).is_err());
    }

    #[test]
    fn user_agent_has_the_git_prefix() {
        assert!(USER_AGENT.starts_with("git/") && USER_AGENT.contains("pyn/"));
    }

    #[test]
    fn chunks_read_across_messages_and_surface_errors() {
        let (tx, rx) = sync_channel(4);
        tx.send(Ok(b"ab".to_vec())).unwrap();
        tx.send(Ok(b"cde".to_vec())).unwrap();
        tx.send(Err(io::Error::other("boom"))).unwrap();
        let mut c = Chunks {
            rx,
            cur: Cursor::new(Vec::new()),
        };
        let mut out = Vec::new();
        let err = c.read_to_end(&mut out).unwrap_err();
        assert_eq!(out, b"abcde");
        assert_eq!(err.to_string(), "boom");
    }
}

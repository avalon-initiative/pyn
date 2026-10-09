use anyhow::{Context, Result, bail};
use pyn_proto as api;
use reqwest::Method;
use reqwest::blocking::{Client, RequestBuilder, Response};

pub struct Api {
    pub http: Client,
    pub base: String,
    pub token: Option<String>,
    pub user: Option<String>,
    /// `owner/name` of the repository that repository-scoped commands act on.
    pub repo: Option<String>,
}

impl Api {
    /// The route for something inside the current repository, such as `/files`.
    pub fn repo_route(&self, tail: &str) -> Result<String> {
        let repo = self.repo.as_ref().context(
            "no repository: run this inside a workspace, or pass --repo owner/name (or set PYN_REPO)",
        )?;
        Ok(format!("/v1/repos/{repo}{tail}"))
    }

    pub fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req = self.http.request(method, format!("{}{path}", self.base));
        match (&self.token, &self.user) {
            (Some(token), _) => req.bearer_auth(token),
            (None, Some(user)) => req.header(api::DEV_USER_HEADER, user),
            _ => req,
        }
    }

    /// A request that carries no credentials, for signing in and registering.
    pub fn anonymous(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{}{path}", self.base))
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.request(Method::GET, path)
    }

    pub fn send(&self, req: RequestBuilder) -> Result<Response> {
        ok(req.send()?)
    }

    /// Every file the server lists, following pages.
    pub fn list_files(&self) -> Result<Vec<api::FileEntry>> {
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut req = self.get(&self.repo_route("/files")?);
            if let Some(a) = &after {
                req = req.query(&[("after", a)]);
            }
            let page: api::FilePage = self.send(req)?.json()?;
            all.extend(page.entries);
            match page.next_after {
                Some(next) => after = Some(next),
                None => return Ok(all),
            }
        }
    }

    /// A file's content at `revision`, with the revision the server reports.
    pub fn content(&self, path: &str, revision: Option<u64>) -> Result<Vec<u8>> {
        let mut req = self
            .get(&self.repo_route("/content")?)
            .query(&[("path", path)]);
        if let Some(r) = revision {
            req = req.query(&[("revision", r)]);
        }
        Ok(self.send(req)?.bytes()?.to_vec())
    }
}

/// Turn a non-2xx response into an error with the server's code and message.
pub fn ok(resp: Response) -> Result<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    match resp.json::<api::ErrorBody>() {
        Ok(e) => bail!(
            "{} ({}): {}",
            e.code,
            status.as_u16(),
            crate::time::localize(&e.message)
        ),
        Err(_) => bail!("server returned {status}"),
    }
}

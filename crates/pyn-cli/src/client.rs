use anyhow::{Context, Result, bail};
use pyn_proto as api;

use crate::transport::{Auth, Call, Method, Reply, Transport};

/// A session with a pyn server: the transport it travels over and who and what it acts on.
pub struct Api {
    pub transport: Box<dyn Transport>,
    pub base: String,
    pub token: Option<String>,
    pub user: Option<String>,
    /// `owner/name` of the repository that repository-scoped commands act on.
    pub repo: Option<String>,
    /// The branch the workspace is on; reads and commits go to it, and the server's default branch when `None`.
    pub branch: Option<String>,
    /// The working directory is a git repository, so no workspace can name the repository.
    pub on_git: bool,
}

impl Api {
    /// The route for something inside the current repository, such as `/files`.
    pub fn repo_route(&self, tail: &str) -> Result<String> {
        let repo = self.repo.as_ref().context(self.no_repo_message())?;
        Ok(format!("/v1/repos/{repo}{tail}"))
    }

    pub fn no_repo_message(&self) -> &'static str {
        if self.on_git {
            "no repository: this is a git repository, so pass --repo owner/name (or set PYN_REPO)"
        } else {
            "no repository: run this inside a workspace, or pass --repo owner/name (or set PYN_REPO)"
        }
    }

    pub fn request(&self, method: Method, path: &str) -> Call {
        let mut call = Call::new(method, path);
        call.auth = match (&self.token, &self.user) {
            (Some(token), _) => Auth::Token(token.clone()),
            (None, Some(user)) => Auth::DevUser(user.clone()),
            _ => Auth::Anonymous,
        };
        call
    }

    /// A request that carries no credentials, for signing in and registering.
    pub fn anonymous(&self, method: Method, path: &str) -> Call {
        Call::new(method, path)
    }

    pub fn get(&self, path: &str) -> Call {
        self.request(Method::GET, path)
    }

    /// `req` limited to the workspace's branch.
    pub fn on_branch(&self, req: Call) -> Call {
        match &self.branch {
            Some(b) => req.query(&[("branch", b)]),
            None => req,
        }
    }

    pub fn send(&self, call: Call) -> Result<Reply> {
        ok(self.transport.send(call)?)
    }

    /// Every file the server lists, following pages.
    pub fn list_files(&self) -> Result<Vec<api::FileEntry>> {
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut req = self.on_branch(self.get(&self.repo_route("/files")?));
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
}

/// A non-2xx response: the server's error code and message.
#[derive(Debug)]
pub struct ApiError {
    pub code: String,
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({}): {}", self.code, self.status, self.message)
    }
}

impl std::error::Error for ApiError {}

/// Turn a non-2xx response into an error with the server's code and message.
pub fn ok(resp: Reply) -> Result<Reply> {
    if resp.is_success() {
        return Ok(resp);
    }
    let status = resp.status;
    match resp.json::<api::ErrorBody>() {
        Ok(e) => Err(ApiError {
            code: e.code,
            status,
            message: crate::time::localize(&e.message),
        }
        .into()),
        Err(_) => bail!("server returned {}", status_line(status)),
    }
}

fn status_line(status: u16) -> String {
    match reqwest::StatusCode::from_u16(status) {
        Ok(s) => s.to_string(),
        Err(_) => status.to_string(),
    }
}

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pyn_proto as api;
use reqwest::blocking::{Client, Response};

/// pyn: version control that merges what can be merged and locks what shouldn't be.
#[derive(Parser)]
#[command(name = "pyn", version)]
struct Cli {
    #[arg(long, env = "PYN_SERVER", default_value = "http://127.0.0.1:7878")]
    server: String,
    /// Dev identity sent as X-Pyn-User.
    #[arg(long, env = "PYN_USER")]
    user: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List live locks.
    Locks,
    /// List files with their mode, head revision and lock.
    Files,
    /// Take the lock on an exclusive file.
    Checkout {
        path: String,
        /// Revision your copy is at (omit if you have none).
        #[arg(long)]
        base: Option<u64>,
    },
    /// Give up a lock without checking in.
    Release { path: String },
    /// Upload a local file as the next revision of `path`. Releases the lock.
    Checkin {
        path: String,
        /// Local file to upload.
        file: std::path::PathBuf,
        #[arg(long)]
        base: Option<u64>,
        #[arg(short, long)]
        message: String,
    },
    /// Fetch a file's content at a revision (the head if omitted).
    Get {
        path: String,
        #[arg(long)]
        rev: Option<u64>,
        /// Write to this file instead of stdout.
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
    },
    /// Show a path's revisions.
    History { path: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let http = Client::new();
    let url = |p: &str| format!("{}{p}", cli.server.trim_end_matches('/'));
    let user = cli.user.as_str();

    match cli.command {
        Command::Locks => {
            let locks: Vec<api::Lock> = ok(http.get(url("/v1/locks")).send()?)?.json()?;
            if locks.is_empty() {
                println!("no locks");
            }
            for l in locks {
                println!("{}\t{}\texpires {}", l.path, l.owner, l.expires_at);
            }
        }
        Command::Files => {
            let mut after: Option<String> = None;
            loop {
                let mut req = http.get(url("/v1/files"));
                if let Some(a) = &after {
                    req = req.query(&[("after", a)]);
                }
                let page: api::FilePage = ok(req.send()?)?.json()?;
                for f in &page.entries {
                    let rev = f.revision.map_or("-".to_string(), |r| format!("r{r}"));
                    let lock = f
                        .lock
                        .as_ref()
                        .map_or(String::new(), |l| format!("locked by {}", l.owner));
                    println!("{}\t{:?}\t{rev}\t{lock}", f.path, f.mode);
                }
                match page.next_after {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
        }
        Command::Checkout { path, base } => {
            let req = api::CheckoutRequest {
                path,
                base_revision: base,
            };
            let l: api::Lock = ok(post(&http, &url("/v1/checkout"), user, &req)?)?.json()?;
            println!("locked {} until {}", l.path, l.expires_at);
        }
        Command::Release { path } => {
            ok(post(
                &http,
                &url("/v1/release"),
                user,
                &api::ReleaseRequest { path },
            )?)?;
            println!("released");
        }
        Command::Checkin {
            path,
            file,
            base,
            message,
        } => {
            let bytes =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let put: api::PutObjectResponse = ok(http
                .put(url("/v1/objects"))
                .header(api::DEV_USER_HEADER, user)
                .body(bytes)
                .send()?)?
            .json()?;
            let req = api::CheckinRequest {
                path,
                content: put.content,
                base_revision: base,
                message,
            };
            let r: api::Revision = ok(post(&http, &url("/v1/checkin"), user, &req)?)?.json()?;
            println!("{} is now at revision {}", r.path, r.id);
        }
        Command::Get { path, rev, output } => {
            let mut req = http
                .get(url("/v1/content"))
                .query(&[("path", path.as_str())]);
            if let Some(r) = rev {
                req = req.query(&[("revision", r)]);
            }
            let resp = ok(req.send()?)?;
            let revision = resp
                .headers()
                .get(api::REVISION_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("?")
                .to_string();
            let bytes = resp.bytes()?;
            match output {
                Some(file) => {
                    std::fs::write(&file, &bytes)
                        .with_context(|| format!("writing {}", file.display()))?;
                    println!(
                        "wrote {path} at revision {revision} ({} bytes) to {}",
                        bytes.len(),
                        file.display()
                    );
                }
                None => std::io::Write::write_all(&mut std::io::stdout(), &bytes)?,
            }
        }
        Command::History { path } => {
            let revs: Vec<api::Revision> = ok(http
                .get(url("/v1/history"))
                .query(&[("path", path)])
                .send()?)?
            .json()?;
            for r in revs {
                println!("{}\t{}\t{}\t{}", r.id, r.author, r.created_at, r.message);
            }
        }
    }
    Ok(())
}

fn post(http: &Client, url: &str, user: &str, body: &impl serde::Serialize) -> Result<Response> {
    Ok(http
        .post(url)
        .header(api::DEV_USER_HEADER, user)
        .json(body)
        .send()?)
}

/// Turn a non-2xx response into an error with the server's code and message.
fn ok(resp: Response) -> Result<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    match resp.json::<api::ErrorBody>() {
        Ok(e) => bail!("{} ({}): {}", e.code, status.as_u16(), e.message),
        Err(_) => bail!("server returned {status}"),
    }
}

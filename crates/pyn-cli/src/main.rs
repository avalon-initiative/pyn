use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pyn_proto as api;
use reqwest::Method;
use reqwest::blocking::{Client, RequestBuilder, Response};

mod credentials;

/// pyn: version control that merges what can be merged and locks what shouldn't be.
#[derive(Parser)]
#[command(name = "pyn", version)]
struct Cli {
    #[arg(long, env = "PYN_SERVER", default_value = "http://127.0.0.1:7878")]
    server: String,
    /// API token to sign in with; defaults to the sign-in saved by `pyn login`.
    #[arg(long, env = "PYN_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Dev identity sent as X-Pyn-User; only works against a server running with PYN_DEV_AUTH.
    #[arg(long, env = "PYN_USER")]
    user: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sign in with a user name and password and remember it for this server.
    Login {
        username: String,
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Forget the saved sign-in for this server and end that session.
    Logout,
    /// Create an account. Invite-only servers need --invite.
    Register {
        username: String,
        #[arg(long)]
        invite: Option<String>,
        #[arg(long)]
        password_stdin: bool,
    },
    /// Change your own password.
    Password {
        /// Read the current password, then the new one, from standard input.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Show who you are signed in as and what you may do.
    Whoami,
    /// Create, list and revoke invitations.
    #[command(subcommand)]
    Invite(InviteCommand),
    /// Add accounts (needs manage_users).
    #[command(subcommand)]
    User(UserCommand),
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
    /// Make an older revision the new head. Needs the lock and the restore permission, and asks for confirmation.
    Restore {
        path: String,
        /// The older revision whose content becomes the new head.
        revision: u64,
        #[arg(short, long)]
        message: Option<String>,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Remove someone else's lock. Needs the force_unlock permission and a reason, which is recorded.
    Unlock {
        path: String,
        #[arg(long)]
        reason: String,
    },
    /// Show the audit log, newest first (needs the view_audit permission).
    Audit {
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        actor: Option<String>,
        /// checkout, release, checkin, restore or force_unlock.
        #[arg(long)]
        action: Option<String>,
        /// Show events older than this id.
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show a path's revisions.
    History { path: String },
    /// Manage your API tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// List or change who has which role.
    #[command(subcommand)]
    Member(MemberCommand),
    /// List or change what each role grants.
    #[command(subcommand)]
    Role(RoleCommand),
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Create a token. It is shown once.
    Create {
        name: String,
        /// Comma-separated permissions, for example read,lock,checkin.
        #[arg(long, value_delimiter = ',', required = true)]
        permissions: Vec<String>,
        /// Expire the token after this many days.
        #[arg(long)]
        expires_days: Option<i64>,
    },
    /// List your tokens, or another user's with --user (needs manage_users).
    List {
        #[arg(long = "for")]
        for_user: Option<String>,
    },
    /// Revoke a token by id.
    Revoke { id: String },
}

#[derive(Subcommand)]
enum InviteCommand {
    /// Create a one-time invitation code. It is shown once.
    Create {
        #[arg(long, default_value = "reader")]
        role: String,
        /// How long the invitation can be used.
        #[arg(long, default_value_t = 48)]
        hours: i64,
    },
    List,
    Revoke {
        id: String,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    /// Add an account with an initial password and a role.
    Add {
        username: String,
        #[arg(long, default_value = "reader")]
        role: String,
        #[arg(long)]
        password_stdin: bool,
    },
}

#[derive(Subcommand)]
enum MemberCommand {
    List,
    /// Give a user a role, adding them if they are new.
    Set {
        user: String,
        role: String,
    },
}

#[derive(Subcommand)]
enum RoleCommand {
    List,
    /// Replace what a role grants.
    Set {
        role: String,
        #[arg(long, value_delimiter = ',', required = true)]
        permissions: Vec<String>,
    },
}

struct Api {
    http: Client,
    base: String,
    token: Option<String>,
    user: Option<String>,
}

impl Api {
    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let req = self.http.request(method, format!("{}{path}", self.base));
        match (&self.token, &self.user) {
            (Some(token), _) => req.bearer_auth(token),
            (None, Some(user)) => req.header(api::DEV_USER_HEADER, user),
            _ => req,
        }
    }

    /// A request that carries no credentials, for signing in and registering.
    fn anonymous(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{}{path}", self.base))
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.request(Method::GET, path)
    }

    fn send(&self, req: RequestBuilder) -> Result<Response> {
        ok(req.send()?)
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let base = cli.server.trim_end_matches('/').to_string();
    let token = cli
        .token
        .or_else(|| credentials::load(&base).map(|saved| saved.token));
    let api = Api {
        http: Client::new(),
        base,
        token,
        user: cli.user,
    };

    match cli.command {
        Command::Login {
            username,
            password_stdin,
        } => {
            let password = credentials::read_password("Password: ", password_stdin)?;
            let body = api::LoginRequest {
                username: username.clone(),
                password,
            };
            let made: api::CreatedToken = api
                .send(api.anonymous(Method::POST, "/v1/login").json(&body))?
                .json()?;
            credentials::save(
                &api.base,
                credentials::Entry {
                    user: username.clone(),
                    token: made.token,
                },
            )?;
            let until = made
                .info
                .expires_at
                .map_or("never".to_string(), |e| e.to_string());
            println!(
                "signed in as {username} on {}; the sign-in expires {until}",
                api.base
            );
        }
        Command::Logout => {
            let Some(saved) = credentials::load(&api.base) else {
                println!("not signed in on {}", api.base);
                return Ok(());
            };
            if let Some(id) = credentials::token_id(&saved.token) {
                let _ = api
                    .request(Method::DELETE, &format!("/v1/tokens/{id}"))
                    .bearer_auth(&saved.token)
                    .send();
            }
            credentials::remove(&api.base)?;
            println!("signed out {} from {}", saved.user, api.base);
        }
        Command::Register {
            username,
            invite,
            password_stdin,
        } => {
            let password = credentials::read_new_password(password_stdin)?;
            let body = api::RegisterRequest {
                username: username.clone(),
                password,
                invite,
            };
            api.send(api.anonymous(Method::POST, "/v1/register").json(&body))?;
            println!("created {username}; sign in with `pyn login {username}`");
        }
        Command::Password { password_stdin } => {
            let current = credentials::read_password("Current password: ", password_stdin)?;
            let new = credentials::read_new_password(password_stdin)?;
            let body = api::ChangePasswordRequest {
                current: Some(current),
                new,
            };
            api.send(api.request(Method::PUT, "/v1/me/password").json(&body))?;
            println!("password changed");
        }
        Command::Invite(InviteCommand::Create { role, hours }) => {
            let body = api::CreateInviteRequest { role, hours };
            let made: api::CreatedInvite = api
                .send(api.request(Method::POST, "/v1/invites").json(&body))?
                .json()?;
            println!("{}", made.code);
            eprintln!(
                "invitation {} for role {}, expires {}; this is the only time the code is shown",
                made.info.id, made.info.role, made.info.expires_at
            );
        }
        Command::Invite(InviteCommand::List) => {
            let invites: Vec<api::InviteInfo> = api.send(api.get("/v1/invites"))?.json()?;
            for i in invites {
                let state = if i.revoked_at.is_some() {
                    "revoked".to_string()
                } else if let Some(user) = &i.used_by {
                    format!("used by {user}")
                } else {
                    "unused".to_string()
                };
                println!("{}\t{}\t{state}\texpires {}", i.id, i.role, i.expires_at);
            }
        }
        Command::Invite(InviteCommand::Revoke { id }) => {
            api.send(api.request(Method::DELETE, &format!("/v1/invites/{id}")))?;
            println!("revoked {id}");
        }
        Command::User(UserCommand::Add {
            username,
            role,
            password_stdin,
        }) => {
            let password = credentials::read_new_password(password_stdin)?;
            let body = api::AddUserRequest {
                username: username.clone(),
                password,
                role: role.clone(),
            };
            api.send(api.request(Method::POST, "/v1/users").json(&body))?;
            println!("added {username} as {role}");
        }
        Command::Whoami => {
            let me: api::Me = api.send(api.get("/v1/me"))?.json()?;
            println!("{}\t{}", me.user, me.permissions.join(","));
        }
        Command::Locks => {
            let locks: Vec<api::Lock> = api.send(api.get("/v1/locks"))?.json()?;
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
                let mut req = api.get("/v1/files");
                if let Some(a) = &after {
                    req = req.query(&[("after", a)]);
                }
                let page: api::FilePage = api.send(req)?.json()?;
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
            let body = api::CheckoutRequest {
                path,
                base_revision: base,
            };
            let l: api::Lock = api
                .send(api.request(Method::POST, "/v1/checkout").json(&body))?
                .json()?;
            println!("locked {} until {}", l.path, l.expires_at);
        }
        Command::Release { path } => {
            let body = api::ReleaseRequest { path };
            api.send(api.request(Method::POST, "/v1/release").json(&body))?;
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
            let put: api::PutObjectResponse = api
                .send(api.request(Method::PUT, "/v1/objects").body(bytes))?
                .json()?;
            let body = api::CheckinRequest {
                path,
                content: put.content,
                base_revision: base,
                message,
            };
            let r: api::Revision = api
                .send(api.request(Method::POST, "/v1/checkin").json(&body))?
                .json()?;
            println!("{} is now at revision {}", r.path, r.id);
        }
        Command::Get { path, rev, output } => {
            let mut req = api.get("/v1/content").query(&[("path", path.as_str())]);
            if let Some(r) = rev {
                req = req.query(&[("revision", r)]);
            }
            let resp = api.send(req)?;
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
            let revs: Vec<api::Revision> = api
                .send(api.get("/v1/history").query(&[("path", path)]))?
                .json()?;
            for r in revs {
                let restored = r
                    .restored_from
                    .map_or(String::new(), |n| format!(" (restored from r{n})"));
                println!(
                    "{}\t{}\t{}\t{}{restored}",
                    r.id, r.author, r.created_at, r.message
                );
            }
        }
        Command::Restore {
            path,
            revision,
            message,
            yes,
        } => restore(&api, path, revision, message, yes)?,
        Command::Unlock { path, reason } => {
            let body = api::ForceUnlockRequest {
                path: path.clone(),
                reason,
            };
            let lock: api::Lock = api
                .send(api.request(Method::POST, "/v1/force-unlock").json(&body))?
                .json()?;
            println!("removed {}'s lock on {path}", lock.owner);
        }
        Command::Audit {
            path,
            actor,
            action,
            before,
            limit,
        } => {
            let mut req = api.get("/v1/audit").query(&[("limit", limit)]);
            for (key, value) in [("path", path), ("actor", actor), ("action", action)] {
                if let Some(v) = value {
                    req = req.query(&[(key, v)]);
                }
            }
            if let Some(b) = before {
                req = req.query(&[("before", b)]);
            }
            let page: api::AuditPage = api.send(req)?.json()?;
            for e in &page.entries {
                let path = e.path.as_deref().unwrap_or("-");
                println!(
                    "{}\t{}\t{}\t{}\t{path}\t{}",
                    e.id, e.at, e.actor, e.action, e.detail
                );
            }
            if let Some(next) = page.next_before {
                eprintln!("more: --before {next}");
            }
        }
        Command::Token(cmd) => token_command(&api, cmd)?,
        Command::Member(MemberCommand::List) => {
            let members: Vec<api::Member> = api.send(api.get("/v1/members"))?.json()?;
            for m in members {
                println!("{}\t{}", m.user, m.role);
            }
        }
        Command::Member(MemberCommand::Set { user, role }) => {
            let body = api::SetMemberRequest { role: role.clone() };
            api.send(
                api.request(Method::PUT, &format!("/v1/members/{user}"))
                    .json(&body),
            )?;
            println!("{user} is now {role}");
        }
        Command::Role(RoleCommand::List) => {
            let grants: Vec<api::RoleGrant> = api.send(api.get("/v1/roles"))?.json()?;
            for g in grants {
                println!("{}\t{}", g.role, g.permissions.join(","));
            }
        }
        Command::Role(RoleCommand::Set { role, permissions }) => {
            let body = api::SetRoleRequest { permissions };
            api.send(
                api.request(Method::PUT, &format!("/v1/roles/{role}"))
                    .json(&body),
            )?;
            println!("updated {role}");
        }
    }
    Ok(())
}

fn restore(
    api: &Api,
    path: String,
    revision: u64,
    message: Option<String>,
    yes: bool,
) -> Result<()> {
    let revs: Vec<api::Revision> = api
        .send(api.get("/v1/history").query(&[("path", path.as_str())]))?
        .json()?;
    let head = revs
        .last()
        .with_context(|| format!("{path} has no revisions"))?;
    let source = revs
        .iter()
        .find(|r| r.id == revision)
        .with_context(|| format!("{path} has no revision {revision}"))?;
    println!(
        "Restore {path} to r{} ({}, \"{}\").",
        source.id, source.author, source.message
    );
    println!(
        "The current head r{} ({}, \"{}\") stays in history; the restore becomes r{}.",
        head.id,
        head.author,
        head.message,
        head.id + 1
    );
    if !yes {
        eprint!("Type the path to confirm: ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != path {
            bail!("not confirmed");
        }
    }
    let body = api::RestoreRequest {
        path: path.clone(),
        revision,
        base_revision: head.id,
        confirm: format!("{path}@r{}", head.id),
        message,
    };
    let r: api::Revision = api
        .send(api.request(Method::POST, "/v1/restore").json(&body))?
        .json()?;
    println!(
        "{} is now at revision {}, restored from r{revision}",
        r.path, r.id
    );
    Ok(())
}

fn token_command(api: &Api, cmd: TokenCommand) -> Result<()> {
    match cmd {
        TokenCommand::Create {
            name,
            permissions,
            expires_days,
        } => {
            let body = api::CreateTokenRequest {
                name,
                permissions,
                expires_at: expires_days.map(|d| chrono::Utc::now() + chrono::Duration::days(d)),
            };
            let made: api::CreatedToken = api
                .send(api.request(Method::POST, "/v1/tokens").json(&body))?
                .json()?;
            println!("{}", made.token);
            eprintln!(
                "token {} created; this is the only time it is shown",
                made.info.id
            );
        }
        TokenCommand::List { for_user } => {
            let mut req = api.get("/v1/tokens");
            if let Some(user) = &for_user {
                req = req.query(&[("user", user)]);
            }
            let tokens: Vec<api::TokenInfo> = api.send(req)?.json()?;
            for t in tokens {
                let state = if t.revoked_at.is_some() {
                    "revoked"
                } else {
                    "active"
                };
                let expires = t.expires_at.map_or("never".to_string(), |e| e.to_string());
                println!(
                    "{}\t{}\t{state}\t{}\texpires {expires}",
                    t.id,
                    t.name,
                    t.permissions.join(",")
                );
            }
        }
        TokenCommand::Revoke { id } => {
            api.send(api.request(Method::DELETE, &format!("/v1/tokens/{id}")))?;
            println!("revoked {id}");
        }
    }
    Ok(())
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

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pyn_proto as api;
use reqwest::Method;
use reqwest::blocking::Client;

mod address;
mod client;
mod credentials;
mod sync;
mod table;
mod time;
mod userconfig;
mod workspace;

use client::Api;
use workspace::{Settings, Workspace, resolve};

use userconfig::DEFAULT_SERVER;

/// pyn: version control that merges what can be merged and locks what shouldn't be.
#[derive(Parser)]
#[command(name = "pyn", version)]
struct Cli {
    /// The server to talk to; falls back to the workspace's setting, then your user setting, then localhost.
    #[arg(long, env = "PYN_SERVER")]
    server: Option<String>,
    /// API token to sign in with; defaults to the sign-in saved by `pyn login`.
    #[arg(long, env = "PYN_TOKEN", hide_env_values = true)]
    token: Option<String>,
    /// Dev identity sent as X-Pyn-User; only works against a server running with PYN_DEV_AUTH.
    #[arg(long, env = "PYN_USER")]
    user: Option<String>,
    /// The repository as owner/name; falls back to the workspace's setting, then your user setting.
    #[arg(long, env = "PYN_REPO", global = true)]
    repo: Option<String>,
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
    /// Download a repository into a new workspace folder.
    Clone {
        /// `<server>/owner/name`, or `owner/name` on the configured server.
        source: String,
        /// Where to put it; defaults to a folder named after the repository.
        dir: Option<std::path::PathBuf>,
    },
    /// Create, list and delete repositories.
    #[command(subcommand)]
    Repo(RepoCommand),
    /// Show what differs between this workspace and the server.
    Status,
    /// Fetch new and newer files; files with local changes are left alone.
    Update {
        /// Only these paths; everything if omitted.
        paths: Vec<String>,
    },
    /// Read and write settings, per workspace or for your user.
    #[command(subcommand)]
    Config(ConfigCommand),
    /// Show who you are signed in as and, in a repository, what you may do there.
    Whoami,
    /// Link SSH public keys to your account.
    #[command(subcommand)]
    Key(KeyCommand),
    /// Create, list and revoke invitations.
    #[command(subcommand)]
    Invite(InviteCommand),
    /// Add accounts to the current repository (needs manage_users).
    #[command(subcommand)]
    User(UserCommand),
    /// List live locks in the repository, or with --mine yours in every repository.
    Locks {
        #[arg(long)]
        mine: bool,
    },
    /// List files with their mode, head revision and lock.
    Files,
    /// List a folder of the repository (the root by default) with each entry's mode, last change and lock.
    Ls { path: Option<String> },
    /// Show the repository summary: counts, locks and recent activity.
    Summary,
    /// Take the lock on an exclusive file.
    Checkout {
        path: String,
        /// Revision your copy is at (omit if you have none).
        #[arg(long)]
        base: Option<u64>,
    },
    /// Give up a lock without checking in.
    Release { path: String },
    /// Upload a file as the next revision of `path`. Releases the lock.
    Checkin {
        path: String,
        /// Local file to upload; in a workspace, the file at `path` by default.
        file: Option<std::path::PathBuf>,
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
        /// An action such as checkout, restore, force_unlock, member_added, role_changed, token_created or repo_updated.
        #[arg(long)]
        action: Option<String>,
        /// Show events older than this id.
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show a path's revisions (oldest first), or without a path the repository's, newest first.
    History {
        path: Option<String>,
        /// Only paths matching this glob; without a slash it matches at any depth (`*.ts`, `Source/**`).
        #[arg(long, conflicts_with = "path")]
        filter: Option<String>,
        /// How many revisions to show for the repository view.
        #[arg(long, default_value_t = 50, conflicts_with = "path")]
        limit: usize,
    },
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
enum RepoCommand {
    /// Create a repository in your namespace; you become its admin.
    Create {
        /// `name`, or `owner/name` where owner is you.
        name: String,
        /// `public` or `private`; private by default.
        #[arg(long)]
        visibility: Option<String>,
        /// How long a checkout lasts before it expires unless renewed.
        #[arg(long)]
        lease_hours: Option<u32>,
        /// Locks one user may hold at once; the server's default if omitted, and a limit in the policy file wins.
        #[arg(long)]
        max_locks: Option<u32>,
        /// Use this file as the repository's first `.pyn/pyn.toml` instead of the default (everything exclusive).
        #[arg(long, conflicts_with = "no_policy")]
        policy: Option<PathBuf>,
        /// Create the repository without a `.pyn/pyn.toml`; the server's fallback policy applies until one is added.
        #[arg(long)]
        no_policy: bool,
    },
    /// List the repositories you belong to.
    List {
        /// Only this owner's.
        #[arg(long)]
        owner: Option<String>,
    },
    /// Delete a repository with its files, history, locks and access. Its audit log is kept.
    Delete {
        /// `owner/name`.
        name: String,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Ask for the user settings and write them to the user configuration file.
    Init,
    /// Show the settings that are set (the effective values unless --global or --local is given).
    List {
        #[arg(long, conflicts_with = "local")]
        global: bool,
        #[arg(long)]
        local: bool,
    },
    /// Show one setting.
    Get {
        key: String,
        #[arg(long, conflicts_with = "local")]
        global: bool,
        #[arg(long)]
        local: bool,
    },
    /// Set one setting for this workspace, or for your user with --global.
    Set {
        key: String,
        value: String,
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Create a token. It is shown once.
    Create {
        name: String,
        /// Comma-separated permissions, for example read,lock,checkin.
        #[arg(long, value_delimiter = ',', required = true)]
        permissions: Vec<String>,
        /// Comma-separated repositories (owner/name) the token is valid for; the current repository by default.
        #[arg(long, value_delimiter = ',')]
        repos: Vec<String>,
        /// Expire the token after this many days.
        #[arg(long)]
        expires_days: Option<i64>,
    },
    /// List your tokens, or another user's with --for (needs manage_users in a repository they belong to).
    List {
        #[arg(long = "for")]
        for_user: Option<String>,
    },
    /// Revoke a token by id.
    Revoke { id: String },
}

#[derive(Subcommand)]
enum KeyCommand {
    /// Add a public key. Without a file, uses ~/.ssh/id_ed25519.pub, id_ecdsa.pub or id_rsa.pub, whichever exists first.
    Add {
        /// A public key file (a `.pub` file).
        file: Option<std::path::PathBuf>,
        /// A name for the key; defaults to the comment in the key.
        #[arg(long)]
        title: Option<String>,
    },
    /// List your keys, or another user's with --for (needs manage_users in a repository they belong to).
    List {
        #[arg(long = "for")]
        for_user: Option<String>,
    },
    /// Remove a key by id.
    Remove {
        id: String,
        #[arg(long = "for")]
        for_user: Option<String>,
    },
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    let ws = Workspace::discover(&cwd);
    let user_config = userconfig::path()?;
    let (mine, mut yours) = (
        ws.as_ref().map(Workspace::settings).unwrap_or_default(),
        Settings::load(&user_config).unwrap_or_default(),
    );
    let source = match &cli.command {
        Command::Clone { source, .. } => Some(address::parse_source(source)?),
        _ => None,
    };
    let needs_server = !matches!(cli.command, Command::Config(_))
        && source.as_ref().is_none_or(|s| s.server.is_none());
    if needs_server
        && cli.server.is_none()
        && mine.server.is_none()
        && yours.server.is_none()
        && !user_config.exists()
    {
        yours = userconfig::first_run(
            &user_config,
            interactive(),
            &mut std::io::stdin().lock(),
            &mut std::io::stderr(),
        )?;
    }
    let server = source
        .as_ref()
        .and_then(|s| s.server.clone())
        .or_else(|| {
            resolve(
                cli.server,
                mine.server.as_ref(),
                yours.server.as_ref(),
                Some(DEFAULT_SERVER),
            )
        })
        .expect("there is a default");
    let repo = match &source {
        Some(s) => Some(s.repo.clone()),
        None => resolve(cli.repo, mine.repo.as_ref(), yours.repo.as_ref(), None),
    };
    let base = server.trim_end_matches('/').to_string();
    let token = cli
        .token
        .or_else(|| credentials::load(&base).map(|saved| saved.token));
    let api = Api {
        http: Client::new(),
        base,
        token,
        user: resolve(cli.user, mine.user.as_ref(), yours.user.as_ref(), None),
        repo,
    };
    let rp = |p: &str| -> Result<String> {
        match &ws {
            Some(w) => w.repo_path(&cwd, p),
            None => Ok(p.to_string()),
        }
    };
    let in_workspace = |what: &str| -> Result<&Workspace> {
        ws.as_ref().with_context(|| {
            format!("`pyn {what}` works inside a workspace; run `pyn clone <dir>` first")
        })
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
                .map_or("never".to_string(), time::local);
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
        Command::Key(KeyCommand::Add { file, title }) => {
            let file = file.map_or_else(default_public_key, Ok)?;
            let key = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            let body = api::AddKeyRequest { title, key };
            let added: api::SshKeyInfo = api
                .send(api.request(Method::POST, "/v1/keys").json(&body))?
                .json()?;
            println!(
                "added {} ({}) as {}",
                added.title, added.fingerprint, added.id
            );
        }
        Command::Key(KeyCommand::List { for_user }) => {
            let mut req = api.get("/v1/keys");
            if let Some(user) = &for_user {
                req = req.query(&[("user", user)]);
            }
            let keys: Vec<api::SshKeyInfo> = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = keys
                .into_iter()
                .map(|k| {
                    let used = k.last_used_at.map_or("never".to_string(), time::local);
                    vec![k.id.to_string(), k.algorithm, used, k.fingerprint, k.title]
                })
                .collect();
            table::show(
                &["ID", "ALGORITHM", "LAST USED", "FINGERPRINT", "TITLE"],
                &rows,
                "no keys",
            );
        }
        Command::Key(KeyCommand::Remove { id, for_user }) => {
            let mut req = api.request(Method::DELETE, &format!("/v1/keys/{id}"));
            if let Some(user) = &for_user {
                req = req.query(&[("user", user)]);
            }
            api.send(req)?;
            println!("removed {id}");
        }
        Command::Invite(InviteCommand::Create { role, hours }) => {
            let body = api::CreateInviteRequest { role, hours };
            let made: api::CreatedInvite = api
                .send(
                    api.request(Method::POST, &api.repo_route("/invites")?)
                        .json(&body),
                )?
                .json()?;
            println!("{}", made.code);
            eprintln!(
                "invitation {} for role {}, expires {}; this is the only time the code is shown",
                made.info.id,
                made.info.role,
                time::local(made.info.expires_at)
            );
        }
        Command::Invite(InviteCommand::List) => {
            let invites: Vec<api::InviteInfo> =
                api.send(api.get(&api.repo_route("/invites")?))?.json()?;
            let rows: Vec<Vec<String>> = invites
                .into_iter()
                .map(|i| {
                    let state = if i.revoked_at.is_some() {
                        "revoked".to_string()
                    } else if let Some(user) = &i.used_by {
                        format!("used by {user}")
                    } else {
                        "unused".to_string()
                    };
                    vec![i.id.to_string(), i.role, time::local(i.expires_at), state]
                })
                .collect();
            table::show(&["ID", "ROLE", "EXPIRES", "STATE"], &rows, "no invitations");
        }
        Command::Invite(InviteCommand::Revoke { id }) => {
            api.send(api.request(Method::DELETE, &api.repo_route(&format!("/invites/{id}"))?))?;
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
            api.send(
                api.request(Method::POST, &api.repo_route("/users")?)
                    .json(&body),
            )?;
            println!("added {username} as {role}");
        }
        Command::Whoami => {
            if api.repo.is_some() {
                let me: api::Me = api.send(api.get(&api.repo_route("/me")?))?.json()?;
                println!("{}\t{}", me.user, me.permissions.join(","));
            } else {
                let me: api::Account = api.send(api.get("/v1/me"))?.json()?;
                println!("{}", me.user);
            }
        }
        Command::Repo(cmd) => repo_command(&api, cmd)?,
        Command::Locks { mine: true } => {
            let locks: Vec<api::MyLock> = api.send(api.get("/v1/me/locks"))?.json()?;
            let rows: Vec<Vec<String>> = locks
                .into_iter()
                .map(|l| {
                    vec![
                        format!("{}/{}", l.owner, l.name),
                        time::local(l.acquired_at),
                        time::local(l.expires_at),
                        l.path,
                    ]
                })
                .collect();
            table::show(
                &["REPOSITORY", "ACQUIRED", "EXPIRES", "PATH"],
                &rows,
                "no locks",
            );
        }
        Command::Locks { mine: false } => {
            let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
            let rows: Vec<Vec<String>> = locks
                .into_iter()
                .map(|l| vec![l.owner, time::local(l.expires_at), l.path])
                .collect();
            table::show(&["OWNER", "EXPIRES", "PATH"], &rows, "no locks");
        }
        Command::Files => {
            let mut after: Option<String> = None;
            let mut rows: Vec<Vec<String>> = Vec::new();
            loop {
                let mut req = api.get(&api.repo_route("/files")?);
                if let Some(a) = &after {
                    req = req.query(&[("after", a)]);
                }
                let page: api::FilePage = api.send(req)?.json()?;
                for f in page.entries {
                    rows.push(vec![
                        sync::mode_name(f.mode).to_string(),
                        f.revision.map_or("-".to_string(), |r| format!("r{r}")),
                        f.lock.map_or("-".to_string(), |l| l.owner),
                        f.path,
                    ]);
                }
                match page.next_after {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }
            table::show(&["MODE", "REV", "LOCKED BY", "PATH"], &rows, "no files");
        }
        Command::Ls { path } => {
            let mut req = api.get(&api.repo_route("/tree")?);
            if let Some(p) = &path {
                req = req.query(&[("path", p)]);
            }
            let listing: api::TreeListing = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = listing
                .entries
                .iter()
                .map(|e| {
                    let slash = if e.kind == api::TreeEntryKind::Folder {
                        "/"
                    } else {
                        ""
                    };
                    vec![
                        format!("{:?}", e.mode).to_lowercase(),
                        e.lock.as_ref().map_or("-".to_string(), |l| l.owner.clone()),
                        format!("{}{slash}", e.name),
                        e.last_change.as_ref().map_or("-".to_string(), |r| {
                            format!("{} ({}, r{})", r.message, r.author, r.id)
                        }),
                    ]
                })
                .collect();
            table::show(
                &["MODE", "LOCKED BY", "NAME", "LAST CHANGE"],
                &rows,
                "empty",
            );
        }
        Command::Summary => {
            let s: api::RepoSummary = api.send(api.get(&api.repo_route("/summary")?))?.json()?;
            println!(
                "{} ({} branch), {} files ({} exclusive, {} shared)",
                s.default_branch, s.branch_count, s.files, s.exclusive_files, s.shared_files
            );
            if !s.locks.is_empty() {
                println!();
                let rows: Vec<Vec<String>> = s
                    .locks
                    .iter()
                    .map(|l| vec![l.owner.clone(), l.path.clone()])
                    .collect();
                println!("{}", table::render(&["LOCKED BY", "PATH"], &rows));
            }
            if !s.activity.is_empty() {
                println!();
                let rows: Vec<Vec<String>> = s
                    .activity
                    .iter()
                    .map(|a| {
                        vec![
                            time::local(a.at),
                            a.actor.clone(),
                            a.action.clone(),
                            a.path.clone().unwrap_or_else(|| "-".into()),
                        ]
                    })
                    .collect();
                println!(
                    "{}",
                    table::render(&["WHEN", "ACTOR", "ACTION", "PATH"], &rows)
                );
            }
        }
        Command::Clone { dir, .. } => {
            let repo = api.repo.clone().expect("clone names a repository");
            let dir = dir.unwrap_or_else(|| {
                address::split_repo(&repo)
                    .map(|(_, n)| n.into())
                    .unwrap_or_default()
            });
            let target = if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            };
            sync::clone_into(&api, &target, &server, &repo)?;
        }
        Command::Status => sync::status(&api, in_workspace("status")?)?,
        Command::Update { paths } => {
            let only = paths.iter().map(|p| rp(p)).collect::<Result<Vec<_>>>()?;
            sync::update(&api, in_workspace("update")?, &only)?;
        }
        Command::Config(cmd) => config_command(cmd, ws.as_ref(), &mine, &yours, &user_config)?,
        Command::Checkout { path, base } => {
            let path = rp(&path)?;
            let known = ws
                .as_ref()
                .and_then(|w| w.load_state().ok())
                .and_then(|s| s.get(&path).map(|k| k.revision));
            let body = api::CheckoutRequest {
                path: path.clone(),
                base_revision: base.or(known),
            };
            let l: api::Lock = api
                .send(
                    api.request(Method::POST, &api.repo_route("/checkout")?)
                        .json(&body),
                )
                .map_err(|e| behind_hint(e, ws.is_some()))?
                .json()?;
            if let Some(w) = &ws {
                sync::after_checkout(w, &path)?;
            }
            println!("locked {} until {}", l.path, time::local(l.expires_at));
        }
        Command::Release { path } => {
            let path = rp(&path)?;
            let body = api::ReleaseRequest { path: path.clone() };
            api.send(
                api.request(Method::POST, &api.repo_route("/release")?)
                    .json(&body),
            )?;
            if let Some(w) = &ws {
                sync::after_release(w, &path)?;
            }
            println!("released");
        }
        Command::Checkin {
            path,
            file,
            base,
            message,
        } => {
            let path = rp(&path)?;
            let file = match (file, &ws) {
                (Some(file), _) => file,
                (None, Some(w)) => w.abs(&path),
                (None, None) => bail!("name the file to upload, or run this inside a workspace"),
            };
            let bytes =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let known = ws
                .as_ref()
                .and_then(|w| w.load_state().ok())
                .and_then(|s| s.get(&path).map(|k| k.revision));
            let put: api::PutObjectResponse = api
                .send(
                    api.request(Method::PUT, &api.repo_route("/objects")?)
                        .body(bytes.clone()),
                )?
                .json()?;
            let body = api::CheckinRequest {
                path: path.clone(),
                content: put.content,
                base_revision: base.or(known),
                message,
            };
            let r: api::Revision = api
                .send(
                    api.request(Method::POST, &api.repo_route("/checkin")?)
                        .json(&body),
                )
                .map_err(|e| behind_hint(e, ws.is_some()))?
                .json()?;
            if let Some(w) = &ws {
                sync::after_checkin(&api, w, &path, r.id, &bytes)?;
            }
            println!("{} is now at revision {}", r.path, r.id);
        }
        Command::Get { path, rev, output } => {
            let path = rp(&path)?;
            let mut req = api
                .get(&api.repo_route("/content")?)
                .query(&[("path", path.as_str())]);
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
        Command::History {
            path,
            filter,
            limit,
        } => {
            let (revs, repo_wide) = match path {
                Some(path) => (path_history(&api, &rp(&path)?)?, false),
                None => (repo_history(&api, filter.as_deref(), limit)?, true),
            };
            let rows: Vec<Vec<String>> = revs
                .into_iter()
                .map(|r| {
                    let restored = r
                        .restored_from
                        .map_or(String::new(), |n| format!(" (restored from r{n})"));
                    let mut row = vec![
                        format!("r{}", r.id),
                        r.author,
                        time::local(r.created_at),
                        format!("{}{restored}", r.message),
                    ];
                    if repo_wide {
                        row.push(r.path);
                    }
                    row
                })
                .collect();
            let header: &[&str] = if repo_wide {
                &["REV", "AUTHOR", "WHEN", "MESSAGE", "PATH"]
            } else {
                &["REV", "AUTHOR", "WHEN", "MESSAGE"]
            };
            table::show(header, &rows, "no history");
        }
        Command::Restore {
            path,
            revision,
            message,
            yes,
        } => restore(&api, rp(&path)?, revision, message, yes)?,
        Command::Unlock { path, reason } => {
            let path = rp(&path)?;
            let body = api::ForceUnlockRequest {
                path: path.clone(),
                reason,
            };
            let lock: api::Lock = api
                .send(
                    api.request(Method::POST, &api.repo_route("/force-unlock")?)
                        .json(&body),
                )?
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
            let path = path.map(|p| rp(&p)).transpose()?;
            let mut req = api
                .get(&api.repo_route("/audit")?)
                .query(&[("limit", limit)]);
            for (key, value) in [("path", path), ("actor", actor), ("action", action)] {
                if let Some(v) = value {
                    req = req.query(&[(key, v)]);
                }
            }
            if let Some(b) = before {
                req = req.query(&[("before", b)]);
            }
            let page: api::AuditPage = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = page
                .entries
                .iter()
                .map(|e| {
                    vec![
                        e.id.to_string(),
                        time::local(e.at),
                        e.actor.clone(),
                        e.action.clone(),
                        e.path.clone().unwrap_or_else(|| "-".into()),
                        time::localize(&e.detail),
                    ]
                })
                .collect();
            table::show(
                &["ID", "WHEN", "ACTOR", "ACTION", "PATH", "DETAIL"],
                &rows,
                "no audit entries",
            );
            if let Some(next) = page.next_before {
                eprintln!("more: --before {next}");
            }
        }
        Command::Token(cmd) => token_command(&api, cmd)?,
        Command::Member(MemberCommand::List) => {
            let members: Vec<api::Member> =
                api.send(api.get(&api.repo_route("/members")?))?.json()?;
            let rows: Vec<Vec<String>> =
                members.into_iter().map(|m| vec![m.role, m.user]).collect();
            table::show(&["ROLE", "USER"], &rows, "no members");
        }
        Command::Member(MemberCommand::Set { user, role }) => {
            let body = api::SetMemberRequest { role: role.clone() };
            api.send(
                api.request(Method::PUT, &api.repo_route(&format!("/members/{user}"))?)
                    .json(&body),
            )?;
            println!("{user} is now {role}");
        }
        Command::Role(RoleCommand::List) => {
            let grants: Vec<api::RoleGrant> =
                api.send(api.get(&api.repo_route("/roles")?))?.json()?;
            let rows: Vec<Vec<String>> = grants
                .into_iter()
                .map(|g| vec![g.role, g.permissions.join(",")])
                .collect();
            table::show(&["ROLE", "PERMISSIONS"], &rows, "no roles");
        }
        Command::Role(RoleCommand::Set { role, permissions }) => {
            let body = api::SetRoleRequest { permissions };
            api.send(
                api.request(Method::PUT, &api.repo_route(&format!("/roles/{role}"))?)
                    .json(&body),
            )?;
            println!("updated {role}");
        }
    }
    Ok(())
}

fn path_history(api: &Api, path: &str) -> Result<Vec<api::Revision>> {
    let page: api::HistoryPage = api
        .send(
            api.get(&api.repo_route("/history")?)
                .query(&[("path", path)]),
        )?
        .json()?;
    Ok(page.revisions)
}

/// Follows the server's cursor until `limit` revisions are collected or history ends.
fn repo_history(api: &Api, filter: Option<&str>, limit: usize) -> Result<Vec<api::Revision>> {
    let mut revs: Vec<api::Revision> = Vec::new();
    let mut before: Option<String> = None;
    while revs.len() < limit {
        let mut query = vec![("limit", (limit - revs.len()).min(200).to_string())];
        query.extend(filter.map(|f| ("filter", f.to_string())));
        query.extend(before.take().map(|b| ("before", b)));
        let page: api::HistoryPage = api
            .send(api.get(&api.repo_route("/history")?).query(&query))?
            .json()?;
        revs.extend(page.revisions);
        before = page.next_cursor;
        if before.is_none() {
            break;
        }
    }
    Ok(revs)
}

fn restore(
    api: &Api,
    path: String,
    revision: u64,
    message: Option<String>,
    yes: bool,
) -> Result<()> {
    let revs = path_history(api, &path)?;
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
        .send(
            api.request(Method::POST, &api.repo_route("/restore")?)
                .json(&body),
        )?
        .json()?;
    println!(
        "{} is now at revision {}, restored from r{revision}",
        r.path, r.id
    );
    Ok(())
}

const POLICY_PATH: &str = ".pyn/pyn.toml";

const DEFAULT_POLICY: &str = "[meta]\ndefault = \"exclusive\"\n";

/// Checks in the repository's first policy file, which the server accepts without a lock.
fn submit_policy(api: &Api, repo: &str, bytes: Vec<u8>) -> Result<()> {
    let put: api::PutObjectResponse = api
        .send(
            api.request(Method::PUT, &format!("{repo}/objects"))
                .body(bytes),
        )?
        .json()?;
    let body = api::CheckinRequest {
        path: POLICY_PATH.into(),
        content: put.content,
        base_revision: None,
        message: "Initial policy".into(),
    };
    api.send(
        api.request(Method::POST, &format!("{repo}/checkin"))
            .json(&body),
    )?;
    Ok(())
}

fn repo_command(api: &Api, cmd: RepoCommand) -> Result<()> {
    match cmd {
        RepoCommand::Create {
            name,
            visibility,
            lease_hours,
            max_locks,
            policy,
            no_policy,
        } => {
            let policy = match (policy, no_policy) {
                (Some(file), _) => Some(
                    std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?,
                ),
                (None, false) => Some(DEFAULT_POLICY.as_bytes().to_vec()),
                (None, true) => None,
            };
            let (owner, name) = match name.split_once('/') {
                Some((owner, name)) => (Some(owner.to_string()), name.to_string()),
                None => (None, name),
            };
            let visibility = visibility
                .map(|v| match v.as_str() {
                    "public" => Ok(api::Visibility::Public),
                    "private" => Ok(api::Visibility::Private),
                    other => bail!("unknown visibility {other:?}: use public or private"),
                })
                .transpose()?;
            let body = api::CreateRepoRequest {
                owner,
                name,
                visibility,
                lease_hours,
                max_locks_per_user: max_locks,
            };
            let made: api::RepoInfo = api
                .send(api.request(Method::POST, "/v1/repos").json(&body))?
                .json()?;
            println!("created {}/{}", made.owner, made.name);
            if let Some(bytes) = policy {
                let repo = format!("/v1/repos/{}/{}", made.owner, made.name);
                submit_policy(api, &repo, bytes)
                    .context("the repository was created, but its policy was not accepted")?;
                println!("added {POLICY_PATH}");
            }
        }
        RepoCommand::List { owner } => {
            let mut req = api.get("/v1/repos");
            if let Some(owner) = &owner {
                req = req.query(&[("owner", owner)]);
            }
            let repos: Vec<api::RepoInfo> = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = repos
                .into_iter()
                .map(|r| {
                    let visibility = match r.visibility {
                        api::Visibility::Public => "public",
                        api::Visibility::Private => "private",
                    };
                    vec![
                        visibility.to_string(),
                        r.role.unwrap_or_else(|| "-".into()),
                        format!("{}/{}", r.owner, r.name),
                    ]
                })
                .collect();
            table::show(
                &["VISIBILITY", "ROLE", "REPOSITORY"],
                &rows,
                "no repositories",
            );
        }
        RepoCommand::Delete { name, yes } => {
            let (owner, short) = address::split_repo(&name)?;
            if !yes {
                eprintln!(
                    "This removes {name} with its files, history, locks and access. Its audit log is kept."
                );
                eprint!("Type the repository name to confirm: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != name {
                    bail!("not confirmed");
                }
            }
            api.send(api.request(Method::DELETE, &format!("/v1/repos/{owner}/{short}")))?;
            println!("deleted {name}");
        }
    }
    Ok(())
}

fn token_command(api: &Api, cmd: TokenCommand) -> Result<()> {
    match cmd {
        TokenCommand::Create {
            name,
            permissions,
            repos,
            expires_days,
        } => {
            let repos =
                if repos.is_empty() {
                    vec![api.repo.clone().context(
                        "name the repositories the token is for with --repos owner/name",
                    )?]
                } else {
                    repos
                };
            let body = api::CreateTokenRequest {
                name,
                permissions,
                repos,
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
            let rows: Vec<Vec<String>> = tokens
                .into_iter()
                .map(|t| {
                    let state = if t.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    };
                    vec![
                        t.id.to_string(),
                        state.to_string(),
                        t.expires_at.map_or("never".to_string(), time::local),
                        t.name,
                        t.permissions.join(","),
                    ]
                })
                .collect();
            table::show(
                &["ID", "STATE", "EXPIRES", "NAME", "PERMISSIONS"],
                &rows,
                "no tokens",
            );
        }
        TokenCommand::Revoke { id } => {
            api.send(api.request(Method::DELETE, &format!("/v1/tokens/{id}")))?;
            println!("revoked {id}");
        }
    }
    Ok(())
}

/// A stale-base refusal in a workspace means the file moved on: say how to catch up.
fn behind_hint(err: anyhow::Error, in_workspace: bool) -> anyhow::Error {
    if in_workspace && err.to_string().starts_with("stale_base") {
        err.context("the server has a newer revision; run `pyn update` first")
    } else {
        err
    }
}

fn interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn config_command(
    cmd: ConfigCommand,
    ws: Option<&Workspace>,
    mine: &Settings,
    yours: &Settings,
    user_config: &std::path::Path,
) -> Result<()> {
    let show = |name: &str, s: &Settings, rows: &mut Vec<Vec<String>>| -> Result<()> {
        for key in workspace::SETTING_KEYS {
            if let Some(v) = s.get(key)? {
                rows.push(vec![key.to_string(), name.to_string(), v.to_string()]);
            }
        }
        Ok(())
    };
    match cmd {
        ConfigCommand::Init => {
            if !interactive() {
                bail!(
                    "`pyn config init` asks questions and needs a terminal; use `pyn config set --global <key> <value>` instead"
                );
            }
            let settings =
                userconfig::prompt(yours, &mut std::io::stdin().lock(), &mut std::io::stderr())?;
            settings.save(user_config)?;
            println!("wrote {}", user_config.display());
        }
        ConfigCommand::List { global, local } => {
            let mut rows = Vec::new();
            if global {
                show("user", yours, &mut rows)?;
            } else if local {
                show("workspace", mine, &mut rows)?;
            } else {
                show("workspace", mine, &mut rows)?;
                show("user", yours, &mut rows)?;
            }
            table::show(&["KEY", "SCOPE", "VALUE"], &rows, "no settings");
        }
        ConfigCommand::Get { key, global, local } => {
            let value = match (global, local) {
                (true, _) => yours.get(&key)?.cloned(),
                (_, true) => mine.get(&key)?.cloned(),
                _ => resolve(
                    None,
                    mine.get(&key)?,
                    yours.get(&key)?,
                    (key == "server").then_some(DEFAULT_SERVER),
                ),
            };
            match value {
                Some(v) => println!("{v}"),
                None => bail!("{key} is not set"),
            }
        }
        ConfigCommand::Set { key, value, global } => {
            if global {
                let mut settings = yours.clone();
                settings.set(&key, value)?;
                settings.save(user_config)?;
            } else {
                let w =
                    ws.context("not in a workspace; run `pyn clone <dir>` first, or use --global")?;
                let mut settings = mine.clone();
                settings.set(&key, value)?;
                settings.save(&w.config_path())?;
            }
        }
    }
    Ok(())
}

fn default_public_key() -> Result<std::path::PathBuf> {
    let home =
        std::env::var("HOME").context("cannot find your home directory; name the key file")?;
    ["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"]
        .iter()
        .map(|name| std::path::Path::new(&home).join(".ssh").join(name))
        .find(|p| p.is_file())
        .context("no public key found in ~/.ssh; name the key file, or create one with `ssh-keygen -t ed25519`")
}

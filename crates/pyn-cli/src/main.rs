use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use pyn_proto as api;
use reqwest::Method;
use reqwest::blocking::Client;

mod address;
mod admin;
mod client;
mod credentials;
mod limits;
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
    /// Create an account. Invite-only servers need --invite; open servers that verify addresses need --email.
    Register {
        username: String,
        #[arg(long)]
        invite: Option<String>,
        #[arg(long)]
        email: Option<String>,
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
    /// Create, inspect and delete organizations, and manage who belongs to them.
    #[command(subcommand)]
    Org(OrgCommand),
    /// Create and manage an organization's teams, and grant them roles on its repositories.
    #[command(subcommand)]
    Team(TeamCommand),
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
    Lock {
        path: String,
        /// Revision your copy is at (omit if you have none).
        #[arg(long)]
        base: Option<u64>,
    },
    /// Give up your lock without checking in; with --force, remove someone else's (needs force_unlock and a reason).
    Unlock {
        path: String,
        /// Remove another user's lock instead of giving up your own.
        #[arg(long, requires = "reason")]
        force: bool,
        /// Why the lock is removed; recorded in the audit log.
        #[arg(long, requires = "force")]
        reason: Option<String>,
    },
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
    /// Show the audit log, newest first (needs the view_audit permission).
    Audit {
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        actor: Option<String>,
        /// An action such as checkout (taking a lock), release, restore, force_unlock, member_added, role_changed, token_created or repo_updated.
        #[arg(long)]
        action: Option<String>,
        /// Show events older than this id.
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show a path's revisions (oldest first), or without a path the repository's, newest first.
    #[command(visible_alias = "history")]
    Log {
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
    /// Run first-run setup on a new server: creates the first administrator and records the essentials.
    Setup {
        /// The first administrator's user name.
        username: String,
        /// The one-time setup token from the server's log or its PYN_SETUP_TOKEN setting.
        #[arg(long, env = "PYN_SETUP_TOKEN", hide_env_values = true)]
        setup_token: Option<String>,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        server_name: Option<String>,
        /// The address people reach the web app at; verification links use it.
        #[arg(long)]
        public_url: Option<String>,
        /// `open`, `invite` or `closed`; the server's configured default if omitted.
        #[arg(long)]
        registration: Option<String>,
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Server administration (server administrators and service credentials only).
    #[command(subcommand)]
    Admin(admin::AdminCommand),
    /// Show what you or an organization you own use on the server, and any limits that apply.
    Usage {
        /// A user or organization; you by default.
        owner: Option<String>,
    },
    /// List or change who has which role.
    #[command(subcommand)]
    Member(MemberCommand),
    /// List or change what each role grants.
    #[command(subcommand)]
    Role(RoleCommand),
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Create a repository in your namespace or an organization you own; you become its admin.
    Create {
        /// `name`, or `owner/name` where owner is you or an organization you own.
        name: String,
        /// `public` or `private`; private by default.
        #[arg(long)]
        visibility: Option<String>,
        /// How long a lock lasts before it expires unless renewed.
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
    /// List public repositories on the server, including ones you have no role in; works signed out.
    Explore {
        /// At most this many repositories.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Make a repository public (anyone may read it) or private (members only). Owner and admin only.
    Visibility {
        /// `owner/name`.
        name: String,
        /// `public` or `private`.
        visibility: String,
    },
    /// Show the stored bytes, files and revisions of a repository.
    Usage {
        /// `owner/name`.
        name: String,
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
enum OrgCommand {
    /// Create an organization; you become its first owner.
    Create { name: String },
    /// List the organizations you belong to.
    List,
    /// Show an organization, and its members if you belong to it.
    Show { name: String },
    /// Delete an organization that owns no repositories. Its audit log is kept.
    Delete {
        name: String,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Show an organization's audit log, newest first (owners only).
    Audit {
        name: String,
        /// Only entries older than this id.
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// List and change an organization's members.
    #[command(subcommand)]
    Member(OrgMemberCommand),
    /// Show and change who may create repositories in an organization (owners only).
    #[command(subcommand)]
    Policy(OrgPolicyCommand),
}

#[derive(Subcommand)]
enum OrgPolicyCommand {
    /// Show the base setting for members and the rules.
    Show { org: String },
    /// Set what members may create when no rule applies.
    Set {
        org: String,
        /// `none`, `private` or `both`.
        #[arg(long)]
        members: String,
    },
    /// Let a team, user or role create repositories.
    Allow {
        org: String,
        /// `team`, `user` or `role`.
        kind: String,
        /// A team slug, a user name, or `owner` or `member` for a role.
        subject: String,
        /// `public`, `private` or `both`.
        #[arg(long, default_value = "both")]
        scope: String,
    },
    /// Stop a team, user or role from creating repositories; beats any allow.
    Deny {
        org: String,
        kind: String,
        subject: String,
        #[arg(long, default_value = "both")]
        scope: String,
    },
    /// Remove an allow or deny rule.
    Remove {
        org: String,
        /// `allow` or `deny`.
        effect: String,
        kind: String,
        subject: String,
    },
}

#[derive(Subcommand)]
enum OrgMemberCommand {
    /// List an organization's members.
    List { org: String },
    /// Add an existing account to an organization (owners only).
    Add {
        org: String,
        user: String,
        /// `owner` or `member`; member by default.
        #[arg(long)]
        role: Option<String>,
    },
    /// Change a member's role to `owner` or `member` (owners only).
    Set {
        org: String,
        user: String,
        role: String,
    },
    /// Remove a member, or leave by naming yourself. Drops their direct roles on the organization's repositories.
    Remove { org: String, user: String },
}

#[derive(Subcommand)]
enum TeamCommand {
    /// Create a team (organization owners only).
    Create {
        /// `<org>/<team>`; the team is 1 to 39 lowercase letters, digits, `-` or `_`.
        team: String,
        /// Display name; the team name by default.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// List an organization's teams.
    List { org: String },
    /// Show a team with its members and the repositories it holds roles in.
    Show { team: String },
    /// Delete a team, its memberships and its roles on repositories (owners only).
    Delete {
        team: String,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// Add or remove team members.
    #[command(subcommand)]
    Member(TeamMemberCommand),
    /// Give a team a role on one of the organization's repositories.
    Grant {
        team: String,
        /// `owner/name` of a repository the organization owns.
        repo: String,
        #[arg(long)]
        role: String,
    },
    /// Take a team's role on a repository away.
    Revoke { team: String, repo: String },
    /// List the teams that hold a role in a repository.
    Access {
        /// `owner/name`; the current repository if omitted.
        repo: Option<String>,
    },
}

#[derive(Subcommand)]
enum TeamMemberCommand {
    /// Add an organization member to a team (owners only).
    Add { team: String, user: String },
    /// Remove a user from a team (owners only).
    Remove { team: String, user: String },
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
            email,
            password_stdin,
        } => {
            let password = credentials::read_new_password(password_stdin)?;
            let body = api::RegisterRequest {
                username: username.clone(),
                password,
                invite,
                email,
            };
            let done: api::Registered = api
                .send(api.anonymous(Method::POST, "/v1/register").json(&body))?
                .json()?;
            match done.status.as_str() {
                "pending_verification" => println!(
                    "created {username}; follow the link in the email sent to you, then sign in"
                ),
                "pending_approval" => {
                    println!(
                        "created {username}; an administrator must approve it before you can sign in"
                    )
                }
                _ => println!("created {username}; sign in with `pyn login {username}`"),
            }
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
        Command::Org(cmd) => org_command(&api, cmd)?,
        Command::Setup {
            username,
            setup_token,
            email,
            server_name,
            public_url,
            registration,
            password_stdin,
        } => admin::setup(
            &api,
            admin::SetupArgs {
                username,
                setup_token,
                email,
                server_name,
                public_url,
                registration,
                password_stdin,
            },
            interactive(),
        )?,
        Command::Admin(cmd) => admin::run(&api, cmd)?,
        Command::Usage { owner } => limits::usage(&api, owner)?,
        Command::Team(cmd) => team_command(&api, cmd)?,
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
        Command::Lock { path, base } => {
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
                sync::after_lock(w, &path)?;
            }
            println!("locked {} until {}", l.path, time::local(l.expires_at));
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
        Command::Log {
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
        Command::Unlock {
            path, force: false, ..
        } => {
            let path = rp(&path)?;
            let body = api::ReleaseRequest { path: path.clone() };
            api.send(
                api.request(Method::POST, &api.repo_route("/release")?)
                    .json(&body),
            )?;
            if let Some(w) = &ws {
                sync::after_unlock(w, &path)?;
            }
            println!("unlocked {path}");
        }
        Command::Unlock {
            path,
            reason: Some(reason),
            ..
        } => {
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
        Command::Unlock { .. } => unreachable!("clap requires a reason with --force"),
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
            let rows: Vec<Vec<String>> = members
                .into_iter()
                .map(|m| vec![m.role, m.source, m.user])
                .collect();
            table::show(&["ROLE", "SOURCE", "USER"], &rows, "no members");
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

fn explain_create_refusal(err: anyhow::Error, org: &str) -> anyhow::Error {
    match err.downcast_ref::<client::ApiError>() {
        Some(e) if e.code == "repo_create_forbidden" => anyhow::anyhow!(
            "{} ({}); owners of {org} can change this with `pyn org policy`",
            e.message,
            e.code
        ),
        Some(e) if e.code == "not_org_member" => {
            anyhow::anyhow!("you are not a member of {org} ({})", e.code)
        }
        _ => err,
    }
}

fn parse_visibility(v: &str) -> Result<api::Visibility> {
    match v {
        "public" => Ok(api::Visibility::Public),
        "private" => Ok(api::Visibility::Private),
        other => bail!("unknown visibility {other:?}: use public or private"),
    }
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
            let visibility = visibility.as_deref().map(parse_visibility).transpose()?;
            let body = api::CreateRepoRequest {
                owner,
                name,
                visibility,
                lease_hours,
                max_locks_per_user: max_locks,
            };
            let org = body.owner.clone().unwrap_or_default();
            let made: api::RepoInfo = api
                .send(api.request(Method::POST, "/v1/repos").json(&body))
                .map_err(|e| explain_create_refusal(e, &org))?
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
        RepoCommand::Explore { limit } => {
            let mut repos: Vec<api::RepoInfo> = Vec::new();
            let mut after: Option<String> = None;
            while repos.len() < limit {
                let mut query = vec![("limit", (limit - repos.len()).min(200).to_string())];
                query.extend(after.take().map(|a| ("after", a)));
                let page: api::RepoPage = api
                    .send(api.get("/v1/explore/repos").query(&query))?
                    .json()?;
                repos.extend(page.repos);
                after = page.next_after;
                if after.is_none() {
                    break;
                }
            }
            let rows: Vec<Vec<String>> = repos
                .into_iter()
                .map(|r| {
                    vec![
                        r.role.unwrap_or_else(|| "-".into()),
                        format!("{}/{}", r.owner, r.name),
                    ]
                })
                .collect();
            table::show(&["ROLE", "REPOSITORY"], &rows, "no public repositories");
        }
        RepoCommand::Visibility { name, visibility } => {
            let (owner, short) = address::split_repo(&name)?;
            let body = api::UpdateRepoRequest {
                visibility: Some(parse_visibility(&visibility)?),
                ..Default::default()
            };
            api.send(
                api.request(Method::PATCH, &format!("/v1/repos/{owner}/{short}"))
                    .json(&body),
            )?;
            println!("{name} is now {visibility}");
        }
        RepoCommand::Usage { name } => {
            let (owner, short) = address::split_repo(&name)?;
            limits::repo_usage(api, owner, short)?;
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

fn org_command(api: &Api, cmd: OrgCommand) -> Result<()> {
    match cmd {
        OrgCommand::Create { name } => {
            let body = api::CreateOrgRequest { name };
            let made: api::OrgInfo = api
                .send(api.request(Method::POST, "/v1/orgs").json(&body))?
                .json()?;
            println!("created organization {}", made.name);
        }
        OrgCommand::List => {
            let orgs: Vec<api::OrgInfo> = api.send(api.get("/v1/orgs"))?.json()?;
            let rows: Vec<Vec<String>> = orgs
                .into_iter()
                .map(|o| {
                    vec![
                        o.role.unwrap_or_else(|| "-".into()),
                        time::local(o.created_at),
                        o.name,
                    ]
                })
                .collect();
            table::show(
                &["ROLE", "CREATED", "ORGANIZATION"],
                &rows,
                "no organizations",
            );
        }
        OrgCommand::Show { name } => {
            let org: api::OrgInfo = api.send(api.get(&format!("/v1/orgs/{name}")))?.json()?;
            println!("organization  {}", org.name);
            println!("created       {}", time::local(org.created_at));
            println!("your role     {}", org.role.as_deref().unwrap_or("-"));
            if org.role.is_some() {
                println!();
                org_members(api, &org.name)?;
            }
        }
        OrgCommand::Delete { name, yes } => {
            if !yes {
                eprintln!(
                    "This removes the organization {name} and its memberships. It must own no repositories. Its audit log is kept."
                );
                eprint!("Type the organization name to confirm: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != name {
                    bail!("not confirmed");
                }
            }
            api.send(api.request(Method::DELETE, &format!("/v1/orgs/{name}")))?;
            println!("deleted organization {name}");
        }
        OrgCommand::Audit {
            name,
            before,
            limit,
        } => {
            let mut req = api
                .get(&format!("/v1/orgs/{name}/audit"))
                .query(&[("limit", limit)]);
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
                        time::localize(&e.detail),
                    ]
                })
                .collect();
            table::show(
                &["ID", "WHEN", "ACTOR", "ACTION", "DETAIL"],
                &rows,
                "no audit entries",
            );
            if let Some(next) = page.next_before {
                eprintln!("more: --before {next}");
            }
        }
        OrgCommand::Member(cmd) => org_member_command(api, cmd)?,
        OrgCommand::Policy(cmd) => org_policy_command(api, cmd)?,
    }
    Ok(())
}

fn team_command(api: &Api, cmd: TeamCommand) -> Result<()> {
    match cmd {
        TeamCommand::Create {
            team,
            name,
            description,
        } => {
            let (org, slug) = address::split_team(&team)?;
            let body = api::CreateTeamRequest {
                slug: slug.into(),
                name,
                description,
            };
            let made: api::TeamInfo = api
                .send(
                    api.request(Method::POST, &format!("/v1/orgs/{org}/teams"))
                        .json(&body),
                )?
                .json()?;
            println!("created team {org}/{}", made.slug);
        }
        TeamCommand::List { org } => {
            let teams: Vec<api::TeamInfo> = api
                .send(api.get(&format!("/v1/orgs/{org}/teams")))?
                .json()?;
            let rows: Vec<Vec<String>> = teams
                .into_iter()
                .map(|t| {
                    vec![
                        t.members.len().to_string(),
                        t.repos.len().to_string(),
                        time::local(t.created_at),
                        t.name,
                        t.slug,
                    ]
                })
                .collect();
            table::show(
                &["MEMBERS", "REPOS", "CREATED", "NAME", "TEAM"],
                &rows,
                "no teams",
            );
        }
        TeamCommand::Show { team } => {
            let (org, slug) = address::split_team(&team)?;
            let t: api::TeamInfo = api
                .send(api.get(&format!("/v1/orgs/{org}/teams/{slug}")))?
                .json()?;
            println!("team         {org}/{}", t.slug);
            println!("name         {}", t.name);
            println!("description  {}", t.description);
            println!("created      {}", time::local(t.created_at));
            println!();
            let rows: Vec<Vec<String>> = t.members.into_iter().map(|m| vec![m]).collect();
            table::show(&["MEMBER"], &rows, "no members");
            println!();
            let rows: Vec<Vec<String>> =
                t.repos.into_iter().map(|r| vec![r.role, r.repo]).collect();
            table::show(&["ROLE", "REPOSITORY"], &rows, "no repositories");
        }
        TeamCommand::Delete { team, yes } => {
            let (org, slug) = address::split_team(&team)?;
            if !yes {
                eprintln!(
                    "This removes the team {team}, its memberships and its roles on repositories."
                );
                eprint!("Type the team name to confirm: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim() != team {
                    bail!("not confirmed");
                }
            }
            api.send(api.request(Method::DELETE, &format!("/v1/orgs/{org}/teams/{slug}")))?;
            println!("deleted team {team}");
        }
        TeamCommand::Member(TeamMemberCommand::Add { team, user }) => {
            let (org, slug) = address::split_team(&team)?;
            api.send(api.request(
                Method::PUT,
                &format!("/v1/orgs/{org}/teams/{slug}/members/{user}"),
            ))?;
            println!("added {user} to {team}");
        }
        TeamCommand::Member(TeamMemberCommand::Remove { team, user }) => {
            let (org, slug) = address::split_team(&team)?;
            api.send(api.request(
                Method::DELETE,
                &format!("/v1/orgs/{org}/teams/{slug}/members/{user}"),
            ))?;
            println!("removed {user} from {team}");
        }
        TeamCommand::Grant { team, repo, role } => {
            let (_, slug) = address::split_team(&team)?;
            address::split_repo(&repo)?;
            let body = api::SetTeamRoleRequest { role: role.clone() };
            api.send(
                api.request(Method::PUT, &format!("/v1/repos/{repo}/teams/{slug}"))
                    .json(&body),
            )?;
            println!("{team} is now {role} on {repo}");
        }
        TeamCommand::Revoke { team, repo } => {
            let (_, slug) = address::split_team(&team)?;
            address::split_repo(&repo)?;
            api.send(api.request(Method::DELETE, &format!("/v1/repos/{repo}/teams/{slug}")))?;
            println!("{team} no longer has a role on {repo}");
        }
        TeamCommand::Access { repo } => {
            let route = match repo {
                Some(r) => {
                    address::split_repo(&r)?;
                    format!("/v1/repos/{r}/teams")
                }
                None => api.repo_route("/teams")?,
            };
            let teams: Vec<api::RepoTeam> = api.send(api.get(&route))?.json()?;
            let rows: Vec<Vec<String>> = teams
                .into_iter()
                .map(|t| vec![t.role, t.name, t.slug])
                .collect();
            table::show(&["ROLE", "NAME", "TEAM"], &rows, "no teams");
        }
    }
    Ok(())
}

fn org_policy_command(api: &Api, cmd: OrgPolicyCommand) -> Result<()> {
    match cmd {
        OrgPolicyCommand::Show { org } => {
            let policy: api::RepoPolicyInfo = api
                .send(api.get(&format!("/v1/orgs/{org}/repo-policy")))?
                .json()?;
            println!("members can create  {}", policy.member_creation);
            println!();
            let rows: Vec<Vec<String>> = policy
                .rules
                .into_iter()
                .map(|r| vec![r.effect, r.kind, r.scope, r.subject])
                .collect();
            table::show(&["EFFECT", "KIND", "SCOPE", "SUBJECT"], &rows, "no rules");
        }
        OrgPolicyCommand::Set { org, members } => {
            let body = api::SetRepoPolicyRequest {
                member_creation: members,
            };
            let set: api::RepoPolicyInfo = api
                .send(
                    api.request(Method::PUT, &format!("/v1/orgs/{org}/repo-policy"))
                        .json(&body),
                )?
                .json()?;
            println!("members of {org} can create: {}", set.member_creation);
        }
        OrgPolicyCommand::Allow {
            org,
            kind,
            subject,
            scope,
        } => set_creation_rule(api, &org, "allow", &kind, &subject, scope)?,
        OrgPolicyCommand::Deny {
            org,
            kind,
            subject,
            scope,
        } => set_creation_rule(api, &org, "deny", &kind, &subject, scope)?,
        OrgPolicyCommand::Remove {
            org,
            effect,
            kind,
            subject,
        } => {
            api.send(api.request(
                Method::DELETE,
                &format!("/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}"),
            ))?;
            println!("removed the {effect} rule for {kind} {subject} in {org}");
        }
    }
    Ok(())
}

fn set_creation_rule(
    api: &Api,
    org: &str,
    effect: &str,
    kind: &str,
    subject: &str,
    scope: String,
) -> Result<()> {
    let body = api::SetCreationRuleRequest { scope };
    let rule: api::CreationRuleInfo = api
        .send(
            api.request(
                Method::PUT,
                &format!("/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}"),
            )
            .json(&body),
        )?
        .json()?;
    println!(
        "{org}: {} {} {} ({} repositories)",
        rule.effect, rule.kind, rule.subject, rule.scope
    );
    Ok(())
}

fn org_members(api: &Api, org: &str) -> Result<()> {
    let members: Vec<api::OrgMember> = api
        .send(api.get(&format!("/v1/orgs/{org}/members")))?
        .json()?;
    let rows: Vec<Vec<String>> = members.into_iter().map(|m| vec![m.role, m.user]).collect();
    table::show(&["ROLE", "USER"], &rows, "no members");
    Ok(())
}

fn org_member_command(api: &Api, cmd: OrgMemberCommand) -> Result<()> {
    match cmd {
        OrgMemberCommand::List { org } => org_members(api, &org)?,
        OrgMemberCommand::Add { org, user, role } => {
            let body = api::AddOrgMemberRequest {
                user: user.clone(),
                role,
            };
            let added: api::OrgMember = api
                .send(
                    api.request(Method::POST, &format!("/v1/orgs/{org}/members"))
                        .json(&body),
                )?
                .json()?;
            println!("added {user} to {org} as {}", added.role);
        }
        OrgMemberCommand::Set { org, user, role } => {
            let body = api::SetOrgRoleRequest { role: role.clone() };
            api.send(
                api.request(Method::PATCH, &format!("/v1/orgs/{org}/members/{user}"))
                    .json(&body),
            )?;
            println!("{user} is now {role} of {org}");
        }
        OrgMemberCommand::Remove { org, user } => {
            api.send(api.request(Method::DELETE, &format!("/v1/orgs/{org}/members/{user}")))?;
            println!("removed {user} from {org}");
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

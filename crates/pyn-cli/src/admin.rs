//! Server administration: first-run setup, accounts, administrators, organizations, service credentials and webhooks.

use crate::transport::Method;
use anyhow::{Result, bail};
use clap::Subcommand;
use pyn_proto as api;

use crate::client::Api;
use crate::{credentials, table, time};

#[derive(Subcommand)]
pub enum AdminCommand {
    /// List and review accounts.
    #[command(subcommand)]
    User(AdminUserCommand),
    /// Act as an account for at most an hour, for a stated reason; the account is told and it is audited.
    #[command(subcommand)]
    AccessSession(crate::access_session::AccessSessionCommand),
    /// Make an active account a server administrator.
    Grant { user: String },
    /// Take the administrator flag away; the last active administrator cannot be revoked.
    Revoke { user: String },
    /// Create, list and revoke service credentials for automation.
    #[command(subcommand)]
    ServiceCredential(ServiceCredentialCommand),
    /// Create and delete organizations on behalf of an owner.
    #[command(subcommand)]
    Org(AdminOrgCommand),
    /// Freeze a user or organization: it keeps every byte and stays readable, but every write under it is refused.
    Freeze {
        owner: String,
        /// Shown to the owner's members.
        #[arg(long)]
        reason: String,
    },
    /// Lift a freeze.
    Unfreeze { owner: String },
    /// Show whether a user or organization is frozen.
    Frozen { owner: String },
    /// Optional per-owner limits on repositories, members and stored content.
    #[command(subcommand)]
    Limits(crate::limits::LimitsCommand),
    /// Webhook subscriptions for lifecycle events, and their signing secrets.
    #[command(subcommand, alias = "webhooks")]
    Webhook(crate::webhook::WebhookCommand),
    /// Invitations to the server itself: a one-time code, optionally emailed, with an optional role.
    #[command(subcommand)]
    Invite(InviteCommand),
}

#[derive(Subcommand)]
pub enum InviteCommand {
    /// Create an invitation; its code is shown once.
    Create {
        /// Send the code to this address when the server has a mailer.
        #[arg(long)]
        email: Option<String>,
        /// Days the code works, 1 to 90; the server's default is 7.
        #[arg(long)]
        days: Option<u32>,
        /// Also give a role in this personal repository, written owner/name (with --role).
        #[arg(long, requires = "role", conflicts_with = "org")]
        repo: Option<String>,
        /// Also make the person a member of this organization with --role (owner or member).
        #[arg(long, requires = "role")]
        org: Option<String>,
        /// The role in the repository or organization.
        #[arg(long)]
        role: Option<String>,
    },
    /// List invitations, newest first; never their codes.
    List,
    /// Revoke an unused invitation by its id.
    Revoke { id: String },
}

#[derive(Subcommand)]
pub enum AdminUserCommand {
    /// List accounts, oldest first.
    List {
        /// Only this state: pending_verification, pending_approval, active or disabled.
        #[arg(long)]
        status: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Add an account with a one-time password the person must change at first sign-in.
    Add {
        name: String,
        /// Read the password from one line of standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Send a fresh confirmation link to an account still waiting on its email address.
    ResendConfirmation { user: String },
    /// Approve an account that waits for approval.
    Approve { user: String },
    /// Disable an account: it cannot sign in and its tokens and keys stop working.
    Disable {
        user: String,
        /// Shown to administrators, not to the account holder.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Enable a disabled account.
    Enable { user: String },
    /// Delete a disabled account after the recovery window, or with --now at once. The reason goes in the audit log.
    Delete {
        user: String,
        /// Delete at once instead of after the recovery window.
        #[arg(long)]
        now: bool,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Subcommand)]
pub enum ServiceCredentialCommand {
    /// Create a credential. Its secret is shown once.
    Create {
        name: String,
        /// Comma-separated scopes: manage_accounts, manage_organizations, manage_limits, manage_webhooks, manage_freeze.
        #[arg(long, value_delimiter = ',', required = true)]
        scopes: Vec<String>,
        /// Refuse the credential from this instant on (RFC 3339, for example 2027-01-01T00:00:00Z).
        #[arg(long)]
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
        /// Comma-separated addresses or CIDR blocks the credential may be used from; default any.
        #[arg(long, value_delimiter = ',')]
        allow_from: Vec<String>,
        /// Requests per minute this credential may make; default the server's limit.
        #[arg(long)]
        rate_limit: Option<u32>,
    },
    /// List credentials, revoked ones included.
    List,
    /// Revoke a credential by name; its name is never reused.
    Revoke { name: String },
}

#[derive(Subcommand)]
pub enum AdminOrgCommand {
    /// Create an organization whose first owner is an existing active user.
    Create {
        name: String,
        #[arg(long)]
        owner: String,
    },
    /// Delete an organization that owns no repositories. Its audit log is kept.
    Delete {
        name: String,
        /// Skip the typed confirmation.
        #[arg(long)]
        yes: bool,
    },
}

pub struct SetupArgs {
    pub username: String,
    pub setup_token: Option<String>,
    pub email: Option<String>,
    pub server_name: Option<String>,
    pub public_url: Option<String>,
    pub registration: Option<String>,
    pub password_stdin: bool,
}

/// Runs first-run setup; it needs no sign-in and does not sign the new administrator in.
pub fn setup(api: &Api, args: SetupArgs, interactive: bool) -> Result<()> {
    let status: api::SetupStatus = api.send(api.anonymous(Method::GET, "/v1/setup"))?.json()?;
    if status.initialised {
        bail!("{} is already set up", api.base);
    }
    let token = match args.setup_token {
        Some(t) => t,
        None if interactive && !args.password_stdin => {
            credentials::read_password("Setup token: ", false)?
        }
        None => bail!("pass the setup token with --setup-token or PYN_SETUP_TOKEN"),
    };
    let password = credentials::read_new_password(args.password_stdin)?;
    let body = api::SetupRequest {
        token,
        username: args.username,
        password,
        email: args.email,
        server_name: args.server_name,
        public_url: args.public_url,
        registration: args.registration.unwrap_or(status.registration),
        access_mode: None,
    };
    let done: api::SetupCompleted = api
        .send(api.anonymous(Method::POST, "/v1/setup").json(&body))?
        .json()?;
    println!(
        "{} is set up with {} as its first administrator; sign in with `pyn login {}`",
        api.base, done.user, done.user
    );
    eprintln!(
        "next: sign in and make a second administrator with `pyn admin grant <user>`, so one lost account does not need host recovery"
    );
    Ok(())
}

pub fn run(api: &Api, cmd: AdminCommand) -> Result<()> {
    match cmd {
        AdminCommand::User(cmd) => user(api, cmd),
        AdminCommand::Grant { user } => {
            let who: api::AccountInfo = api
                .send(api.request(Method::PUT, &format!("/v1/admin/users/{user}/admin")))?
                .json()?;
            println!("{} is a server administrator", who.user);
            Ok(())
        }
        AdminCommand::Revoke { user } => {
            let who: api::AccountInfo = api
                .send(api.request(Method::DELETE, &format!("/v1/admin/users/{user}/admin")))?
                .json()?;
            println!("{} is not a server administrator", who.user);
            Ok(())
        }
        AdminCommand::AccessSession(cmd) => crate::access_session::run(api, cmd),
        AdminCommand::ServiceCredential(cmd) => service_credential(api, cmd),
        AdminCommand::Org(cmd) => org(api, cmd),
        AdminCommand::Freeze { owner, reason } => crate::freeze::freeze(api, &owner, reason),
        AdminCommand::Unfreeze { owner } => crate::freeze::unfreeze(api, &owner),
        AdminCommand::Frozen { owner } => crate::freeze::show(api, &owner),
        AdminCommand::Limits(cmd) => crate::limits::run(api, cmd),
        AdminCommand::Webhook(cmd) => crate::webhook::run(api, cmd),
        AdminCommand::Invite(cmd) => invite(api, cmd),
    }
}

fn invite(api: &Api, cmd: InviteCommand) -> Result<()> {
    match cmd {
        InviteCommand::Create {
            email,
            days,
            repo,
            org,
            role,
        } => {
            let grant = role.map(|role| api::ServerInvitationGrant {
                repository: repo,
                organization: org,
                role,
            });
            let body = api::CreateServerInvitationRequest {
                email,
                expires_in_days: days,
                grant,
            };
            let made: api::CreatedServerInvitation = api
                .send(
                    api.request(Method::POST, "/v1/admin/invitations")
                        .json(&body),
                )?
                .json()?;
            println!("{}", made.code);
            let sent = if made.emailed {
                "emailed"
            } else {
                "not emailed"
            };
            eprintln!(
                "invitation {} created, expires {}, {sent}; this is the only time the code is shown",
                made.invitation.id,
                time::local(made.invitation.expires_at)
            );
        }
        InviteCommand::List => {
            let all: Vec<api::ServerInvitationInfo> =
                api.send(api.get("/v1/admin/invitations"))?.json()?;
            let rows: Vec<Vec<String>> = all
                .into_iter()
                .map(|i| {
                    let grant = i.grant.map_or("-".to_string(), |g| {
                        let target = g.repository.or(g.organization).unwrap_or_default();
                        format!("{}@{target}", g.role)
                    });
                    vec![
                        format!("{:?}", i.status).to_lowercase(),
                        time::local(i.expires_at),
                        i.created_by,
                        i.email.unwrap_or_else(|| "-".into()),
                        grant,
                        i.id,
                    ]
                })
                .collect();
            table::show(
                &["STATE", "EXPIRES", "BY", "EMAIL", "GRANT", "ID"],
                &rows,
                "no invitations",
            );
        }
        InviteCommand::Revoke { id } => {
            api.send(api.request(Method::DELETE, &format!("/v1/admin/invitations/{id}")))?;
            println!("revoked {id}");
        }
    }
    Ok(())
}

fn user(api: &Api, cmd: AdminUserCommand) -> Result<()> {
    match cmd {
        AdminUserCommand::List { status, limit } => {
            let mut req = api.get("/v1/admin/users").query(&[("limit", limit)]);
            if let Some(s) = &status {
                req = req.query(&[("status", s)]);
            }
            let accounts: Vec<api::AccountInfo> = api.send(req)?.json()?;
            let rows: Vec<Vec<String>> = accounts
                .into_iter()
                .map(|a| {
                    vec![
                        a.status,
                        if a.admin { "admin" } else { "-" }.to_string(),
                        time::local(a.created_at),
                        a.email.unwrap_or_else(|| "-".into()),
                        a.user,
                    ]
                })
                .collect();
            table::show(
                &["STATUS", "ROLE", "CREATED", "EMAIL", "USER"],
                &rows,
                "no accounts",
            );
        }
        AdminUserCommand::Add {
            name,
            password_stdin,
        } => {
            let password = credentials::read_new_password(password_stdin)?;
            let body = api::AddAccountRequest {
                username: name,
                password,
            };
            let added: api::AccountInfo = api
                .send(api.request(Method::POST, "/v1/admin/users").json(&body))?
                .json()?;
            println!(
                "added {}; the password is one-time and must be changed at first sign-in",
                added.user
            );
        }
        AdminUserCommand::ResendConfirmation { user } => {
            api.send(api.request(
                Method::POST,
                &format!("/v1/admin/users/{user}/resend-confirmation"),
            ))?;
            println!("confirmation sent to {user}");
        }
        AdminUserCommand::Approve { user } => {
            let who = account_action(api, &user, "approve", None)?;
            println!("approved {}", who.user);
        }
        AdminUserCommand::Disable { user, reason } => {
            let body = api::DisableAccountRequest { reason };
            let who = account_action(api, &user, "disable", Some(&body))?;
            println!("disabled {}", who.user);
        }
        AdminUserCommand::Enable { user } => {
            let who = account_action(api, &user, "enable", None)?;
            println!("enabled {}", who.user);
        }
        AdminUserCommand::Delete { user, now, reason } => {
            let body = api::AdminDeleteAccountRequest {
                reason,
                immediately: now,
            };
            let done: api::AdminDeleteAccountResult = api
                .send(
                    api.request(Method::POST, &format!("/v1/admin/users/{user}/delete"))
                        .json(&body),
                )?
                .json()?;
            match done.purge_at {
                Some(at) if !done.deleted => println!(
                    "{} is scheduled for deletion on {}; `pyn admin user enable {}` cancels it",
                    done.user,
                    time::local(at),
                    done.user
                ),
                _ => println!("deleted {}", done.user),
            }
        }
    }
    Ok(())
}

fn account_action(
    api: &Api,
    user: &str,
    action: &str,
    body: Option<&api::DisableAccountRequest>,
) -> Result<api::AccountInfo> {
    let mut req = api.request(Method::POST, &format!("/v1/admin/users/{user}/{action}"));
    if let Some(b) = body {
        req = req.json(b);
    }
    api.send(req)?.json()
}

fn service_credential(api: &Api, cmd: ServiceCredentialCommand) -> Result<()> {
    match cmd {
        ServiceCredentialCommand::Create {
            name,
            scopes,
            expires_at,
            allow_from,
            rate_limit,
        } => {
            let body = api::CreateServiceCredentialRequest {
                name,
                scopes,
                expires_at,
                allowed_sources: allow_from,
                rate_limit_per_minute: rate_limit,
            };
            let made: api::CreatedServiceCredential = api
                .send(
                    api.request(Method::POST, "/v1/admin/service-credentials")
                        .json(&body),
                )?
                .json()?;
            println!("{}", made.secret);
            eprintln!(
                "service credential {} created; this is the only time the secret is shown",
                made.info.name
            );
        }
        ServiceCredentialCommand::List => {
            let all: Vec<api::ServiceCredentialInfo> =
                api.send(api.get("/v1/admin/service-credentials"))?.json()?;
            let rows: Vec<Vec<String>> = all
                .into_iter()
                .map(|c| {
                    let state = if c.revoked_at.is_some() {
                        "revoked"
                    } else if c.expires_at.is_some_and(|t| t <= chrono::Utc::now()) {
                        "expired"
                    } else {
                        "active"
                    };
                    vec![
                        state.to_string(),
                        time::local(c.created_at),
                        c.last_used_at.map_or("never".to_string(), time::local),
                        c.last_used_from.unwrap_or_else(|| "-".to_string()),
                        c.created_by,
                        c.scopes.join(","),
                        c.name,
                    ]
                })
                .collect();
            table::show(
                &[
                    "STATE",
                    "CREATED",
                    "LAST USED",
                    "FROM",
                    "BY",
                    "SCOPES",
                    "NAME",
                ],
                &rows,
                "no service credentials",
            );
        }
        ServiceCredentialCommand::Revoke { name } => {
            api.send(api.request(
                Method::DELETE,
                &format!("/v1/admin/service-credentials/{name}"),
            ))?;
            println!("revoked {name}");
        }
    }
    Ok(())
}

fn org(api: &Api, cmd: AdminOrgCommand) -> Result<()> {
    match cmd {
        AdminOrgCommand::Create { name, owner } => {
            let body = api::AdminCreateOrgRequest { name, owner };
            let made: api::OrgInfo = api
                .send(api.request(Method::POST, "/v1/admin/orgs").json(&body))?
                .json()?;
            println!(
                "created organization {} with {} as its owner",
                made.name, body.owner
            );
        }
        AdminOrgCommand::Delete { name, yes } => {
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
            api.send(api.request(Method::DELETE, &format!("/v1/admin/orgs/{name}")))?;
            println!("deleted organization {name}");
        }
    }
    Ok(())
}

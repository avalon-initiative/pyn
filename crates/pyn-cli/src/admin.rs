//! Server administration: first-run setup, accounts, administrators, organizations and service credentials.

use anyhow::{Result, bail};
use clap::Subcommand;
use pyn_proto as api;
use reqwest::Method;

use crate::client::Api;
use crate::{credentials, table, time};

#[derive(Subcommand)]
pub enum AdminCommand {
    /// List and review accounts.
    #[command(subcommand)]
    User(AdminUserCommand),
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
}

#[derive(Subcommand)]
pub enum ServiceCredentialCommand {
    /// Create a credential. Its secret is shown once.
    Create {
        name: String,
        /// Comma-separated scopes: manage_accounts, manage_organizations.
        #[arg(long, value_delimiter = ',', required = true)]
        scopes: Vec<String>,
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
    };
    let done: api::SetupCompleted = api
        .send(api.anonymous(Method::POST, "/v1/setup").json(&body))?
        .json()?;
    println!(
        "{} is set up with {} as its first administrator; sign in with `pyn login {}`",
        api.base, done.user, done.user
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
        AdminCommand::ServiceCredential(cmd) => service_credential(api, cmd),
        AdminCommand::Org(cmd) => org(api, cmd),
    }
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
    Ok(api.send(req)?.json()?)
}

fn service_credential(api: &Api, cmd: ServiceCredentialCommand) -> Result<()> {
    match cmd {
        ServiceCredentialCommand::Create { name, scopes } => {
            let body = api::CreateServiceCredentialRequest { name, scopes };
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
                    } else {
                        "active"
                    };
                    vec![
                        state.to_string(),
                        time::local(c.created_at),
                        c.last_used_at.map_or("never".to_string(), time::local),
                        c.created_by,
                        c.scopes.join(","),
                        c.name,
                    ]
                })
                .collect();
            table::show(
                &["STATE", "CREATED", "LAST USED", "BY", "SCOPES", "NAME"],
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

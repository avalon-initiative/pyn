//! `pyn admin access-session`: act as an account for at most an hour, with a reason (decision #1024).

use anyhow::Result;
use clap::Subcommand;
use pyn_proto as api;

use crate::client::Api;
use crate::transport::Method;
use crate::{json_out, table, time};

#[derive(Subcommand)]
pub enum AccessSessionCommand {
    /// Start a session as an account. The secret is printed once and works as a bearer token (`PYN_TOKEN`); the
    /// session ends by itself after an hour and the account is told.
    Start {
        user: String,
        /// One of abuse_report, security_incident, support_request, legal_request, other.
        #[arg(long)]
        reason: String,
        /// Required for `other`; the account sees them.
        #[arg(long)]
        notes: Option<String>,
    },
    /// End a session now.
    End { id: String },
    /// List sessions, newest first, with the routes each called.
    List {
        /// Only sessions on this account.
        #[arg(long)]
        user: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

pub fn run(api: &Api, cmd: AccessSessionCommand) -> Result<()> {
    match cmd {
        AccessSessionCommand::Start {
            user,
            reason,
            notes,
        } => {
            let body = api::StartAccessSessionRequest { reason, notes };
            let made: api::StartedAccessSession = api
                .send(
                    api.request(
                        Method::POST,
                        &format!("/v1/admin/users/{user}/access-sessions"),
                    )
                    .json(&body),
                )?
                .json()?;
            if json_out::on() {
                json_out::print(serde_json::json!({"session": made.info, "secret": made.secret}));
                return Ok(());
            }
            println!("{}", made.secret);
            eprintln!(
                "access session {} as {} until {}; this is the only time the secret is shown, and {} is told",
                made.info.id,
                made.info.user,
                time::local(made.info.expires_at),
                made.info.user
            );
        }
        AccessSessionCommand::End { id } => {
            let ended: api::AccessSessionInfo = api
                .send(api.request(Method::POST, &format!("/v1/admin/access-sessions/{id}/end")))?
                .json()?;
            if json_out::on() {
                json_out::print_as("session", &ended);
                return Ok(());
            }
            println!(
                "access session {} on {}: {}",
                ended.id, ended.user, ended.state
            );
        }
        AccessSessionCommand::List { user, limit } => {
            let mut req = api
                .get("/v1/admin/access-sessions")
                .query(&[("limit", limit)]);
            if let Some(u) = &user {
                req = req.query(&[("user", u)]);
            }
            let sessions: Vec<api::AccessSessionInfo> = api.send(req)?.json()?;
            if json_out::on() {
                json_out::print_as("sessions", &sessions);
                return Ok(());
            }
            let rows: Vec<Vec<String>> = sessions
                .into_iter()
                .map(|s| {
                    vec![
                        s.state,
                        s.id,
                        time::local(s.started_at),
                        time::local(s.expires_at),
                        s.admin,
                        s.reason,
                        s.reached.iter().map(|r| r.count).sum::<u64>().to_string(),
                        s.user,
                    ]
                })
                .collect();
            table::show(
                &[
                    "STATE", "ID", "STARTED", "ENDS", "BY", "REASON", "REQUESTS", "USER",
                ],
                &rows,
                "no access sessions",
            );
        }
    }
    Ok(())
}

//! The signed-in user's own account: deleting it, restoring it during the recovery window, and its deletion status.

use anyhow::Result;
use clap::Subcommand;
use pyn_proto as api;

use crate::client::Api;
use crate::transport::Method;
use crate::{credentials, time};

#[derive(Subcommand)]
pub enum AccountCommand {
    /// Delete your account. It is disabled now and removed for good after the recovery window.
    Delete {
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Cancel a scheduled deletion and use the account again; sign in first with `pyn login`.
    Restore,
    /// Show whether the account is scheduled for deletion and when it is removed.
    Status,
}

pub fn run(api: &Api, cmd: AccountCommand) -> Result<()> {
    match cmd {
        AccountCommand::Delete { password_stdin } => delete(api, password_stdin),
        AccountCommand::Restore => restore(api),
        AccountCommand::Status => status(api),
    }
}

fn delete(api: &Api, password_stdin: bool) -> Result<()> {
    let password = credentials::read_password("Password: ", password_stdin)?;
    let body = api::DeleteOwnAccountRequest { password };
    let done: api::AccountDeletionStatus = api
        .send(
            api.request(Method::POST, "/v1/account/deletion")
                .json(&body),
        )?
        .json()?;
    println!("{}", scheduled_line(&done));
    println!("to keep the account, sign in and run `pyn account restore` before then");
    Ok(())
}

fn restore(api: &Api) -> Result<()> {
    let done: api::AccountDeletionStatus = api
        .send(api.request(Method::POST, "/v1/account/deletion/restore"))?
        .json()?;
    println!("restored {}", done.user);
    Ok(())
}

fn status(api: &Api) -> Result<()> {
    let status: api::AccountDeletionStatus = api.send(api.get("/v1/account/deletion"))?.json()?;
    if status.pending {
        println!("{}", scheduled_line(&status));
        println!("run `pyn account restore` to keep it");
    } else {
        println!("{} is not scheduled for deletion", status.user);
    }
    Ok(())
}

fn scheduled_line(status: &api::AccountDeletionStatus) -> String {
    match status.purge_at {
        Some(at) => format!(
            "{} is scheduled for deletion on {}",
            status.user,
            time::local(at)
        ),
        None => format!("{} is not scheduled for deletion", status.user),
    }
}

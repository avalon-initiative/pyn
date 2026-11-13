//! `pyn account`: your own email addresses and username.

use anyhow::{Result, bail};
use clap::Subcommand;
use pyn_proto as api;

use crate::client::Api;
use crate::transport::{Method, Reply};
use crate::{credentials, table, time};

#[derive(Subcommand)]
pub enum AccountCommand {
    /// Add, list and choose your email addresses.
    #[command(subcommand)]
    Email(EmailCommand),
    /// Change your username. The old name becomes free for anyone to take.
    Rename {
        new_name: String,
        /// Skip the confirmation.
        #[arg(long)]
        yes: bool,
        /// Read the password from standard input instead of prompting.
        #[arg(long)]
        password_stdin: bool,
    },
}

#[derive(Subcommand)]
pub enum EmailCommand {
    /// List your addresses.
    List,
    /// Add an address; a link is mailed to it to verify it.
    Add { address: String },
    /// Make a verified address the one the server uses for your account.
    Primary { address: String },
    /// Remove an address.
    Remove { address: String },
    /// Mail the verification link again.
    Resend { address: String },
}

pub fn run(api: &Api, command: AccountCommand) -> Result<()> {
    match command {
        AccountCommand::Email(cmd) => email(api, cmd),
        AccountCommand::Rename {
            new_name,
            yes,
            password_stdin,
        } => rename(api, &new_name, yes, password_stdin),
    }
}

fn email(api: &Api, cmd: EmailCommand) -> Result<()> {
    match cmd {
        EmailCommand::List => {
            let held: Vec<api::AccountEmail> = api.send(api.get("/v1/me/emails"))?.json()?;
            if held.is_empty() {
                println!("no addresses");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = held
                .iter()
                .map(|e| {
                    vec![
                        if e.primary { "primary" } else { "" }.to_string(),
                        if e.verified { "verified" } else { "pending" }.to_string(),
                        time::local(e.added_at),
                        e.address.clone(),
                    ]
                })
                .collect();
            println!(
                "{}",
                table::render(&["PRIMARY", "STATUS", "ADDED", "ADDRESS"], &rows)
            );
        }
        EmailCommand::Add { address } => {
            let body = api::AddEmailRequest {
                address: address.clone(),
            };
            let added: api::AccountEmail = api
                .send(api.request(Method::POST, "/v1/me/emails").json(&body))?
                .json()?;
            println!(
                "added {}; open the link sent to it to verify it",
                added.address
            );
        }
        EmailCommand::Primary { address } => {
            api.send(api.request(Method::PUT, &route(&address, "/primary")))?;
            println!("{address} is now your primary address");
        }
        EmailCommand::Remove { address } => {
            api.send(api.request(Method::DELETE, &route(&address, "")))?;
            println!("removed {address}");
        }
        EmailCommand::Resend { address } => {
            api.send(api.request(Method::POST, &route(&address, "/resend")))?;
            println!("sent a new link to {address}");
        }
    }
    Ok(())
}

fn route(address: &str, tail: &str) -> String {
    format!("/v1/me/emails/{}{tail}", encode(address))
}

/// Percent-encodes everything but the characters an address can carry in a path segment untouched.
fn encode(address: &str) -> String {
    let mut out = String::new();
    for b in address.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' | b'+' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn rename(api: &Api, new_name: &str, yes: bool, password_stdin: bool) -> Result<()> {
    let old = api.send(api.get("/v1/me"))?.json::<api::Account>()?.user;
    if !yes {
        eprintln!("Renaming {old} to {new_name}:");
        eprintln!("  - {old} becomes free: anyone may take that name.");
        eprintln!(
            "  - Your repositories move to {new_name}, and their {old}/... addresses keep redirecting to them."
        );
        eprintln!(
            "  - A redirect stops, one repository at a time, when someone creates a repository with the same name under {old}."
        );
        eprintln!(
            "  - Links and remotes that use {old} can then reach the new holder's repositories instead: update them after renaming."
        );
        eprint!("Type the new name to confirm: ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if answer.trim() != new_name {
            bail!("not confirmed; nothing was changed");
        }
    }
    let password = credentials::read_password("Password: ", password_stdin)?;
    let body = api::RenameRequest {
        username: new_name.to_string(),
        password: Some(password),
    };
    let done: api::Renamed = api
        .send(api.request(Method::PUT, "/v1/me/username").json(&body))?
        .json()?;
    println!("renamed {} to {}", done.old_name, done.new_name);
    if let (Some(token), Some(saved)) = (&api.token, credentials::load(&api.base)?)
        && &saved.token == token
    {
        credentials::save(
            &api.base,
            credentials::Entry {
                user: done.new_name.clone(),
                token: saved.token,
            },
        )?;
    }
    if !done.repositories.is_empty() {
        println!(
            "update your remotes: {}/... is now {}/...",
            done.old_name, done.new_name
        );
    }
    Ok(())
}

/// Says once when the server answered for an address that has moved, so the remote can be updated.
pub fn note_moved(api: &Api, reply: &Reply) {
    static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let Some(now) = reply.header(api::MOVED_TO_HEADER) else {
        return;
    };
    if SAID.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let now = crate::sanitize::text(now);
    match &api.repo {
        Some(old) => {
            eprintln!("note: {old} has moved to {now}; update your remote and links to use {now}")
        }
        None => eprintln!(
            "note: that repository has moved to {now}; update your remote and links to use {now}"
        ),
    }
}

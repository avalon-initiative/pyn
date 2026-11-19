//! `pyn org settings`: an organization's repository defaults.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};
use pyn_proto as api;
use reqwest::Method;

use crate::client::Api;

#[derive(Clone, Copy, ValueEnum)]
pub enum Switch {
    On,
    Off,
    Unset,
}

#[derive(Subcommand)]
pub enum OrgSettingsCommand {
    /// Show the settings.
    Show { org: String },
    /// Change the settings; one left out is cleared.
    Set {
        org: String,
        /// Whether exclusive files are read-only until their lock is held, for repositories whose `.pyn/pyn.toml` does
        /// not say. `unset` follows the built-in default (on).
        #[arg(long, value_enum)]
        exclusive_read_only: Option<Switch>,
    },
}

fn show(org: &str, settings: &api::OrgSettingsInfo) {
    let word = match settings.exclusive_read_only {
        Some(true) => "on",
        Some(false) => "off",
        None => "unset (on)",
    };
    println!("{org}: exclusive files read-only until locked  {word}");
}

pub fn run(api: &Api, cmd: OrgSettingsCommand) -> Result<()> {
    match cmd {
        OrgSettingsCommand::Show { org } => {
            let settings: api::OrgSettingsInfo = api
                .send(api.get(&format!("/v1/orgs/{org}/settings")))?
                .json()?;
            show(&org, &settings);
        }
        OrgSettingsCommand::Set {
            org,
            exclusive_read_only,
        } => {
            let body = api::SetOrgSettingsRequest {
                exclusive_read_only: match exclusive_read_only {
                    Some(Switch::On) => Some(true),
                    Some(Switch::Off) => Some(false),
                    Some(Switch::Unset) | None => None,
                },
            };
            let set: api::OrgSettingsInfo = api
                .send(
                    api.request(Method::PUT, &format!("/v1/orgs/{org}/settings"))
                        .json(&body),
                )?
                .json()?;
            show(&org, &set);
        }
    }
    Ok(())
}

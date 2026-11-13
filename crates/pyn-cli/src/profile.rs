//! Account profiles: show anyone's, edit your own text fields and avatar.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use pyn_proto as api;

use crate::client::Api;
use crate::sanitize::text;
use crate::transport::Method;
use crate::{table, time};

#[derive(Subcommand)]
pub enum ProfileCommand {
    /// Show a user's profile and the repositories and organizations you may see; yours when no user is named.
    Show { user: Option<String> },
    /// Change your profile; fields you leave out stay as they are, and an empty value clears one.
    Set {
        /// Display name, at most 64 characters.
        #[arg(long)]
        name: Option<String>,
        /// A short bio, at most 160 characters.
        #[arg(long)]
        bio: Option<String>,
        #[arg(long)]
        location: Option<String>,
        #[arg(long)]
        company: Option<String>,
        /// An http(s) link; repeat for up to 5. Giving any replaces the list, and `--link ""` alone clears it.
        #[arg(long = "link")]
        links: Vec<String>,
    },
    /// Set or remove your avatar.
    #[command(subcommand)]
    Avatar(AvatarCommand),
}

#[derive(Subcommand)]
pub enum AvatarCommand {
    /// Upload a png, jpeg or webp image of at most 1 MiB.
    Set { file: PathBuf },
    /// Remove your avatar.
    Remove,
}

pub fn run(api: &Api, cmd: ProfileCommand) -> Result<()> {
    match cmd {
        ProfileCommand::Show { user } => show(api, user),
        ProfileCommand::Set {
            name,
            bio,
            location,
            company,
            links,
        } => set(api, name, bio, location, company, links),
        ProfileCommand::Avatar(AvatarCommand::Set { file }) => {
            let bytes =
                std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
            let req = api
                .request(Method::PUT, "/v1/me/avatar")
                .header("content-type", "application/octet-stream")
                .body(bytes);
            api.send(req)?;
            println!("avatar updated");
            Ok(())
        }
        ProfileCommand::Avatar(AvatarCommand::Remove) => {
            api.send(api.request(Method::DELETE, "/v1/me/avatar"))?;
            println!("avatar removed");
            Ok(())
        }
    }
}

fn show(api: &Api, user: Option<String>) -> Result<()> {
    let user = match user {
        Some(user) => user,
        None => api.send(api.get("/v1/me"))?.json::<api::Account>()?.user,
    };
    let page: api::PublicProfile = api.send(api.get(&format!("/v1/users/{user}")))?.json()?;
    let p = &page.profile;
    let mut fields: Vec<(&str, String)> = vec![("User", text(&page.user).into_owned())];
    let optional = [
        ("Name", &p.display_name),
        ("Bio", &p.bio),
        ("Location", &p.location),
        ("Company", &p.company),
    ];
    for (label, value) in optional {
        if let Some(v) = value {
            fields.push((label, text(v).into_owned()));
        }
    }
    if !p.links.is_empty() {
        let links: Vec<_> = p.links.iter().map(|l| text(l).into_owned()).collect();
        fields.push(("Links", links.join(", ")));
    }
    if let Some(url) = &p.avatar_url {
        fields.push(("Avatar", text(url).into_owned()));
    }
    fields.push(("Joined", time::local(page.created_at)));
    if !page.organizations.is_empty() {
        let orgs: Vec<_> = page
            .organizations
            .iter()
            .map(|o| text(&o.name).into_owned())
            .collect();
        fields.push(("Organizations", orgs.join(", ")));
    }
    for (label, value) in fields {
        println!("{:<14}{value}", format!("{label}:"));
    }
    println!("\nRepositories ({})", page.repository_count);
    let rows: Vec<Vec<String>> = page
        .repositories
        .iter()
        .map(|r| {
            let visibility = match r.visibility {
                api::Visibility::Public => "public",
                api::Visibility::Private => "private",
            };
            let role = r.role.clone().unwrap_or_else(|| "-".into());
            vec![visibility.into(), role, format!("{}/{}", r.owner, r.name)]
        })
        .collect();
    table::show(
        &["VISIBILITY", "ROLE", "REPOSITORY"],
        &rows,
        "no repositories",
    );
    Ok(())
}

fn set(
    api: &Api,
    name: Option<String>,
    bio: Option<String>,
    location: Option<String>,
    company: Option<String>,
    links: Vec<String>,
) -> Result<()> {
    if name.is_none()
        && bio.is_none()
        && location.is_none()
        && company.is_none()
        && links.is_empty()
    {
        bail!("nothing to change: pass --name, --bio, --location, --company or --link");
    }
    let current = api.send(api.get("/v1/me"))?.json::<api::Account>()?.profile;
    let body = api::UpdateProfileRequest {
        display_name: name.or(current.display_name),
        bio: bio.or(current.bio),
        location: location.or(current.location),
        company: company.or(current.company),
        links: if links.is_empty() {
            current.links
        } else {
            links
        },
    };
    api.send(api.request(Method::PUT, "/v1/me/profile").json(&body))?;
    println!("profile updated");
    Ok(())
}

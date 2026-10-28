//! The repository's mailmap and `pyn check-mailmap`: `.mailmap`, `mailmap.blob`, `mailmap.file`, in git's order.

use std::io::BufRead;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::Mailmap;

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes};

/// Show the canonical name and email of each contact.
#[derive(Debug, Args)]
pub struct CheckMailmapArgs {
    /// Read more contacts, one per line, from standard input.
    #[arg(long)]
    stdin: bool,
    /// Contacts as `Name <user@host>` or `<user@host>`.
    contacts: Vec<String>,
}

impl GitHome {
    /// The mailmap git applies here; unreadable or missing sources are skipped.
    pub(super) fn mailmap(&self) -> Result<Mailmap> {
        let mut map = Mailmap::new();
        let blob = match self.config().get("mailmap.blob") {
            Some(spec) => Some(spec),
            None if self.is_bare() => Some("HEAD:.mailmap"),
            None => None,
        };
        if !self.is_bare() {
            let path = self.ws.root.join(".mailmap");
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file())
                && let Ok(bytes) = std::fs::read(&path)
            {
                map.parse(&bytes);
            }
        }
        if let Some(spec) = blob {
            let resolver = Resolver {
                git: self,
                prefix: &[],
            };
            if let Ok(Some(id)) = resolver.object(spec)
                && let Ok(bytes) = self.repo.blob(&id)
            {
                map.parse(&bytes);
            }
        }
        if let Some(file) = self.config().get("mailmap.file") {
            let path = match file.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(rest)),
                None if file.starts_with('~') => {
                    bail!("mailmap.file with a `~user` path is not supported")
                }
                None => Some(self.ws.root.join(file)),
            };
            if let Some(path) = path
                && let Ok(bytes) = std::fs::read(path)
            {
                map.parse(&bytes);
            }
        }
        Ok(map)
    }
}

/// Splits `Name <user@host>` into the name and the email, as git's identity parser does.
fn split_contact(contact: &str) -> Option<(&str, &str)> {
    let left = contact.find('<')?;
    let right = left + contact[left..].find('>')?;
    Some((contact[..left].trim_end(), &contact[left + 1..right]))
}

pub(super) fn check_mailmap(git: &GitHome, args: CheckMailmapArgs) -> Result<()> {
    if args.contacts.is_empty() && !args.stdin {
        bail!("no contacts specified");
    }
    let map = git.mailmap()?;
    let mut out = Vec::new();
    let done = contacts(&map, &args, &mut out);
    emit_bytes(&out);
    done
}

fn contacts(map: &Mailmap, args: &CheckMailmapArgs, out: &mut Vec<u8>) -> Result<()> {
    let mut one = |contact: &str| -> Result<()> {
        let Some((name, email)) = split_contact(contact) else {
            bail!("unable to parse contact: {contact}");
        };
        let (name, email) = map.map(name.as_bytes(), email.as_bytes());
        if !name.is_empty() {
            out.extend_from_slice(&name);
            out.push(b' ');
        }
        out.extend_from_slice(format!("<{}>\n", String::from_utf8_lossy(&email)).as_bytes());
        Ok(())
    };
    for contact in &args.contacts {
        one(contact)?;
    }
    if args.stdin {
        for line in std::io::stdin().lock().lines() {
            one(&line?)?;
        }
    }
    Ok(())
}

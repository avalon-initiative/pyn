//! Commands run on the server host against the configured database: no HTTP route, so they need host control.

use std::io::BufRead;
use std::sync::Arc;

use pyn_core::{
    AccessService, AccessStore, AdminRecovery, SecretKey, SystemClock, account, rotate_secret_key,
};
use pyn_postgres::PgMetadataStore;

use crate::events::WebhookLimitEvents;

pub const USAGE: &str = "usage:\n  pyn-server recover-admin <username> [--password-stdin]\n  pyn-server rotate-secret-key";

#[derive(Debug, PartialEq, Eq)]
pub enum HostCommand {
    RecoverAdmin {
        username: String,
        password_stdin: bool,
    },
    RotateSecretKey,
}

/// `None` when `args` (after the program name) name no host command, so the server starts normally.
pub fn parse(args: &[String]) -> anyhow::Result<Option<HostCommand>> {
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("recover-admin") => {
            let mut username = None;
            let mut password_stdin = false;
            for arg in rest {
                match arg.as_str() {
                    "--password-stdin" => password_stdin = true,
                    flag if flag.starts_with('-') => {
                        anyhow::bail!("unknown option {flag}\n{USAGE}")
                    }
                    name if username.is_none() => username = Some(name.to_string()),
                    _ => anyhow::bail!("recover-admin takes one user name\n{USAGE}"),
                }
            }
            let username = username
                .ok_or_else(|| anyhow::anyhow!("recover-admin needs a user name\n{USAGE}"))?;
            Ok(Some(HostCommand::RecoverAdmin {
                username,
                password_stdin,
            }))
        }
        Some("rotate-secret-key") => {
            anyhow::ensure!(
                rest.is_empty(),
                "rotate-secret-key takes no arguments\n{USAGE}"
            );
            Ok(Some(HostCommand::RotateSecretKey))
        }
        _ => Ok(None),
    }
}

/// The first line of `input` without its line ending.
pub fn read_password(mut input: impl BufRead) -> anyhow::Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    let password = line.trim_end_matches(['\n', '\r']);
    anyhow::ensure!(!password.is_empty(), "no password on standard input");
    Ok(password.to_string())
}

async fn connect() -> anyhow::Result<Arc<PgMetadataStore>> {
    let url = std::env::var("PYN_DATABASE_URL").map_err(|_| {
        anyhow::anyhow!(
            "PYN_DATABASE_URL is not set: this command works on the database, and the in-memory store keeps nothing between runs"
        )
    })?;
    Ok(Arc::new(PgMetadataStore::connect(&url, false).await?))
}

/// Recovers `username` as an administrator; returns what happened and the password when one was generated.
pub async fn recover_admin(
    store: Arc<PgMetadataStore>,
    public_url: String,
    username: &str,
    password: Option<String>,
) -> anyhow::Result<(AdminRecovery, Option<String>)> {
    anyhow::ensure!(
        store.server_settings().await?.is_some(),
        "this server is not set up yet; finish first-run setup with the setup token instead"
    );
    let service = AccessService::new(
        store.clone(),
        Arc::new(SystemClock),
        Arc::new(WebhookLimitEvents::new(store.clone(), public_url)),
    )
    .with_audit(store);
    let generated = match password {
        Some(_) => None,
        None => Some(account::generate_password()?),
    };
    let password = password.or_else(|| generated.clone()).unwrap_or_default();
    let done = service.recover_admin(username, &password).await?;
    Ok((done, generated))
}

fn key_from_env(name: &str) -> anyhow::Result<Option<SecretKey>> {
    match std::env::var(name) {
        Ok(text) => Ok(Some(
            SecretKey::from_hex(&text).map_err(|e| anyhow::anyhow!("{name}: {e}"))?,
        )),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => anyhow::bail!("{name} is not valid text"),
    }
}

pub async fn run(command: HostCommand) -> anyhow::Result<()> {
    match command {
        HostCommand::RecoverAdmin {
            username,
            password_stdin,
        } => {
            let password = password_stdin
                .then(|| read_password(std::io::stdin().lock()))
                .transpose()?;
            let public_url = std::env::var("PYN_PUBLIC_URL").unwrap_or_default();
            let (done, generated) =
                recover_admin(connect().await?, public_url, &username, password).await?;
            tracing::warn!(
                user = %done.user,
                created = done.created,
                reenabled = done.reenabled,
                tokens_revoked = done.tokens_revoked,
                "host recovery: administrator recovered"
            );
            if let Some(password) = generated {
                println!("{password}");
                eprintln!(
                    "{} is an administrator; the password above is shown once, change it after signing in",
                    done.user
                );
            } else {
                eprintln!(
                    "{} is an administrator with the password you gave",
                    done.user
                );
            }
        }
        HostCommand::RotateSecretKey => {
            let current = key_from_env("PYN_SECRET_KEY")?;
            let new = key_from_env("PYN_NEW_SECRET_KEY")?
                .ok_or_else(|| anyhow::anyhow!("PYN_NEW_SECRET_KEY is not set"))?;
            let rewritten =
                rotate_secret_key(connect().await?.as_ref(), current.as_ref(), &new).await?;
            eprintln!(
                "re-encrypted {rewritten} webhook secret(s) under key {}; set PYN_SECRET_KEY to the new key and restart the server",
                new.key_id()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_command_means_the_server_starts() {
        assert_eq!(parse(&[]).unwrap(), None);
        assert_eq!(parse(&args(&["--healthcheck"])).unwrap(), None);
    }

    #[test]
    fn recover_admin_takes_one_name_and_an_optional_flag() {
        assert_eq!(
            parse(&args(&["recover-admin", "alice"])).unwrap(),
            Some(HostCommand::RecoverAdmin {
                username: "alice".into(),
                password_stdin: false
            })
        );
        assert_eq!(
            parse(&args(&["recover-admin", "--password-stdin", "alice"])).unwrap(),
            Some(HostCommand::RecoverAdmin {
                username: "alice".into(),
                password_stdin: true
            })
        );
        for bad in [
            &["recover-admin"][..],
            &["recover-admin", "a", "b"],
            &["recover-admin", "a", "--password"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rotate_secret_key_takes_no_arguments() {
        assert_eq!(
            parse(&args(&["rotate-secret-key"])).unwrap(),
            Some(HostCommand::RotateSecretKey)
        );
        assert!(parse(&args(&["rotate-secret-key", "x"])).is_err());
    }

    #[test]
    fn a_password_is_the_first_line_without_its_ending() {
        assert_eq!(
            read_password("s3cret pass\r\nmore\n".as_bytes()).unwrap(),
            "s3cret pass"
        );
        assert_eq!(
            read_password("no newline".as_bytes()).unwrap(),
            "no newline"
        );
        assert!(read_password("".as_bytes()).is_err());
        assert!(read_password("\n".as_bytes()).is_err());
    }
}

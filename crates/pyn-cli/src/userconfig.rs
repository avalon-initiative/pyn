//! The user-level configuration file and the first-run prompt that creates it.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::credentials;
use crate::workspace::Settings;

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:7878";

/// `$PYN_CLI_CONFIG`, else `config.toml` in the pyn configuration directory.
pub fn path() -> Result<PathBuf> {
    match std::env::var_os("PYN_CLI_CONFIG") {
        Some(p) if !p.is_empty() => Ok(p.into()),
        _ => Ok(credentials::dir()?.join("config.toml")),
    }
}

/// Asks for each setting, offering the current value or the recommended default; blank accepts it.
pub fn prompt(
    current: &Settings,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Settings> {
    let mut settings = current.clone();
    let server = current.server.as_deref().unwrap_or(DEFAULT_SERVER);
    settings.server = Some(ask(input, out, "Server URL", Some(server))?.expect("has a default"));
    settings.user = ask(
        input,
        out,
        "Dev identity (X-Pyn-User, blank for none)",
        current.user.as_deref(),
    )?;
    Ok(settings)
}

fn ask(
    input: &mut impl BufRead,
    out: &mut impl Write,
    label: &str,
    default: Option<&str>,
) -> Result<Option<String>> {
    write!(out, "{label} [{}]: ", default.unwrap_or(""))?;
    out.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        bail!("input ended before {label} was answered");
    }
    let answer = line.trim();
    Ok(if answer.is_empty() {
        default.map(String::from)
    } else {
        Some(answer.to_string())
    })
}

/// Creates the file through the prompt on first run, or explains how to set the missing server.
pub fn first_run(
    file: &Path,
    interactive: bool,
    input: &mut impl BufRead,
    out: &mut impl Write,
) -> Result<Settings> {
    if !interactive {
        bail!(
            "no server is set and there is no configuration file at {}; pass --server, set PYN_SERVER, \
             run `pyn config set --global server <url>`, or run `pyn config init` in a terminal",
            file.display()
        );
    }
    writeln!(
        out,
        "No pyn configuration found; creating {}.",
        file.display()
    )?;
    let settings = prompt(&Settings::default(), input, out)?;
    settings.save(file).context("saving the configuration")?;
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_answers_take_the_defaults() {
        let mut out = Vec::new();
        let s = prompt(&Settings::default(), &mut "\n\n".as_bytes(), &mut out).unwrap();
        assert_eq!(s.server.as_deref(), Some(DEFAULT_SERVER));
        assert_eq!(s.user, None);
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.contains(&format!("[{DEFAULT_SERVER}]")), "{shown}");
    }

    #[test]
    fn answers_override_and_current_values_are_the_defaults() {
        let mut out = Vec::new();
        let s = prompt(
            &Settings::default(),
            &mut "http://h:1\nalice\n".as_bytes(),
            &mut out,
        )
        .unwrap();
        assert_eq!(s.server.as_deref(), Some("http://h:1"));
        assert_eq!(s.user.as_deref(), Some("alice"));
        let again = prompt(&s, &mut "\n\n".as_bytes(), &mut Vec::new()).unwrap();
        assert_eq!(again, s);
    }

    #[test]
    fn first_run_writes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/config.toml");
        let s = first_run(&file, true, &mut "\n\n".as_bytes(), &mut Vec::new()).unwrap();
        assert_eq!(Settings::load(&file).unwrap(), s);
    }

    #[test]
    fn non_interactive_names_the_setting_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        let err = first_run(&file, false, &mut "".as_bytes(), &mut Vec::new()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no server is set") && msg.contains("pyn config set"),
            "{msg}"
        );
        assert!(!file.exists());
    }

    #[test]
    fn closed_input_is_an_error_not_a_hang() {
        let err = prompt(&Settings::default(), &mut "".as_bytes(), &mut Vec::new());
        assert!(err.is_err());
    }
}

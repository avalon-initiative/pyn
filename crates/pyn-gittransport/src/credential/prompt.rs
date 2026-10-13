//! Asking a person or an askpass program for a username or password.

use std::io::{BufRead, IsTerminal, Write};

use pyn_git::Config;

use crate::env::Env;
use crate::error::{Error, Result};

pub trait Prompt: Send {
    /// The answer to `prompt`; `secret` answers are not echoed.
    fn ask(&mut self, prompt: &str, secret: bool) -> Result<String>;
}

/// An askpass program (`GIT_ASKPASS`, `core.askPass`, `SSH_ASKPASS`, in that order), else the terminal.
pub struct SystemPrompt {
    env: Env,
    askpass: Option<String>,
}

pub fn askpass_program(env: &Env, config: &Config) -> Option<String> {
    env.non_empty("GIT_ASKPASS")
        .or_else(|| config.get("core.askpass").filter(|v| !v.is_empty()))
        .or_else(|| env.non_empty("SSH_ASKPASS"))
        .map(str::to_owned)
}

impl SystemPrompt {
    pub fn new(env: &Env, config: &Config) -> Self {
        Self {
            askpass: askpass_program(env, config),
            env: env.clone(),
        }
    }

    fn terminal_allowed(&self) -> bool {
        let off = self
            .env
            .get("GIT_TERMINAL_PROMPT")
            .is_some_and(crate::url::is_false);
        !off && std::io::stdin().is_terminal()
    }
}

impl Prompt for SystemPrompt {
    fn ask(&mut self, prompt: &str, secret: bool) -> Result<String> {
        if let Some(program) = &self.askpass {
            return run_askpass(&self.env, program, prompt);
        }
        if !self.terminal_allowed() {
            return Err(Error::Credential(format!(
                "could not read {}: terminal prompts disabled (set GIT_ASKPASS, or put credentials in the URL or a credential helper)",
                prompt.trim_end_matches([':', ' '])
            )));
        }
        if secret {
            return Ok(rpassword::prompt_password(prompt)?);
        }
        eprint!("{prompt}");
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(line.trim_end_matches(['\n', '\r']).to_owned())
    }
}

fn run_askpass(env: &Env, program: &str, prompt: &str) -> Result<String> {
    let out = env
        .command(program)
        .arg(prompt)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| Error::Credential(format!("cannot run askpass program '{program}': {e}")))?;
    if !out.status.success() {
        return Err(Error::Credential(format!(
            "askpass program '{program}' failed ({})",
            out.status
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.trim_end_matches(['\n', '\r']).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn askpass_order_is_env_then_config_then_ssh_askpass() {
        let config = Config::parse(b"[core]\n\taskPass = from-config\n").unwrap();
        let all = Env::default()
            .with("GIT_ASKPASS", "from-env")
            .with("SSH_ASKPASS", "from-ssh");
        assert_eq!(askpass_program(&all, &config).as_deref(), Some("from-env"));
        let no_env = Env::default().with("SSH_ASKPASS", "from-ssh");
        assert_eq!(
            askpass_program(&no_env, &config).as_deref(),
            Some("from-config")
        );
        assert_eq!(
            askpass_program(&no_env, &Config::default()).as_deref(),
            Some("from-ssh")
        );
        let empty = Env::default().with("GIT_ASKPASS", "");
        assert_eq!(askpass_program(&empty, &Config::default()), None);
    }

    #[test]
    fn terminal_prompt_off_fails_with_a_clear_message() {
        let env = Env::default().with("GIT_TERMINAL_PROMPT", "0");
        let mut p = SystemPrompt::new(&env, &Config::default());
        let e = p.ask("Password for 'https://host': ", true).unwrap_err();
        assert!(e.to_string().contains("terminal prompts disabled"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn askpass_program_answers_and_is_trimmed() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("ask");
        std::fs::write(
            &script,
            "#!/bin/sh\ncase \"$1\" in Username*) echo alice;; *) echo \"pw-for:$1\";; esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = Env::default().with("GIT_ASKPASS", script.to_str().unwrap());
        let mut p = SystemPrompt::new(&env, &Config::default());
        assert_eq!(p.ask("Username for 'https://h': ", false).unwrap(), "alice");
        assert_eq!(
            p.ask("Password for 'https://alice@h': ", true).unwrap(),
            "pw-for:Password for 'https://alice@h': "
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_askpass_is_an_error() {
        let env = Env::default().with("GIT_ASKPASS", "false");
        let mut p = SystemPrompt::new(
            &env.clone().with("PATH", "/usr/bin:/bin"),
            &Config::default(),
        );
        assert!(p.ask("Username: ", false).is_err());
    }
}

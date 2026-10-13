//! From a remote URL string to a [`Transport`].

use std::path::Path;

use pyn_git::Config;
use pyn_gitwire::Transport;

use crate::credential::{Prompt, SystemPrompt};
use crate::env::Env;
use crate::error::Result;
use crate::http::HttpTransport;
use crate::ssh::SshTransport;
use crate::url::{RemoteUrl, rewrite};

pub struct Connection {
    pub transport: Box<dyn Transport>,
    /// The URL after `insteadOf`, without credentials.
    pub display: String,
}

pub struct Connector {
    config: Config,
    env: Env,
    prompt: Option<Box<dyn Prompt>>,
}

impl Connector {
    pub fn new(config: Config, env: Env) -> Self {
        Self {
            config,
            env,
            prompt: None,
        }
    }

    /// Answers credential questions with `prompt` instead of askpass and the terminal.
    pub fn with_prompt(mut self, prompt: Box<dyn Prompt>) -> Self {
        self.prompt = Some(prompt);
        self
    }

    /// Applies `insteadOf` (`pushInsteadOf` when `push`), then picks the transport the URL names.
    pub fn connect(self, url: &str, push: bool) -> Result<Connection> {
        let resolved = rewrite(&self.config, url, push);
        let parsed = RemoteUrl::parse(&resolved)?;
        let display = parsed.display();
        let transport: Box<dyn Transport> = match parsed {
            RemoteUrl::Ssh(u) => Box::new(SshTransport::new(u, &self.config, &self.env)),
            RemoteUrl::Http(u) => {
                let prompt = self
                    .prompt
                    .unwrap_or_else(|| Box::new(SystemPrompt::new(&self.env, &self.config)));
                Box::new(HttpTransport::new(u, &self.config, &self.env, prompt)?)
            }
        };
        Ok(Connection { transport, display })
    }
}

/// The user's git configuration outside any repository: `/etc/gitconfig`, then `$XDG_CONFIG_HOME/git/config`
/// and `~/.gitconfig` (or the file `GIT_CONFIG_GLOBAL` names).
pub fn user_config(env: &Env) -> Result<Config> {
    let mut config = Config::default();
    let system = Path::new("/etc/gitconfig");
    if env.get("GIT_CONFIG_NOSYSTEM").is_none() && system.is_file() {
        config.extend(Config::read(system)?);
    }
    if let Some(file) = env.non_empty("GIT_CONFIG_GLOBAL") {
        if Path::new(file).is_file() {
            config.extend(Config::read(Path::new(file))?);
        }
    } else if let Some(home) = env.non_empty("HOME") {
        let xdg = env.non_empty("XDG_CONFIG_HOME").map(Path::new);
        config.extend(Config::global(Path::new(home), xdg)?);
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect(config: &str, url: &str, push: bool) -> crate::Result<Connection> {
        Connector::new(Config::parse(config.as_bytes()).unwrap(), Env::default()).connect(url, push)
    }

    #[test]
    fn instead_of_applies_at_connect_time_and_the_display_is_clean() {
        let config = "[url \"https://tok@git.example/\"]\n\tinsteadOf = ex:\n";
        let c = connect(config, "ex:team/r.git", false).unwrap();
        assert_eq!(c.display, "https://git.example/team/r.git");
        let c = connect(
            "[url \"git@host:\"]\n\tpushInsteadOf = https://host/\n",
            "https://host/r.git",
            true,
        )
        .unwrap();
        assert_eq!(c.display, "git@host:r.git");
    }

    #[test]
    fn local_paths_are_refused_after_rewriting() {
        let e = connect("", "/srv/repo", false).err().unwrap();
        assert!(
            e.to_string()
                .contains("local-path remotes are not supported yet")
        );
        let rewritten = connect("[url \"/srv/\"]\n\tinsteadOf = srv:\n", "srv:r", false);
        assert!(rewritten.is_err());
    }

    #[test]
    fn user_config_reads_global_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".gitconfig"), "[user]\n\tname = n\n").unwrap();
        let env = Env::default()
            .with("HOME", tmp.path().to_str().unwrap())
            .with("GIT_CONFIG_NOSYSTEM", "1");
        assert_eq!(user_config(&env).unwrap().get("user.name"), Some("n"));
        let env = env.with("GIT_CONFIG_GLOBAL", "/nonexistent");
        assert_eq!(user_config(&env).unwrap().get("user.name"), None);
    }
}

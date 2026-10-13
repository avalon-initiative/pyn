#![allow(dead_code)]

pub mod gitsrv;

use std::path::Path;

use pyn_git::Config;
use pyn_gittransport::credential::Prompt;
use pyn_gittransport::{Connector, Env, Result};

pub use gitsrv::*;

/// A prompt that answers from a list and records what was asked; running out of answers fails.
pub struct Scripted {
    pub answers: Vec<String>,
    pub asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Scripted {
    pub fn new(answers: &[&str]) -> (Self, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        (
            Self {
                answers: answers.iter().map(|s| (*s).to_owned()).collect(),
                asked: asked.clone(),
            },
            asked,
        )
    }
}

impl Prompt for Scripted {
    fn ask(&mut self, prompt: &str, _secret: bool) -> Result<String> {
        self.asked.lock().unwrap().push(prompt.to_owned());
        if self.answers.is_empty() {
            return Err(pyn_gittransport::Error::Credential(
                "no answers left".into(),
            ));
        }
        Ok(self.answers.remove(0))
    }
}

/// An environment with a private home and no system or global git configuration.
pub fn env(home: &Path) -> Env {
    std::fs::create_dir_all(home).unwrap();
    Env::default()
        .with("PATH", &path_env())
        .with("HOME", home.to_str().unwrap())
        .with("GIT_CONFIG_NOSYSTEM", "1")
        .with("GIT_TERMINAL_PROMPT", "0")
}

/// `config` on top of settings that keep a test away from any proxy in the environment.
pub fn connector(config: &str, env: Env) -> Connector {
    let text = format!("[http]\n\tproxy =\n{config}");
    Connector::new(Config::parse(text.as_bytes()).unwrap(), env)
}

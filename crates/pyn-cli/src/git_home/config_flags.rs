//! `pyn config` in git's flag forms: `--list`, `--get`, `--get-all`, `--add`, `--unset`, `--unset-all`, `<key>` and
//! `<key> <value>`, on the system, user and repository git config files.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::Args;
use pyn_git::{Config, ConfigFile};

use super::plumbing::emit_bytes;

#[derive(Debug, Args, Default)]
pub struct ConfigFlags {
    /// List every setting as `key=value`.
    #[arg(short = 'l', long)]
    list: bool,
    /// Print the last value of a key.
    #[arg(long)]
    get: bool,
    /// Print every value of a key.
    #[arg(long)]
    get_all: bool,
    /// Add a value without replacing the ones the key has.
    #[arg(long)]
    add: bool,
    /// Remove a key that has one value.
    #[arg(long)]
    unset: bool,
    /// Remove every value of a key.
    #[arg(long)]
    unset_all: bool,
    /// Use the user's git config file.
    #[arg(long)]
    global: bool,
    /// Use the system git config file.
    #[arg(long)]
    system: bool,
    /// Use the repository's git config file.
    #[arg(long)]
    local: bool,
    /// Use this config file.
    #[arg(short = 'f', long, value_name = "FILE")]
    file: Option<PathBuf>,
    /// `<key>` to read it, `<key> <value>` to set it.
    #[arg(value_name = "KEY_VALUE")]
    words: Vec<String>,
}

impl ConfigFlags {
    pub fn is_empty(&self) -> bool {
        !(self.list
            || self.get
            || self.get_all
            || self.add
            || self.unset
            || self.unset_all
            || self.global
            || self.system
            || self.local
            || self.file.is_some()
            || !self.words.is_empty())
    }
}

enum Action {
    List,
    Get,
    GetAll,
    Add,
    Set,
    Unset,
    UnsetAll,
}

struct Key {
    section: String,
    sub: Option<String>,
    name: String,
}

fn usage(message: &str) -> ! {
    eprintln!("error: {message}");
    eprintln!("usage: git config [<options>]");
    std::process::exit(129);
}

fn exit_error(message: &str, code: i32) -> ! {
    eprintln!("error: {message}");
    std::process::exit(code);
}

fn parse_key(text: &str) -> Key {
    let Some((head, name)) = text.rsplit_once('.') else {
        exit_error(&format!("key does not contain a section: {text}"), 1);
    };
    if name.is_empty() {
        exit_error(&format!("key does not contain variable name: {text}"), 1);
    }
    let (section, sub) = match head.split_once('.') {
        Some((s, sub)) => (s, Some(sub.to_string())),
        None => (head, None),
    };
    let word = |s: &str| s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let ok = !section.is_empty()
        && word(section)
        && word(name)
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && sub.as_deref().is_none_or(|s| !s.contains('\n'));
    if !ok {
        exit_error(&format!("invalid key: {text}"), 1);
    }
    Key {
        section: section.to_string(),
        sub,
        name: name.to_string(),
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn system_path() -> PathBuf {
    env_path("GIT_CONFIG_SYSTEM").unwrap_or_else(|| PathBuf::from("/etc/gitconfig"))
}

/// Implicit reads honor GIT_CONFIG_NOSYSTEM; an explicit `--system` does not, as in git.
fn system_file() -> Option<PathBuf> {
    let off = std::env::var("GIT_CONFIG_NOSYSTEM")
        .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"));
    (!off).then(system_path)
}

/// The user-level files git reads, in precedence order, lowest first.
fn global_files() -> Vec<PathBuf> {
    if let Some(p) = env_path("GIT_CONFIG_GLOBAL") {
        return vec![p];
    }
    let home = env_path("HOME");
    let xdg = env_path("XDG_CONFIG_HOME")
        .or_else(|| home.as_ref().map(|h| h.join(".config")))
        .map(|x| x.join("git/config"));
    let mut files = Vec::new();
    files.extend(xdg);
    files.extend(home.map(|h| h.join(".gitconfig")));
    files
}

/// Where a write to the user's file goes: the first that exists, else `~/.gitconfig`.
pub(super) fn global_target() -> PathBuf {
    if let Some(p) = env_path("GIT_CONFIG_GLOBAL") {
        return p;
    }
    let files = global_files();
    let Some(home) = files.last() else {
        eprintln!("fatal: $HOME not set");
        std::process::exit(128);
    };
    if home.is_file() {
        return home.clone();
    }
    files.iter().find(|p| p.is_file()).unwrap_or(home).clone()
}

fn local_path(cwd: &Path) -> Result<Option<PathBuf>> {
    Ok(super::detect(cwd)?.map(|g| g.local_config_path()))
}

pub fn run(cwd: &Path, f: &ConfigFlags) -> Result<()> {
    let actions = [f.list, f.get, f.get_all, f.add, f.unset, f.unset_all];
    if actions.iter().filter(|a| **a).count() > 1 {
        usage("only one action at a time");
    }
    let scopes = [f.global, f.system, f.local, f.file.is_some()];
    if scopes.iter().filter(|a| **a).count() > 1 {
        usage("only one config file at a time");
    }
    let action = match (f.list, f.get, f.get_all, f.add, f.unset, f.unset_all) {
        (true, ..) => Action::List,
        (_, true, ..) => Action::Get,
        (_, _, true, ..) => Action::GetAll,
        (_, _, _, true, ..) => Action::Add,
        (_, _, _, _, true, _) => Action::Unset,
        (_, _, _, _, _, true) => Action::UnsetAll,
        _ => match f.words.len() {
            1 => Action::Get,
            2 => Action::Set,
            _ => usage("wrong number of arguments, should be from 1 to 2"),
        },
    };
    let want = match action {
        Action::List => 0..=0,
        Action::Add | Action::Set => 2..=2,
        _ => 1..=1,
    };
    if !want.contains(&f.words.len()) {
        usage("wrong number of arguments");
    }
    let local = local_path(cwd)?;
    if f.local && local.is_none() {
        bail!("--local can only be used inside a git repository");
    }
    let explicit = if f.global {
        Some(global_target())
    } else if f.system {
        Some(system_path())
    } else if f.local {
        local.clone()
    } else {
        f.file.as_ref().map(|p| cwd.join(p))
    };
    match action {
        Action::List | Action::Get | Action::GetAll => read(f, &action, explicit, local),
        _ => {
            let target = match explicit {
                Some(p) => p,
                None => match local {
                    Some(p) => p,
                    None => bail!("not in a git directory"),
                },
            };
            write(&action, &f.words, &target)
        }
    }
}

fn read(
    f: &ConfigFlags,
    action: &Action,
    explicit: Option<PathBuf>,
    local: Option<PathBuf>,
) -> Result<()> {
    let mut config = Config::default();
    match explicit {
        Some(path) => {
            if path.is_file() {
                config.extend(Config::read(&path)?);
            } else if matches!(action, Action::List) {
                bail!(
                    "unable to read config file '{}': No such file or directory",
                    path.display()
                );
            }
        }
        None => {
            let sources = system_file().into_iter().chain(global_files()).chain(local);
            for path in sources.filter(|p| p.is_file()) {
                config.extend(Config::read(&path)?);
            }
        }
    }
    let mut out = String::new();
    match action {
        Action::List => {
            for e in config.entries() {
                out.push_str(&e.section);
                if let Some(sub) = &e.subsection {
                    out.push('.');
                    out.push_str(sub);
                }
                out.push('.');
                out.push_str(&e.key);
                if let Some(v) = &e.value {
                    out.push('=');
                    out.push_str(v);
                }
                out.push('\n');
            }
        }
        _ => {
            let key = &f.words[0];
            parse_key(key);
            let values = config.get_all(key);
            let shown: &[&str] = match (action, values.last()) {
                (Action::Get, Some(last)) => &[*last],
                (Action::GetAll, _) => &values,
                _ => &[],
            };
            if values.is_empty() {
                emit_bytes(b"");
                std::process::exit(1);
            }
            for v in shown {
                out.push_str(v);
                out.push('\n');
            }
        }
    }
    emit_bytes(out.as_bytes());
    Ok(())
}

fn write(action: &Action, words: &[String], target: &Path) -> Result<()> {
    let key = parse_key(&words[0]);
    let mut file = ConfigFile::read(target)?;
    let (section, sub, name) = (key.section.as_str(), key.sub.as_deref(), key.name.as_str());
    match action {
        Action::Add => file.add(section, sub, name, &words[1])?,
        Action::Set => match file.set(section, sub, name, &words[1]) {
            Err(pyn_git::Error::MultipleValues(k)) => {
                eprintln!("warning: {k} has multiple values");
                exit_error(
                    &format!(
                        "cannot overwrite multiple values with a single value\n       Use a regexp, --add or --replace-all to change {}.",
                        words[0]
                    ),
                    5,
                );
            }
            other => other?,
        },
        Action::Unset | Action::UnsetAll => {
            let have = file.values(section, sub, name).len();
            if have == 0 {
                std::process::exit(5);
            }
            if have > 1 && matches!(action, Action::Unset) {
                eprintln!("warning: {} has multiple values", words[0]);
                std::process::exit(5);
            }
            file.unset_all(section, sub, name);
        }
        _ => unreachable!("read actions do not write"),
    }
    if let Err(e) = file.write(target) {
        exit_error(
            &format!("could not lock config file {}: {e}", target.display()),
            255,
        );
    }
    Ok(())
}

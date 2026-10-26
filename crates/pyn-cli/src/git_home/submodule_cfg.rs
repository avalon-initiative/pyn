//! `.gitmodules` and the `submodule.<name>.*` settings in `.git/config`, and how a submodule's url is resolved.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use pyn_git::{ConfigFile, tracking};
use pyn_gitwork::Head;

use super::GitHome;

/// One `[submodule "<name>"]` section of `.gitmodules`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Module {
    pub name: String,
    pub path: String,
    pub url: Option<String>,
    pub branch: Option<String>,
    pub update: Option<String>,
    pub shallow: Option<bool>,
    pub ignore: Option<String>,
    pub fetch_recurse: Option<String>,
}

pub(super) fn gitmodules_path(git: &GitHome) -> Result<PathBuf> {
    Ok(git.repo.root()?.join(".gitmodules"))
}

pub(super) fn read_file(path: &Path) -> Result<ConfigFile> {
    if path.exists() {
        Ok(ConfigFile::read(path)?)
    } else {
        Ok(ConfigFile::default())
    }
}

/// The submodules `.gitmodules` in the work tree declares, in file order.
pub(super) fn read(git: &GitHome) -> Result<Vec<Module>> {
    let file = read_file(&gitmodules_path(git)?)?;
    let one = |name: &str, key: &str| {
        file.values("submodule", Some(name), key)
            .into_iter()
            .next_back()
    };
    Ok(file
        .subsections("submodule")
        .into_iter()
        .filter_map(|name| {
            Some(Module {
                path: one(&name, "path")?,
                url: one(&name, "url"),
                branch: one(&name, "branch"),
                update: one(&name, "update"),
                shallow: one(&name, "shallow").map(|v| matches!(v.as_str(), "true" | "yes" | "on")),
                ignore: one(&name, "ignore"),
                fetch_recurse: one(&name, "fetchRecurseSubmodules"),
                name,
            })
        })
        .collect())
}

pub(super) fn local_path(git: &GitHome) -> PathBuf {
    git.repo.git().common_dir().join("config")
}

pub(super) fn local(git: &GitHome) -> Result<ConfigFile> {
    read_file(&local_path(git))
}

/// The last value of `submodule.<name>.<key>` in `.git/config`.
pub(super) fn local_value(git: &GitHome, name: &str, key: &str) -> Result<Option<String>> {
    Ok(local(git)?
        .values("submodule", Some(name), key)
        .into_iter()
        .next_back())
}

/// The remote a relative url is resolved against: the current branch's, else `origin`.
fn superproject_remote_url(git: &GitHome) -> Option<String> {
    let config = git.config();
    let remote = match git.repo.head() {
        Ok(Head::Branch { name, .. }) => tracking(config, &name).remote,
        _ => None,
    }
    .unwrap_or_else(|| "origin".to_owned());
    config
        .get(&format!("remote.{remote}.url"))
        .map(str::to_owned)
}

pub(super) fn is_relative(url: &str) -> bool {
    url.starts_with("./") || url.starts_with("../")
}

/// Removes the last path component of `base`; the flag says a scp-like `host:path` colon was the separator.
fn chop_last_dir(base: &mut String, relative: bool) -> Result<bool> {
    if let Some(i) = base.rfind('/') {
        base.truncate(i);
        return Ok(false);
    }
    if let Some(i) = base.rfind(':') {
        base.truncate(i);
        return Ok(true);
    }
    if relative || base == "." {
        bail!("cannot strip one component off url '{base}'");
    }
    *base = ".".into();
    Ok(false)
}

/// A url given relative to the superproject's remote, made absolute; other urls are returned unchanged.
pub(super) fn resolve_url(git: &GitHome, url: &str, warn: bool) -> Result<String> {
    if !is_relative(url) {
        return Ok(url.to_owned());
    }
    let remote = match superproject_remote_url(git) {
        Some(r) => r,
        None => {
            if warn {
                eprintln!(
                    "warning: could not look up configuration 'remote.origin.url'. Assuming this repository is its own authoritative upstream."
                );
            }
            git.repo.root()?.display().to_string()
        }
    };
    join_relative(&remote, url)
}

fn join_relative(remote: &str, url: &str) -> Result<String> {
    let mut base = remote.trim_end_matches('/').to_owned();
    let relative = is_relative(&base);
    let mut rest = url;
    let mut colon = false;
    loop {
        if let Some(r) = rest.strip_prefix("./") {
            rest = r;
        } else if let Some(r) = rest.strip_prefix("../") {
            rest = r;
            colon = chop_last_dir(&mut base, relative)?;
        } else {
            break;
        }
    }
    Ok(format!("{base}{}{rest}", if colon { ":" } else { "/" }))
}

/// Writes `path`, `url` and the optional `branch` of a new submodule, replacing a section of the same name.
pub(super) fn write_module(git: &GitHome, module: &Module) -> Result<()> {
    let file_path = gitmodules_path(git)?;
    let mut file = read_file(&file_path)?;
    file.remove_section("submodule", Some(&module.name));
    let name = Some(module.name.as_str());
    file.add("submodule", name, "path", &module.path)?;
    if let Some(url) = &module.url {
        file.add("submodule", name, "url", url)?;
    }
    if let Some(branch) = &module.branch {
        file.add("submodule", name, "branch", branch)?;
    }
    Ok(file.write(&file_path)?)
}

/// Sets or, with `None`, removes one key of a module in `.gitmodules`.
pub(super) fn set_key(git: &GitHome, name: &str, key: &str, value: Option<&str>) -> Result<()> {
    let file_path = gitmodules_path(git)?;
    let mut file = read_file(&file_path)?;
    match value {
        Some(v) => file.set("submodule", Some(name), key, v)?,
        None => {
            file.unset_all("submodule", Some(name), key);
        }
    }
    Ok(file.write(&file_path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_urls_climb_the_remote() {
        let j = |r, u| join_relative(r, u).unwrap();
        assert_eq!(
            j("https://h/org/sup.git", "../lib.git"),
            "https://h/org/lib.git"
        );
        assert_eq!(j("https://h/org/sup/", "./lib"), "https://h/org/sup/lib");
        assert_eq!(j("/a/b/sup", "../../x/y"), "/a/x/y");
        assert_eq!(j("host:org/sup", "../lib"), "host:org/lib");
        assert_eq!(j("host:sup", "../lib"), "host:lib");
    }
}

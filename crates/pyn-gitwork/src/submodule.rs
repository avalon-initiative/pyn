//! Read-only gitlink inspection and the superproject's submodule ignore settings.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;

use pyn_git::Config;
use pyn_object::ObjectId;

use crate::error::{Error, Result, io_at};
use crate::{Kind, Repo, fs_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmoduleIgnore {
    None,
    Untracked,
    Dirty,
    All,
}

/// The visible changes within an initialized submodule, after ignore settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmoduleStatus {
    /// HEAD, or the recorded gitlink ID when HEAD is unborn.
    pub head: ObjectId,
    pub new_commits: bool,
    pub modified: bool,
    pub untracked: bool,
}

impl SubmoduleStatus {
    pub fn changed(&self) -> bool {
        self.new_commits || self.modified || self.untracked
    }
}

#[cfg(feature = "test-support")]
fn probe(repo: &Repo, event: &str) {
    if std::env::var_os("PYN_DEBUG_SUBMODULE_CONFIG").is_some() {
        eprintln!(
            "submodule config {event}: {}",
            repo.git().git_dir().display()
        );
    }
}

#[cfg(not(feature = "test-support"))]
fn probe(_: &Repo, _: &str) {}

impl Repo {
    fn gitmodules(&self) -> Result<Config> {
        let file = self.root()?.join(".gitmodules");
        match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_file() => {
                probe(self, "parse");
                return Ok(Config::read(&file)?);
            }
            Ok(_) => return Ok(Config::default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_at(&file)(e)),
        }
        probe(self, "index");
        let index = self.read_index()?;
        if index
            .entries
            .iter()
            .any(|e| e.stage > 0 && e.path.as_slice() == b".gitmodules")
        {
            return Ok(Config::default());
        }
        let id = match index
            .entries
            .iter()
            .find(|e| e.stage == 0 && e.path.as_slice() == b".gitmodules")
        {
            Some(e) => Some(e.id),
            None => {
                probe(self, "head");
                match self.head_commit()? {
                    Some(head) => self
                        .entry_at(&self.commit(&head)?.tree, b".gitmodules")?
                        .map(|e| e.id),
                    None => None,
                }
            }
        };
        Ok(match id {
            Some(id) => {
                probe(self, "parse");
                Config::parse(&self.blob(&id)?)?
            }
            None => Config::default(),
        })
    }

    /// Validates the global diff setting without loading `.gitmodules`.
    pub fn validate_diff_ignore_submodules(&self) -> Result<()> {
        probe(self, "validation");
        for value in self.config().get_all("diff.ignoresubmodules") {
            parse_ignore(value)?;
        }
        Ok(())
    }

    /// Submodule settings shared by one inspection of this repository.
    pub fn submodules(&self) -> Submodules<'_> {
        Submodules {
            repo: self,
            config: OnceCell::new(),
            children: RefCell::new(HashMap::new()),
        }
    }

    /// Per-name config overrides `.gitmodules`, then `diff.ignoreSubmodules`, then the command's default.
    pub fn submodule_ignore(
        &self,
        path: &[u8],
        default: SubmoduleIgnore,
    ) -> Result<SubmoduleIgnore> {
        self.submodules().ignore(path, default)
    }

    /// A missing `.git` is an uninitialized checkout; opening never discovers the parent repository.
    pub fn submodule_status(
        &self,
        path: &[u8],
        recorded: ObjectId,
        default: SubmoduleIgnore,
    ) -> Result<Option<SubmoduleStatus>> {
        self.submodules().status(path, recorded, default)
    }
}

#[derive(Default)]
struct SubmoduleConfig {
    paths: HashMap<Vec<u8>, String>,
    ignores: HashMap<String, String>,
    default: Option<String>,
}

struct ChildInspection {
    repo: Repo,
    head: Option<ObjectId>,
    dirt: Option<(bool, bool)>,
}

/// Lazily loaded submodule settings and child inspections for one status or diff operation.
pub struct Submodules<'a> {
    repo: &'a Repo,
    config: OnceCell<SubmoduleConfig>,
    children: RefCell<HashMap<Vec<u8>, ChildInspection>>,
}

impl Submodules<'_> {
    fn config(&self) -> Result<&SubmoduleConfig> {
        if self.config.get().is_none() {
            let modules = self.repo.gitmodules()?;
            let mut config = SubmoduleConfig {
                default: self
                    .repo
                    .config()
                    .get("diff.ignoresubmodules")
                    .map(str::to_owned),
                ..SubmoduleConfig::default()
            };
            for e in modules.entries() {
                let Some(name) = e.subsection.as_ref().filter(|_| e.section == "submodule") else {
                    continue;
                };
                match e.key.as_str() {
                    "path" => {
                        if let Some(path) = &e.value {
                            config.paths.insert(path.as_bytes().to_vec(), name.clone());
                        }
                    }
                    "ignore" => {
                        let value = e.value.as_deref().unwrap_or("");
                        if parse_ignore(value).is_ok() {
                            config.ignores.insert(name.clone(), value.to_owned());
                        } else {
                            eprintln!(
                                "warning: Invalid parameter '{value}' for config option 'submodule.{name}.ignore'"
                            );
                        }
                    }
                    _ => {}
                }
            }
            for e in self.repo.config().entries() {
                if e.section == "submodule"
                    && e.key == "ignore"
                    && let Some(name) = &e.subsection
                {
                    config
                        .ignores
                        .insert(name.clone(), e.value.clone().unwrap_or_default());
                }
            }
            let _ = self.config.set(config);
        }
        Ok(self.config.get().expect("submodule settings initialized"))
    }

    pub fn ignore(&self, path: &[u8], default: SubmoduleIgnore) -> Result<SubmoduleIgnore> {
        let config = self.config()?;
        let default = config
            .default
            .as_deref()
            .map(parse_ignore)
            .transpose()?
            .unwrap_or(default);
        config
            .paths
            .get(path)
            .and_then(|name| config.ignores.get(name))
            .map(|value| parse_ignore(value))
            .transpose()
            .map(|value| value.unwrap_or(default))
    }

    /// Inspects a gitlink using this operation's settings.
    pub fn status(
        &self,
        path: &[u8],
        recorded: ObjectId,
        default: SubmoduleIgnore,
    ) -> Result<Option<SubmoduleStatus>> {
        let ignore = self.ignore(path, default)?;
        if ignore == SubmoduleIgnore::All {
            return Ok(None);
        }
        let root = fs_path(self.repo.root()?, path);
        if !root.join(".git").exists() {
            return Ok(None);
        }
        let mut children = self.children.borrow_mut();
        let child = match children.entry(path.to_vec()) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let repo = self.repo.open_submodule(&root)?;
                let head = repo.head_commit()?;
                e.insert(ChildInspection {
                    repo,
                    head,
                    dirt: None,
                })
            }
        };
        let head = child.head.unwrap_or(recorded);
        let mut state = SubmoduleStatus {
            head,
            new_commits: head != recorded,
            modified: false,
            untracked: false,
        };
        if ignore != SubmoduleIgnore::Dirty {
            if child.dirt.is_none() {
                child.repo.validate_diff_ignore_submodules()?;
                let (mut modified, mut untracked) = (false, false);
                for e in child.repo.status()?.entries {
                    modified |= e.staged.is_some()
                        || match e.submodule {
                            Some(nested) => nested.new_commits || nested.modified,
                            None => e.unstaged.is_some_and(|k| k != Kind::Untracked),
                        };
                    untracked |= e.unstaged == Some(Kind::Untracked)
                        || e.submodule.is_some_and(|s| s.untracked);
                }
                // Keep untracked dirt until the caller's status/diff policy is applied.
                child.dirt = Some((modified, untracked));
            }
            let (modified, untracked) = child.dirt.expect("child inspected");
            state.modified = modified;
            state.untracked = untracked && ignore != SubmoduleIgnore::Untracked;
        }
        Ok(Some(state))
    }
}

fn parse_ignore(value: &str) -> Result<SubmoduleIgnore> {
    match value {
        "none" => Ok(SubmoduleIgnore::None),
        "untracked" => Ok(SubmoduleIgnore::Untracked),
        "dirty" => Ok(SubmoduleIgnore::Dirty),
        "all" => Ok(SubmoduleIgnore::All),
        _ => Err(Error::Invalid(format!(
            "bad --ignore-submodules argument: {value}"
        ))),
    }
}

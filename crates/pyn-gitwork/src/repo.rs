use std::path::{Path, PathBuf};

use pyn_git::{Config, ObjectDb, Ref, RefStore, Repository};
use pyn_object::{Algorithm, Commit, Kind, ObjectId, TreeEntry, parse_tree};

use crate::error::{Error, Result, io_at};
use crate::ignore::{Ignore, parse_rules};

/// What `HEAD` points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    /// On a branch (the name without `refs/heads/`); `id` is `None` before its first commit.
    Branch {
        name: String,
        id: Option<ObjectId>,
    },
    Detached(ObjectId),
}

impl Head {
    pub fn id(&self) -> Option<ObjectId> {
        match self {
            Self::Branch { id, .. } => *id,
            Self::Detached(id) => Some(*id),
        }
    }
}

/// An opened git work tree with its object store and the config in effect (global under repository).
pub struct Repo {
    git: Repository,
    objects: ObjectDb,
    refs: RefStore,
    config: Config,
    home: Option<PathBuf>,
    xdg: Option<PathBuf>,
}

impl Repo {
    /// Opens the repository whose `.git` is at or above `start`. `home` and `xdg` locate the global config and
    /// excludes file.
    pub fn discover(start: &Path, home: Option<&Path>, xdg: Option<&Path>) -> Result<Self> {
        Self::from_git(Repository::discover(start)?, home, xdg)
    }

    pub fn open(root: &Path, home: Option<&Path>, xdg: Option<&Path>) -> Result<Self> {
        Self::from_git(Repository::open(root)?, home, xdg)
    }

    fn from_git(git: Repository, home: Option<&Path>, xdg: Option<&Path>) -> Result<Self> {
        let mut config = match home {
            Some(h) => Config::global(h, xdg)?,
            None => Config::default(),
        };
        config.extend(git.config().clone());
        Ok(Self {
            objects: git.objects()?,
            refs: git.refs(),
            config,
            home: home.map(Path::to_path_buf),
            xdg: xdg.map(Path::to_path_buf),
            git,
        })
    }

    pub fn git(&self) -> &Repository {
        &self.git
    }

    pub(crate) fn open_submodule(&self, root: &Path) -> Result<Self> {
        Self::open(root, self.home.as_deref(), self.xdg.as_deref())
    }

    pub fn algorithm(&self) -> Algorithm {
        self.git.algorithm()
    }

    pub fn objects(&self) -> &ObjectDb {
        &self.objects
    }

    pub fn refs(&self) -> &RefStore {
        &self.refs
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The work tree root; a bare repository has none.
    pub fn root(&self) -> Result<&Path> {
        self.git
            .work_tree()
            .ok_or_else(|| Error::Invalid("this operation must be run in a work tree".into()))
    }

    pub fn head(&self) -> Result<Head> {
        match self.refs.head()? {
            Ref::Direct(id) => Ok(Head::Detached(id)),
            Ref::Symbolic(target) => {
                let id = self.refs.resolve(&target)?;
                let name = target
                    .strip_prefix("refs/heads/")
                    .unwrap_or(&target)
                    .to_owned();
                Ok(Head::Branch { name, id })
            }
        }
    }

    pub fn head_commit(&self) -> Result<Option<ObjectId>> {
        Ok(self.head()?.id())
    }

    pub fn commit(&self, id: &ObjectId) -> Result<Commit> {
        let object = self.objects.read(id)?;
        if object.kind != Kind::Commit {
            return Err(Error::Revision(format!("{id} is not a commit")));
        }
        Ok(Commit::parse(self.algorithm(), &object.data)?)
    }

    pub fn tree(&self, id: &ObjectId) -> Result<Vec<TreeEntry>> {
        let object = self.objects.read(id)?;
        if object.kind != Kind::Tree {
            return Err(Error::Invalid(format!("{id} is not a tree")));
        }
        Ok(parse_tree(self.algorithm(), &object.data)?)
    }

    pub fn blob(&self, id: &ObjectId) -> Result<Vec<u8>> {
        let object = self.objects.read(id)?;
        if object.kind != Kind::Blob {
            return Err(Error::Invalid(format!("{id} is not a blob")));
        }
        Ok(object.data)
    }

    pub fn bool_setting(&self, key: &str, default: bool) -> bool {
        self.config.get_bool(key).ok().flatten().unwrap_or(default)
    }

    /// The ignore sources that apply to every folder.
    pub fn ignore(&self) -> Result<Ignore> {
        let read = |path: &Path| match std::fs::read(path) {
            Ok(bytes) => Ok(parse_rules(&bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(io_at(path)(e)),
        };
        let exclude = read(&self.git.common_dir().join("info/exclude"))?;
        let global = match self.global_excludes_path() {
            Some(path) => read(&path)?,
            None => Vec::new(),
        };
        Ok(Ignore::new(
            exclude,
            global,
            self.bool_setting("core.ignorecase", false),
        ))
    }

    fn global_excludes_path(&self) -> Option<PathBuf> {
        let home = self.home.as_deref();
        match self.config.get("core.excludesfile") {
            Some(p) if !p.is_empty() => Some(match p.strip_prefix("~/") {
                Some(rest) => home?.join(rest),
                None => PathBuf::from(p),
            }),
            _ => {
                let xdg = self
                    .xdg
                    .clone()
                    .or_else(|| home.map(|h| h.join(".config")))?;
                Some(xdg.join("git/ignore"))
            }
        }
    }
}

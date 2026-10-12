use std::path::{Path, PathBuf};

use pyn_object::Algorithm;

use crate::config::Config;
use crate::error::{Error, Result, corrupt, io_at};
use crate::index::Index;
use crate::odb::ObjectDb;
use crate::refs::RefStore;

/// An opened git repository: a main repository, a linked worktree or a submodule.
pub struct Repository {
    git_dir: PathBuf,
    common_dir: PathBuf,
    work_tree: Option<PathBuf>,
    algorithm: Algorithm,
    config: Config,
}

/// Resolves a `.git` entry to the git directory: the directory itself, or the target of a `gitdir: <path>` file
/// (relative paths are relative to the file's directory).
pub fn resolve_git_dir(dot_git: &Path) -> Result<PathBuf> {
    if dot_git.is_dir() {
        return Ok(dot_git.to_path_buf());
    }
    let text = std::fs::read_to_string(dot_git).map_err(io_at(dot_git))?;
    let target = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| corrupt(".git file", "expected `gitdir: <path>`"))?;
    let base = dot_git.parent().unwrap_or(Path::new("."));
    let dir = base.join(target);
    if !dir.is_dir() {
        return Err(corrupt(
            ".git file",
            format!("{} is not a directory", dir.display()),
        ));
    }
    Ok(dir)
}

impl Repository {
    /// Opens a work tree root (its `.git` is a directory or a `gitdir:` file) or a git directory itself.
    pub fn open(path: &Path) -> Result<Self> {
        let dot_git = path.join(".git");
        if dot_git.exists() {
            let git_dir = resolve_git_dir(&dot_git)?;
            return Self::from_git_dir(git_dir, Some(path.to_path_buf()));
        }
        if path.join("HEAD").is_file() && path.join("objects").is_dir() {
            return Self::from_git_dir(path.to_path_buf(), None);
        }
        Err(Error::Invalid {
            what: "repository",
            detail: format!("{} is not a git repository", path.display()),
        })
    }

    /// Walks up from `start` to the nearest directory holding a `.git`.
    pub fn discover(start: &Path) -> Result<Self> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            if d.join(".git").exists() {
                return Self::open(d);
            }
            dir = d.parent();
        }
        Err(Error::Invalid {
            what: "repository",
            detail: format!("no .git found at or above {}", start.display()),
        })
    }

    fn from_git_dir(git_dir: PathBuf, work_tree: Option<PathBuf>) -> Result<Self> {
        let common_dir = match std::fs::read_to_string(git_dir.join("commondir")) {
            Ok(text) => git_dir.join(text.trim()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
            Err(e) => return Err(io_at(&git_dir.join("commondir"))(e)),
        };
        let canon = |p: PathBuf| std::fs::canonicalize(&p).map_err(io_at(&p));
        let (git_dir, common_dir) = (canon(git_dir)?, canon(common_dir)?);
        let config = Config::read(&common_dir.join("config"))?;
        let algorithm = config.object_format()?;
        if config
            .get("extensions.refstorage")
            .is_some_and(|s| !s.eq_ignore_ascii_case("files"))
        {
            return Err(Error::Unsupported("reftable ref storage".into()));
        }
        let work_tree = match config.path_in("core.worktree", &common_dir) {
            Some(w) => Some(w),
            None if config.get_bool("core.bare")? == Some(true) => None,
            None => work_tree,
        };
        Ok(Self {
            git_dir,
            common_dir,
            work_tree,
            algorithm,
            config,
        })
    }

    /// The per-worktree git directory (holds `HEAD` and `index`).
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// The directory shared by all worktrees (holds objects, refs and config).
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }

    pub fn work_tree(&self) -> Option<&Path> {
        self.work_tree.as_deref()
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// The repository's own config; layer a global one under it with [`Config::global`] and [`Config::extend`].
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn objects(&self) -> Result<ObjectDb> {
        ObjectDb::open(&self.common_dir.join("objects"), self.algorithm)
    }

    pub fn refs(&self) -> RefStore {
        RefStore::new(
            self.git_dir.clone(),
            self.common_dir.clone(),
            self.algorithm,
        )
    }

    pub fn index_path(&self) -> PathBuf {
        self.git_dir.join("index")
    }

    /// The index, or `None` when the repository has none yet.
    pub fn read_index(&self) -> Result<Option<Index>> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(None);
        }
        Index::read(&path, self.algorithm).map(Some)
    }

    pub fn write_index(&self, index: &Index) -> Result<()> {
        index.write(&self.index_path(), self.algorithm)
    }
}

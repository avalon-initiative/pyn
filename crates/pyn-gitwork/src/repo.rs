use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use pyn_git::{Config, ObjectDb, Ref, RefStore, Repository};
use pyn_object::{Algorithm, Commit, FileMode, Kind, ObjectId, TreeEntry, parse_tree};

use crate::attributes::Attributes;
use crate::convert::{Conversion, Settings};
use crate::error::{Error, Result, io_at};
use crate::filter::Filters;
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

/// Where the per-folder `.gitattributes` files are read from.
#[derive(Debug, Clone, Copy)]
pub enum AttrSource {
    WorkTree,
    Index,
    Tree(ObjectId),
    Nowhere,
}

/// The system-wide attributes file; `GIT_ATTR_NOSYSTEM` switches it off.
fn system_attributes_path() -> Option<PathBuf> {
    let off = std::env::var("GIT_ATTR_NOSYSTEM")
        .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"));
    (!off).then(|| PathBuf::from("/etc/gitattributes"))
}

/// The folder a `.gitattributes` path lives in (`""` for the root), or `None` for any other path.
fn attribute_folder(path: &[u8]) -> Option<&[u8]> {
    if path == b".gitattributes" {
        return Some(b"");
    }
    path.strip_suffix(b"/.gitattributes")
}

fn blob_data(objects: &ObjectDb, id: &ObjectId) -> Option<Vec<u8>> {
    let object = objects.read(id).ok()?;
    (object.kind == Kind::Blob).then_some(object.data)
}

fn tree_entry(
    objects: &ObjectDb,
    algorithm: Algorithm,
    tree: &ObjectId,
    path: &[u8],
) -> Option<TreeEntry> {
    let mut current = *tree;
    let mut parts = path.split(|b| *b == b'/').peekable();
    while let Some(part) = parts.next() {
        let object = objects.read(&current).ok()?;
        if object.kind != Kind::Tree {
            return None;
        }
        let found = parse_tree(algorithm, &object.data)
            .ok()?
            .into_iter()
            .find(|e| e.name == part)?;
        if parts.peek().is_none() {
            return Some(found);
        }
        if !found.mode.is_tree() {
            return None;
        }
        current = found.id;
    }
    None
}

/// An opened git work tree with its object store and the config in effect (global under repository).
pub struct Repo {
    git: Repository,
    pub(crate) objects: Arc<ObjectDb>,
    pub(crate) abbrev: OnceLock<std::result::Result<usize, String>>,
    refs: RefStore,
    config: Config,
    home: Option<PathBuf>,
    xdg: Option<PathBuf>,
    attributes: Mutex<Option<Arc<Attributes>>>,
    filters: Arc<Filters>,
    pub(crate) ignore_override: Mutex<Option<crate::SubmoduleIgnore>>,
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
        let mut lfs_config = git
            .work_tree()
            .map(crate::lfs::lfsconfig_layer)
            .unwrap_or_default();
        lfs_config.extend(config.clone());
        Ok(Self {
            objects: Arc::new(git.objects()?),
            abbrev: OnceLock::new(),
            refs: git.refs(),
            config,
            home: home.map(Path::to_path_buf),
            xdg: xdg.map(Path::to_path_buf),
            attributes: Mutex::new(None),
            filters: Arc::new(Filters::new(git.work_tree(), git.common_dir(), &lfs_config)),
            ignore_override: Mutex::new(None),
            git,
        })
    }

    pub fn filters(&self) -> &Filters {
        &self.filters
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

    pub fn observe_refs(&mut self, observer: Arc<dyn pyn_git::RefObserver>) {
        self.refs.set_observer(observer);
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
        let mut commit = Commit::parse(self.algorithm(), &object.data)?;
        if self.objects.is_shallow(id) {
            commit.parents.clear();
        }
        Ok(commit)
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

    /// The attributes in effect: the system and global files, `.gitattributes` files from the work tree (from `HEAD`'s
    /// tree in a bare repository) and `info/attributes`. The folders read are cached until [`Self::reset_attributes`].
    pub fn attributes(&self) -> Result<Arc<Attributes>> {
        let mut cached = self.attributes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = &*cached {
            return Ok(a.clone());
        }
        let source = self.default_attr_source()?;
        let attrs = Arc::new(self.attributes_from(&source)?);
        *cached = Some(attrs.clone());
        Ok(attrs)
    }

    /// Where `.gitattributes` files are read from: `GIT_ATTR_SOURCE`, else `attr.tree`, else the work tree
    /// (`HEAD`'s tree in a bare repository). Only the environment variable fails on a bad name.
    fn default_attr_source(&self) -> Result<AttrSource> {
        let named = match std::env::var_os("GIT_ATTR_SOURCE") {
            Some(v) => Some((v.to_string_lossy().into_owned(), true)),
            None => self.config.get("attr.tree").map(|v| (v.to_owned(), false)),
        };
        if let Some((spec, strict)) = named {
            return match self.resolve_treeish(&spec) {
                Ok(Some(tree)) => Ok(AttrSource::Tree(tree)),
                _ if strict => Err(Error::Invalid(
                    "bad --attr-source or GIT_ATTR_SOURCE".into(),
                )),
                _ if self.git.work_tree().is_some() => Ok(AttrSource::WorkTree),
                _ => Ok(AttrSource::Nowhere),
            };
        }
        if self.git.work_tree().is_some() {
            return Ok(AttrSource::WorkTree);
        }
        let commit = self.head_commit()?.and_then(|id| self.commit(&id).ok());
        Ok(commit.map_or(AttrSource::Nowhere, |c| AttrSource::Tree(c.tree)))
    }

    /// The tree a tree-ish names: a revision, a tree id, `<rev>^{tree}` or `<rev>:<path>`.
    pub fn resolve_treeish(&self, spec: &str) -> Result<Option<ObjectId>> {
        let (base, path) = match spec.split_once(':') {
            Some((b, p)) => (b, Some(p)),
            None => (spec, None),
        };
        let base = base.strip_suffix("^{tree}").unwrap_or(base);
        let mut tree = match self.resolve(base) {
            Ok(commit) => self.commit(&commit)?.tree,
            Err(_) => match ObjectId::from_hex(self.algorithm(), base) {
                Ok(id)
                    if self
                        .objects
                        .read_opt(&id)?
                        .is_some_and(|o| o.kind == Kind::Tree) =>
                {
                    id
                }
                _ => return Ok(None),
            },
        };
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            match self.entry_at(&tree, path.as_bytes())? {
                Some(e) if e.mode.is_tree() => tree = e.id,
                _ => return Ok(None),
            }
        }
        Ok(Some(tree))
    }

    /// The attributes with the per-folder `.gitattributes` files taken from `source`; not cached.
    pub fn attributes_from(&self, source: &AttrSource) -> Result<Attributes> {
        self.attributes_with_system(source, system_attributes_path())
    }

    fn attributes_with_system(
        &self,
        source: &AttrSource,
        system: Option<PathBuf>,
    ) -> Result<Attributes> {
        let read = |path: Option<PathBuf>| match path.map(std::fs::read) {
            Some(Ok(bytes)) => Ok(bytes),
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::Io {
                path: "attributes".into(),
                source: e,
            }),
            _ => Ok(Vec::new()),
        };
        let mut global = read(system)?;
        if !global.is_empty() && global.last() != Some(&b'\n') {
            global.push(b'\n');
        }
        global.extend(read(self.global_attributes_path())?);
        let info = read(Some(self.git.common_dir().join("info/attributes")))?;
        let loader: crate::attributes::Loader = match source {
            AttrSource::WorkTree => {
                let root = self.git.work_tree().map(Path::to_path_buf);
                Box::new(move |dir: &[u8]| {
                    let file = crate::status::fs_path(root.as_deref()?, dir).join(".gitattributes");
                    let meta = std::fs::symlink_metadata(&file).ok()?;
                    meta.is_file().then(|| std::fs::read(&file).ok())?
                })
            }
            AttrSource::Index => {
                let files: HashMap<Vec<u8>, ObjectId> = self
                    .read_index()?
                    .entries
                    .iter()
                    .filter(|e| e.stage == 0 && e.mode & 0o170000 == 0o100000)
                    .filter_map(|e| {
                        let dir = attribute_folder(&e.path)?;
                        Some((dir.to_vec(), e.id))
                    })
                    .collect();
                let objects = self.objects.clone();
                Box::new(move |dir: &[u8]| blob_data(&objects, files.get(dir)?))
            }
            AttrSource::Tree(tree) => {
                let objects = self.objects.clone();
                let (tree, algorithm) = (*tree, self.algorithm());
                Box::new(move |dir: &[u8]| {
                    let mut path = dir.to_vec();
                    if !path.is_empty() {
                        path.push(b'/');
                    }
                    path.extend_from_slice(b".gitattributes");
                    let entry = tree_entry(&objects, algorithm, &tree, &path)?;
                    matches!(entry.mode, FileMode::Regular | FileMode::Executable)
                        .then(|| blob_data(&objects, &entry.id))?
                })
            }
            AttrSource::Nowhere => Box::new(|_| None),
        };
        Ok(Attributes::new(
            &global,
            &info,
            loader,
            self.bool_setting("core.ignorecase", false),
        ))
    }

    /// Forgets the cached attribute files; call after writing or removing a `.gitattributes`.
    pub fn reset_attributes(&self) {
        *self.attributes.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// The line-ending, `ident` and filter conversion that applies to `path`.
    pub fn conversion(&self, path: &[u8]) -> Result<Conversion> {
        Ok(Conversion::new(
            Settings::from_config(&self.config),
            &self.attributes()?.for_path(path),
        )
        .with_filters(&self.config, &self.filters))
    }

    fn global_attributes_path(&self) -> Option<PathBuf> {
        let home = self.home.as_deref();
        match self.config.get("core.attributesfile") {
            Some(p) if !p.is_empty() => Some(match p.strip_prefix("~/") {
                Some(rest) => home?.join(rest),
                None => PathBuf::from(p),
            }),
            _ => {
                let xdg = self
                    .xdg
                    .clone()
                    .or_else(|| home.map(|h| h.join(".config")))?;
                Some(xdg.join("git/attributes"))
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_file_has_the_lowest_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        pyn_git::init(&root.join(".git"), false, Algorithm::Sha1, "main").unwrap();
        let system = root.join("sysattrs");
        std::fs::write(&system, "*.txt sys=1 keep=1 -text").unwrap();
        std::fs::write(root.join(".gitattributes"), "*.txt sys=2\n").unwrap();
        let repo = Repo::open(&root, None, None).unwrap();
        let read = |system| {
            let attrs = repo.attributes_with_system(&AttrSource::WorkTree, system);
            attrs.unwrap().for_path(b"a.txt")
        };
        let found = read(Some(system));
        assert_eq!(found.get("sys").as_str(), Some("2"));
        assert_eq!(found.get("keep").as_str(), Some("1"));
        assert!(found.get("text").is_unset());
        let found = read(Some(root.join("missing")));
        assert!(found.get("keep").is_unspecified());
        assert_eq!(found.get("sys").as_str(), Some("2"));
        assert!(read(None).get("keep").is_unspecified());
    }
}

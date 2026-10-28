use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pyn_object::{Algorithm, ObjectId};

use crate::error::{Error, Result, corrupt, invalid, io_at};

const MAX_SYMREF_DEPTH: usize = 5;
const PER_WORKTREE: [&str; 4] = ["HEAD", "refs/bisect/", "refs/worktree/", "refs/rewritten/"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ref {
    Direct(ObjectId),
    Symbolic(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedRef {
    pub name: String,
    pub id: ObjectId,
    /// The fully peeled target of an annotated tag, when `packed-refs` records one.
    pub peeled: Option<ObjectId>,
}

/// Where a ref transaction stands when its observer is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    Prepared,
    Committed,
    Aborted,
}

/// One ref in a transaction; an absent side is `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    pub name: String,
    pub old: Option<ObjectId>,
    pub new: Option<ObjectId>,
}

/// Told of every ref transaction; refusing `Prepared` aborts it.
pub trait RefObserver: Send + Sync {
    /// Returns false to refuse a `Prepared` transaction; the answer to the other states is ignored.
    fn transaction(&self, state: TxState, updates: &[RefUpdate]) -> bool;
}

/// Loose refs, `packed-refs` and `HEAD` for one repository (and one worktree).
pub struct RefStore {
    git_dir: PathBuf,
    pub(crate) common_dir: PathBuf,
    pub(crate) algorithm: Algorithm,
    observer: Option<Arc<dyn RefObserver>>,
    in_transaction: AtomicBool,
}

impl RefStore {
    pub fn new(git_dir: PathBuf, common_dir: PathBuf, algorithm: Algorithm) -> Self {
        Self {
            git_dir,
            common_dir,
            algorithm,
            observer: None,
            in_transaction: AtomicBool::new(false),
        }
    }

    pub fn set_observer(&mut self, observer: Arc<dyn RefObserver>) {
        self.observer = Some(observer);
    }

    /// Runs `write` as one transaction over `updates`, telling the observer before and after; one inside another is not reported.
    pub fn transaction<T, E: From<Error>>(
        &self,
        updates: &[RefUpdate],
        write: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        let Some(observer) = &self.observer else {
            return write();
        };
        if self.in_transaction.swap(true, Ordering::SeqCst) {
            return write();
        }
        let result = self.observed(observer.as_ref(), updates, write);
        self.in_transaction.store(false, Ordering::SeqCst);
        result
    }

    fn observed<T, E: From<Error>>(
        &self,
        observer: &dyn RefObserver,
        updates: &[RefUpdate],
        write: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        if !observer.transaction(TxState::Prepared, updates) {
            observer.transaction(TxState::Aborted, updates);
            return Err(Error::Vetoed.into());
        }
        let result = write();
        let state = if result.is_ok() {
            TxState::Committed
        } else {
            TxState::Aborted
        };
        observer.transaction(state, updates);
        result
    }

    /// The updates a write of `name` to `new` makes, with `HEAD` first when it follows the branch.
    fn updates_for(&self, name: &str, new: Option<ObjectId>) -> Vec<RefUpdate> {
        let attached = name == "HEAD" && matches!(self.read("HEAD"), Ok(Some(Ref::Symbolic(_))));
        let old = if attached {
            None
        } else {
            self.resolve(name).ok().flatten()
        };
        let one = |n: &str| RefUpdate {
            name: n.to_owned(),
            old,
            new,
        };
        let follows =
            new.is_some() && matches!(self.read("HEAD"), Ok(Some(Ref::Symbolic(t))) if t == name);
        if follows {
            vec![one("HEAD"), one(name)]
        } else {
            vec![one(name)]
        }
    }

    pub(crate) fn dir_for(&self, name: &str) -> &Path {
        if PER_WORKTREE
            .iter()
            .any(|p| name == *p || name.starts_with(p))
        {
            &self.git_dir
        } else {
            &self.common_dir
        }
    }

    /// `HEAD` itself: symbolic (`refs/heads/main`, possibly unborn) or detached.
    pub fn head(&self) -> Result<Ref> {
        self.read("HEAD")?
            .ok_or_else(|| Error::RefNotFound("HEAD".into()))
    }

    /// One level of a ref, without following symbolic refs. Loose wins over packed.
    pub fn read(&self, name: &str) -> Result<Option<Ref>> {
        check_name(name)?;
        let path = self.dir_for(name).join(name);
        match fs::read(&path) {
            Ok(bytes) => return self.parse_loose(name, &bytes).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            // A directory where a file is expected means `name` is only a namespace.
            Err(e) if path.is_dir() || e.kind() == std::io::ErrorKind::NotADirectory => {}
            Err(e) => return Err(io_at(&path)(e)),
        }
        Ok(self
            .packed()?
            .into_iter()
            .find(|p| p.name == name)
            .map(|p| Ref::Direct(p.id)))
    }

    pub(crate) fn parse_loose(&self, name: &str, bytes: &[u8]) -> Result<Ref> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| corrupt("ref", format!("{name} is not UTF-8")))?
            .trim_end();
        if let Some(target) = text.strip_prefix("ref:") {
            let target = target.trim();
            check_name(target)?;
            return Ok(Ref::Symbolic(target.to_owned()));
        }
        ObjectId::from_hex(self.algorithm, text)
            .map(Ref::Direct)
            .map_err(|_| corrupt("ref", format!("{name} holds neither an id nor a symref")))
    }

    /// Follows symbolic refs to an id; `None` if the chain ends at a missing ref (an unborn branch).
    pub fn resolve(&self, name: &str) -> Result<Option<ObjectId>> {
        let mut current = name.to_owned();
        for _ in 0..MAX_SYMREF_DEPTH {
            match self.read(&current)? {
                None => return Ok(None),
                Some(Ref::Direct(id)) => return Ok(Some(id)),
                Some(Ref::Symbolic(next)) => current = next,
            }
        }
        Err(corrupt("ref", format!("symbolic ref loop at {name}")))
    }

    pub fn packed(&self) -> Result<Vec<PackedRef>> {
        let path = self.common_dir.join("packed-refs");
        match fs::read(&path) {
            Ok(bytes) => parse_packed(self.algorithm, &bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(io_at(&path)(e)),
        }
    }

    /// All refs under `prefix` (for example `refs/heads/`), resolved to ids and sorted by name.
    pub fn list(&self, prefix: &str) -> Result<Vec<(String, ObjectId)>> {
        let mut found: BTreeMap<String, ObjectId> = BTreeMap::new();
        for p in self.packed()? {
            if p.name.starts_with(prefix) {
                found.insert(p.name, p.id);
            }
        }
        let mut loose = Vec::new();
        walk(&self.common_dir.join("refs"), "refs", &mut loose)?;
        for name in loose {
            if !name.starts_with(prefix) || check_name(&name).is_err() {
                continue;
            }
            if let Some(id) = self.resolve(&name)? {
                found.insert(name, id);
            }
        }
        Ok(found.into_iter().collect())
    }

    /// Creates or moves a direct ref through a `.lock` file, as git does.
    pub fn update(&self, name: &str, id: &ObjectId) -> Result<()> {
        self.update_checked(name, Expect::Any, id)
    }

    /// Like [`RefStore::update`], but fails unless the ref is as `expect` says once the lock is held.
    pub fn update_checked(&self, name: &str, expect: Expect, id: &ObjectId) -> Result<()> {
        check_name(name)?;
        if id.algorithm() != self.algorithm {
            return Err(invalid("ref", "id algorithm differs from the repository"));
        }
        self.write_ref(name, expect, format!("{id}\n"))
    }

    pub fn set_symbolic(&self, name: &str, target: &str) -> Result<()> {
        check_name(name)?;
        check_name(target)?;
        self.write_ref(name, Expect::Any, format!("ref: {target}\n"))
    }

    fn write_ref(&self, name: &str, expect: Expect, content: String) -> Result<()> {
        let path = self.dir_for(name).join(name);
        let parent = path.parent().expect("ref path has a parent");
        fs::create_dir_all(parent).map_err(io_at(parent))?;
        let lock = lock_path(&path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let result = self.check_expect(name, expect).and_then(|()| {
            let new = content.strip_prefix("ref: ").map_or_else(
                || ObjectId::from_hex(self.algorithm, content.trim()).ok(),
                |_| None,
            );
            let mut publish = || {
                file.write_all(content.as_bytes())
                    .and_then(|()| file.sync_all())
                    .and_then(|()| fs::rename(&lock, &path))
                    .map_err(io_at(&path))
            };
            match new {
                Some(id) => self.transaction(&self.updates_for(name, Some(id)), publish),
                None => publish(),
            }
        });
        if result.is_err() {
            let _ = fs::remove_file(&lock);
        }
        result
    }

    fn check_expect(&self, name: &str, expect: Expect) -> Result<()> {
        let current = match self.read(name)? {
            Some(Ref::Direct(id)) => Some(id),
            _ => None,
        };
        let ok = match expect {
            Expect::Any => true,
            Expect::Absent => current.is_none(),
            Expect::Id(id) => current == Some(id),
        };
        if ok {
            Ok(())
        } else {
            Err(Error::RefChanged(name.to_owned()))
        }
    }

    /// Removes a ref, loose and packed, through its `.lock` file; fails if it is not as `expect` says.
    pub fn delete(&self, name: &str, expect: Expect) -> Result<()> {
        check_name(name)?;
        let path = self.dir_for(name).join(name);
        let parent = path.parent().expect("ref path has a parent");
        fs::create_dir_all(parent).map_err(io_at(parent))?;
        let lock = lock_path(&path);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let result = self.check_expect(name, expect).and_then(|()| {
            self.transaction(&self.updates_for(name, None), || {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_at(&path)(e)),
                }
                self.remove_packed(name)
            })
        });
        let _ = fs::remove_file(&lock);
        if result.is_ok() {
            self.prune_empty_dirs(&self.common_dir.join("refs"), parent);
        }
        result
    }

    fn remove_packed(&self, name: &str) -> Result<()> {
        let path = self.common_dir.join("packed-refs");
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(io_at(&path)(e)),
        };
        if !parse_packed(self.algorithm, &bytes)?
            .iter()
            .any(|p| p.name == name)
        {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut kept = String::new();
        let mut skipping = false;
        for line in text.split_inclusive('\n') {
            if line.starts_with('^') {
                if skipping {
                    continue;
                }
            } else if !line.starts_with('#') {
                skipping = line
                    .trim_end()
                    .split_once(' ')
                    .is_some_and(|(_, n)| n == name);
                if skipping {
                    continue;
                }
            }
            kept.push_str(line);
        }
        let lock = lock_path(&path);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let result = file
            .write_all(kept.as_bytes())
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&lock, &path));
        if let Err(e) = result {
            let _ = fs::remove_file(&lock);
            return Err(io_at(&path)(e));
        }
        Ok(())
    }

    /// Removes empty folders from `dir` upward, keeping `base` and its first level (`heads`, `tags`).
    pub(crate) fn prune_empty_dirs(&self, refs: &Path, dir: &Path) {
        let mut dir = dir.to_path_buf();
        while dir.starts_with(refs) && dir.parent() != Some(refs) && dir != refs {
            if fs::remove_dir(&dir).is_err() {
                break;
            }
            match dir.parent() {
                Some(p) => dir = p.to_path_buf(),
                None => break,
            }
        }
    }
}

/// What a ref must hold when a write or delete gets the lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Any,
    Absent,
    Id(ObjectId),
}

pub(crate) fn lock_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.lock", path.display()))
}

pub(crate) fn walk(dir: &Path, name: &str, out: &mut Vec<String>) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in rd {
        let entry = entry.map_err(io_at(dir))?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        let child = format!("{name}/{file_name}");
        if entry.file_type().map_err(io_at(dir))?.is_dir() {
            walk(&entry.path(), &child, out)?;
        } else if !file_name.ends_with(".lock") {
            out.push(child);
        }
    }
    Ok(())
}

fn parse_packed(algorithm: Algorithm, bytes: &[u8]) -> Result<Vec<PackedRef>> {
    let text = std::str::from_utf8(bytes).map_err(|_| corrupt("packed-refs", "not UTF-8"))?;
    let hex_len = algorithm.digest_len() * 2;
    let mut out: Vec<PackedRef> = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(peeled) = line.strip_prefix('^') {
            let id = ObjectId::from_hex(algorithm, peeled)
                .map_err(|_| corrupt("packed-refs", "bad peeled id"))?;
            out.last_mut()
                .ok_or_else(|| corrupt("packed-refs", "peeled line before any ref"))?
                .peeled = Some(id);
            continue;
        }
        let (hex, name) = line
            .split_once(' ')
            .filter(|(h, _)| h.len() == hex_len)
            .ok_or_else(|| corrupt("packed-refs", format!("bad line {line:?}")))?;
        check_name(name)?;
        out.push(PackedRef {
            name: name.to_owned(),
            id: ObjectId::from_hex(algorithm, hex).map_err(|_| corrupt("packed-refs", "bad id"))?,
            peeled: None,
        });
    }
    Ok(out)
}

/// Rejects names that could escape the git dir or that git itself refuses.
pub fn check_name(name: &str) -> Result<()> {
    let bad = |why: &str| Err(invalid("ref name", format!("{name:?}: {why}")));
    if name.is_empty() || name.starts_with('/') || name.ends_with('/') || name.ends_with('.') {
        return bad("empty or edge slash or dot");
    }
    if name.contains("..") || name.contains("//") || name.contains("@{") || name == "@" {
        return bad("forbidden sequence");
    }
    if name
        .chars()
        .any(|c| c.is_control() || " ~^:?*[\\".contains(c))
    {
        return bad("forbidden character");
    }
    for part in name.split('/') {
        if part.starts_with('.') || part.ends_with(".lock") {
            return bad("component starts with a dot or ends in .lock");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in [
            "HEAD",
            "refs/heads/main",
            "refs/tags/v1.0",
            "refs/heads/a-b/c",
        ] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/x",
            "refs/../x",
            "refs/heads/a.lock",
            ".hidden",
            "a b",
            "a//b",
            "a/",
            "x\u{0}y",
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn packed_with_peeled() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let text = format!(
            "# pack-refs with: peeled fully-peeled sorted \n{a} refs/tags/v1\n^{b}\n{b} refs/heads/main\n"
        );
        let refs = parse_packed(Algorithm::Sha1, text.as_bytes()).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(
            refs[0].peeled,
            Some(ObjectId::from_hex(Algorithm::Sha1, &b).unwrap())
        );
        assert!(refs[1].peeled.is_none());
        assert!(parse_packed(Algorithm::Sha1, b"zz refs/x\n").is_err());
        assert!(parse_packed(Algorithm::Sha256, text.as_bytes()).is_err());
    }
}

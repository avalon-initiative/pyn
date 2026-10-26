use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use pyn_object::{Algorithm, Kind, ObjectId, hash_object};

use crate::error::{Error, Result, corrupt, io_at};
use crate::pack::Pack;

const MAX_ALTERNATE_DEPTH: usize = 5;

/// A decoded object: its kind and body, without the loose-object header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub kind: Kind,
    pub data: Vec<u8>,
}

/// A repository's object store: loose objects, packs and alternates.
pub struct ObjectDb {
    dir: PathBuf,
    algorithm: Algorithm,
    packs: RwLock<Vec<Arc<Pack>>>,
    alternates: Vec<ObjectDb>,
    shallow: RwLock<Arc<HashSet<ObjectId>>>,
}

impl ObjectDb {
    pub fn open(objects_dir: &Path, algorithm: Algorithm) -> Result<Self> {
        Self::open_depth(objects_dir, algorithm, 0)
    }

    fn open_depth(dir: &Path, algorithm: Algorithm, depth: usize) -> Result<Self> {
        let mut alternates = Vec::new();
        let list = dir.join("info/alternates");
        if depth < MAX_ALTERNATE_DEPTH && list.is_file() {
            let text = fs::read_to_string(&list).map_err(io_at(&list))?;
            for line in text
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                let path = dir.join(line);
                if path.is_dir() {
                    alternates.push(Self::open_depth(&path, algorithm, depth + 1)?);
                }
            }
        }
        let db = Self {
            dir: dir.to_path_buf(),
            algorithm,
            packs: RwLock::new(Vec::new()),
            alternates,
            shallow: RwLock::default(),
        };
        db.rescan_packs()?;
        Ok(db)
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Commits whose parents are treated as absent (a shallow repository's boundary).
    pub fn set_shallow(&self, ids: HashSet<ObjectId>) {
        *self.shallow.write().expect("shallow lock") = Arc::new(ids);
    }

    pub fn is_shallow(&self, id: &ObjectId) -> bool {
        self.shallow.read().expect("shallow lock").contains(id)
    }

    pub(crate) fn alternates_list(&self) -> &[ObjectDb] {
        &self.alternates
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn packs(&self) -> Vec<Arc<Pack>> {
        self.packs.read().expect("pack list lock").clone()
    }

    /// Picks up packs created since the last scan; returns whether any were added.
    pub(crate) fn rescan_packs(&self) -> Result<bool> {
        let pack_dir = self.dir.join("pack");
        let Ok(rd) = fs::read_dir(&pack_dir) else {
            return Ok(false);
        };
        let mut found = Vec::new();
        for entry in rd {
            let path = entry.map_err(io_at(&pack_dir))?.path();
            if path.extension().is_some_and(|e| e == "idx") && path.with_extension("pack").is_file()
            {
                found.push(path);
            }
        }
        found.sort();
        let mut packs = self.packs.write().expect("pack list lock");
        let mut added = false;
        for idx in found {
            if packs.iter().all(|p| p.idx_path() != idx) {
                packs.push(Arc::new(Pack::open(&idx, self.algorithm)?));
                added = true;
            }
        }
        Ok(added)
    }

    fn loose_path(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.dir.join(&hex[..2]).join(&hex[2..])
    }

    fn check_algorithm(&self, id: &ObjectId) -> Result<()> {
        if id.algorithm() != self.algorithm {
            return Err(crate::error::invalid(
                "object id",
                format!("{id:?} in a {} repository", self.algorithm.name()),
            ));
        }
        Ok(())
    }

    pub fn contains(&self, id: &ObjectId) -> Result<bool> {
        if id.algorithm() != self.algorithm {
            return Ok(false);
        }
        Ok(self.read_opt(id)?.is_some())
    }

    pub fn read(&self, id: &ObjectId) -> Result<Object> {
        self.read_opt(id)?.ok_or(Error::ObjectNotFound(*id))
    }

    /// Reads `id` from packs, then loose, then alternates, verifying its hash; `None` if absent everywhere.
    pub fn read_opt(&self, id: &ObjectId) -> Result<Option<Object>> {
        self.check_algorithm(id)?;
        for _ in 0..2 {
            for pack in self.packs() {
                if let Some(o) = pack.read(id)? {
                    return Ok(Some(o));
                }
            }
            if let Some(o) = self.read_loose(id)? {
                return Ok(Some(o));
            }
            if !self.rescan_packs()? {
                break;
            }
        }
        for alt in &self.alternates {
            if let Some(o) = alt.read_opt(id)? {
                return Ok(Some(o));
            }
        }
        Ok(None)
    }

    fn read_loose(&self, id: &ObjectId) -> Result<Option<Object>> {
        let path = self.loose_path(id);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_at(&path)(e)),
        };
        let mut z = ZlibDecoder::new(file);
        let mut header = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match z.read(&mut byte) {
                Ok(1) if byte[0] == 0 => break,
                Ok(1) if header.len() < 40 => header.push(byte[0]),
                Ok(_) => return Err(corrupt("loose object", format!("{id}: bad header"))),
                Err(e) => return Err(corrupt("loose object", format!("{id}: {e}"))),
            }
        }
        let bad = || corrupt("loose object", format!("{id}: bad header"));
        let sp = header.iter().position(|b| *b == b' ').ok_or_else(bad)?;
        let kind = Kind::from_name(&header[..sp]).ok_or_else(bad)?;
        let len: u64 = std::str::from_utf8(&header[sp + 1..])
            .ok()
            .filter(|s| !s.starts_with('0') || *s == "0")
            .and_then(|s| s.parse().ok())
            .ok_or_else(bad)?;
        let take = len.checked_add(1).ok_or_else(bad)?;
        let mut data = Vec::with_capacity(len.min(64 << 20) as usize);
        z.take(take)
            .read_to_end(&mut data)
            .map_err(|e| corrupt("loose object", format!("{id}: {e}")))?;
        if data.len() as u64 != len {
            return Err(corrupt(
                "loose object",
                format!("{id}: length differs from header"),
            ));
        }
        if hash_object(self.algorithm, kind, &data)? != *id {
            return Err(corrupt(
                "loose object",
                format!("{id}: content does not match its id"),
            ));
        }
        Ok(Some(Object { kind, data }))
    }

    /// Writes a loose object the way git does (zlib, `0444`, temp file then rename) unless it already exists.
    pub fn write(&self, kind: Kind, data: &[u8]) -> Result<ObjectId> {
        let id = hash_object(self.algorithm, kind, data)?;
        if self.contains(&id)? {
            return Ok(id);
        }
        self.store_loose(&id, kind, data, None)?;
        Ok(id)
    }

    /// Writes a loose copy even when a pack holds the object, stamping the file with `mtime` when given.
    pub fn write_loose(
        &self,
        kind: Kind,
        data: &[u8],
        mtime: Option<std::time::SystemTime>,
    ) -> Result<ObjectId> {
        let id = hash_object(self.algorithm, kind, data)?;
        if !self.loose_path(&id).is_file() {
            self.store_loose(&id, kind, data, mtime)?;
        }
        Ok(id)
    }

    fn store_loose(
        &self,
        id: &ObjectId,
        kind: Kind,
        data: &[u8],
        mtime: Option<std::time::SystemTime>,
    ) -> Result<()> {
        let path = self.loose_path(id);
        let parent = path.parent().expect("loose path has a parent");
        fs::create_dir_all(parent).map_err(io_at(parent))?;

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = parent.join(format!(
            "tmp_obj_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> std::io::Result<()> {
            let file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            let mut z = ZlibEncoder::new(file, Compression::default());
            z.write_all(format!("{} {}\0", kind.name(), data.len()).as_bytes())?;
            z.write_all(data)?;
            let file = z.finish()?;
            file.sync_all()?;
            if let Some(t) = mtime {
                file.set_modified(t)?;
            }
            set_readonly(&file)?;
            fs::rename(&tmp, &path)
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(io_at(&path)(e));
        }
        Ok(())
    }
}

fn set_readonly(file: &File) -> std::io::Result<()> {
    let mut perms = file.metadata()?.permissions();
    perms.set_readonly(true);
    file.set_permissions(perms)
}

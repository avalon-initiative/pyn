use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use pyn_object::{Algorithm, Kind, ObjectId, hash_object};

use crate::error::{Error, Result, corrupt, io_at};
use crate::pack::Pack;

const MAX_ALTERNATE_DEPTH: usize = 5;

/// Fetches objects a partial clone lacks from its promisor remote into the repository whose common directory is given.
pub type PromisorFetch = fn(&Path, &[ObjectId]) -> std::result::Result<(), String>;

static PROMISOR_FETCH: OnceLock<PromisorFetch> = OnceLock::new();

thread_local! {
    static FETCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs the function that downloads missing promisor objects; the first call wins.
pub fn set_promisor_fetch(f: PromisorFetch) {
    let _ = PROMISOR_FETCH.set(f);
}

struct Promisor {
    common_dir: PathBuf,
    /// Objects a download already failed to bring, so a command does not ask again.
    failed: Mutex<HashSet<ObjectId>>,
    objects: OnceLock<HashSet<ObjectId>>,
}

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
    promisor: Option<Promisor>,
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
            promisor: None,
        };
        db.rescan_packs()?;
        Ok(db)
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    /// Marks this store as a partial clone's: objects it lacks are downloaded on demand by [`read`](Self::read).
    pub fn with_promisor(mut self, common_dir: &Path) -> Self {
        self.promisor = Some(Promisor {
            common_dir: common_dir.to_path_buf(),
            failed: Mutex::default(),
            objects: OnceLock::new(),
        });
        self
    }

    pub fn is_promisor(&self) -> bool {
        self.promisor.is_some()
    }

    /// Whether a pack here has a `.promisor` file.
    pub fn is_promisor_pack(&self, pack: &Pack) -> bool {
        pack.pack_path().with_extension("promisor").is_file()
    }

    /// Objects held in promisor packs and the ones those objects name: what a partial clone may lack.
    pub fn is_promised(&self, id: &ObjectId) -> bool {
        let Some(p) = &self.promisor else {
            return false;
        };
        p.objects.get_or_init(|| self.promised_set()).contains(id)
    }

    fn promised_set(&self) -> HashSet<ObjectId> {
        let mut set = HashSet::new();
        for pack in self.packs().iter().filter(|p| self.is_promisor_pack(p)) {
            for id in pack.ids() {
                set.insert(id);
                if let Ok(Some(o)) = pack.read(&id) {
                    set.extend(crate::walk::object_links(self.algorithm, &id, &o));
                }
            }
        }
        set
    }

    /// Downloads whichever of `ids` are absent; a no-op outside a partial clone.
    pub fn prefetch(&self, ids: &[ObjectId]) -> Result<()> {
        let Some(p) = &self.promisor else {
            return Ok(());
        };
        let missing: Vec<ObjectId> = {
            let failed = p.failed.lock().expect("failed lock");
            let mut seen = HashSet::new();
            ids.iter()
                .filter(|i| seen.insert(**i) && !failed.contains(i))
                .filter(|i| !self.has_local(i))
                .copied()
                .collect()
        };
        if missing.is_empty() || FETCHING.with(|f| f.replace(true)) {
            return Ok(());
        }
        let outcome = match PROMISOR_FETCH.get() {
            Some(fetch) => fetch(&p.common_dir, &missing),
            None => Err("no way to reach the promisor remote".into()),
        };
        FETCHING.with(|f| f.set(false));
        self.rescan_packs()?;
        let mut failed = p.failed.lock().expect("failed lock");
        for id in &missing {
            if !self.has_local(id) {
                failed.insert(*id);
            }
        }
        outcome.map_err(|e| Error::Unsupported(format!("promisor fetch: {e}")))
    }

    fn has_local(&self, id: &ObjectId) -> bool {
        id.algorithm() == self.algorithm && matches!(self.read_local_opt(id), Ok(Some(_)))
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

    /// Object ids in this store and its recursive alternates, without duplicates.
    /// `prefix` must be normalized lowercase ASCII hex: empty matches all ids, otherwise one or more digits.
    pub fn ids_with_prefix(&self, prefix: &str) -> Result<Vec<ObjectId>> {
        let mut found = Vec::new();
        self.collect_ids(prefix, &mut found)?;
        found.sort_by_key(ObjectId::to_hex);
        found.dedup();
        Ok(found)
    }

    fn collect_ids(&self, prefix: &str, found: &mut Vec<ObjectId>) -> Result<()> {
        self.rescan_packs()?;
        for pack in self.packs() {
            found.extend(pack.ids().filter(|id| id.to_hex().starts_with(prefix)));
        }
        let dirs: Vec<String> = if prefix.len() >= 2 {
            vec![prefix[..2].to_string()]
        } else {
            (0..256).map(|n| format!("{n:02x}")).collect()
        };
        for fanout in dirs {
            let dir = self.dir.join(&fanout);
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries {
                    let entry = entry.map_err(io_at(&dir))?;
                    let hex = format!("{fanout}{}", entry.file_name().to_string_lossy());
                    if hex.starts_with(prefix)
                        && let Ok(id) = ObjectId::from_hex(self.algorithm, &hex)
                    {
                        found.push(id);
                    }
                }
            }
        }
        for alternate in &self.alternates {
            alternate.collect_ids(prefix, found)?;
        }
        Ok(())
    }

    /// Refreshes pack metadata, then extends at least two hex digits using index searches and one loose
    /// fan-out read per store. A failed refresh selects the full hash width.
    pub fn abbreviation_len(&self, id: &ObjectId, minimum: usize) -> usize {
        let full = self.algorithm.digest_len() * 2;
        let mut len = minimum.min(full);
        if len == full {
            return full;
        }
        if self.rescan_packs().is_err() {
            return full;
        }
        for pack in self.packs() {
            len = pack.abbreviation_len(id, len);
        }
        let hex = id.to_hex();
        let dir = self.dir.join(&hex[..2]);
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let other = format!("{}{}", &hex[..2], entry.file_name().to_string_lossy());
                if let Ok(other) = ObjectId::from_hex(self.algorithm, &other) {
                    len = len.max(crate::pack::distinguishing_len(
                        id.as_bytes(),
                        other.as_bytes(),
                    ));
                }
            }
        }
        for alternate in &self.alternates {
            len = alternate.abbreviation_len(id, len);
        }
        len
    }

    /// Git estimates size from packed objects, counting each canonical directory once and distinct packs separately.
    pub fn approximate_object_count(&self) -> Result<usize> {
        self.count_packed_objects(&mut HashSet::new())
    }

    fn count_packed_objects(&self, seen: &mut HashSet<PathBuf>) -> Result<usize> {
        let dir = fs::canonicalize(&self.dir).map_err(io_at(&self.dir))?;
        if !seen.insert(dir) {
            return Ok(0);
        }
        self.rescan_packs()?;
        let mut count: usize = self.packs().iter().map(|pack| pack.len()).sum();
        for alternate in &self.alternates {
            count = count.saturating_add(alternate.count_packed_objects(seen)?);
        }
        Ok(count)
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
        let known: HashSet<_> = packs.iter().map(|p| p.idx_path().to_path_buf()).collect();
        let mut added = false;
        for idx in found {
            if !known.contains(&idx) {
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

    /// Whether the object is stored here; never downloads.
    pub fn contains(&self, id: &ObjectId) -> Result<bool> {
        if id.algorithm() != self.algorithm {
            return Ok(false);
        }
        Ok(self.read_local_opt(id)?.is_some())
    }

    pub fn read(&self, id: &ObjectId) -> Result<Object> {
        self.read_opt(id)?.ok_or(Error::ObjectNotFound(*id))
    }

    /// Like [`read_local_opt`](Self::read_local_opt), but a partial clone downloads an absent object from its promisor remote.
    pub fn read_opt(&self, id: &ObjectId) -> Result<Option<Object>> {
        if let Some(o) = self.read_local_opt(id)? {
            return Ok(Some(o));
        }
        if self.promisor.is_none() {
            return Ok(None);
        }
        self.prefetch(&[*id])?;
        self.read_local_opt(id)
    }

    /// Reads `id` from packs, then loose, then alternates, verifying its hash; `None` if absent everywhere.
    pub fn read_local_opt(&self, id: &ObjectId) -> Result<Option<Object>> {
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
            if let Some(o) = alt.read_local_opt(id)? {
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

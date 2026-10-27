use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pyn_object::{Kind, ObjectId};

use crate::delta;
use crate::error::{Error, Result, io_at};
use crate::odb::ObjectDb;
use crate::pack_write::{PackEntry, PackWriter};

const PACK_EXTRAS: [&str; 5] = ["keep", "bitmap", "promisor", "rev", "mtimes"];

/// A loose object file.
#[derive(Debug, Clone)]
pub struct LooseObject {
    pub id: ObjectId,
    pub path: PathBuf,
    /// Bytes the file occupies on disk (blocks, where the platform reports them).
    pub disk_bytes: u64,
    pub mtime: i64,
}

/// A file in the object store that git would not recognise.
#[derive(Debug, Clone)]
pub struct Garbage {
    pub path: PathBuf,
    pub bytes: u64,
}

/// One object to pack; `hint` groups likely delta partners (see [`name_hash`]).
#[derive(Debug, Clone, Copy)]
pub struct PackItem {
    pub id: ObjectId,
    pub hint: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct PackOptions {
    pub deltas: bool,
    pub window: usize,
    pub depth: usize,
    /// Keep the deltas objects already have in existing packs (git's default; `repack -f` turns it off).
    pub reuse: bool,
}

impl Default for PackOptions {
    fn default() -> Self {
        Self {
            deltas: true,
            window: 10,
            depth: 50,
            reuse: true,
        }
    }
}

/// The hash git buckets path names by, so versions of one file sort next to each other.
pub fn name_hash(name: &[u8]) -> u32 {
    name.iter()
        .filter(|b| !b.is_ascii_whitespace())
        .fold(0u32, |h, b| (h >> 2).wrapping_add(u32::from(*b) << 24))
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(unix)]
fn disk_bytes(meta: &fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::blocks(meta) * 512
}

#[cfg(not(unix))]
fn disk_bytes(meta: &fs::Metadata) -> u64 {
    meta.len()
}

fn is_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl ObjectDb {
    /// The loose objects of this store alone, and files in the fan-out folders that are not objects.
    pub fn loose_objects(&self) -> Result<(Vec<LooseObject>, Vec<Garbage>)> {
        let rest_len = self.algorithm().digest_len() * 2 - 2;
        let mut found = Vec::new();
        let mut garbage = Vec::new();
        let Ok(top) = fs::read_dir(self.dir()) else {
            return Ok((found, garbage));
        };
        for dir in top {
            let dir = dir.map_err(io_at(self.dir()))?;
            let prefix = dir.file_name().to_string_lossy().into_owned();
            if prefix.len() != 2 || !is_hex(&prefix) || !dir.path().is_dir() {
                continue;
            }
            for entry in fs::read_dir(dir.path()).map_err(io_at(&dir.path()))? {
                let entry = entry.map_err(io_at(&dir.path()))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let meta = entry.metadata().map_err(io_at(&entry.path()))?;
                let id = (name.len() == rest_len && is_hex(&name))
                    .then(|| ObjectId::from_hex(self.algorithm(), &format!("{prefix}{name}")).ok())
                    .flatten();
                match id {
                    Some(id) => found.push(LooseObject {
                        id,
                        path: entry.path(),
                        disk_bytes: disk_bytes(&meta),
                        mtime: mtime_of(&meta),
                    }),
                    None => garbage.push(Garbage {
                        path: entry.path(),
                        bytes: meta.len(),
                    }),
                }
            }
        }
        found.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
        garbage.sort_by(|a, b| a.path.cmp(&b.path));
        Ok((found, garbage))
    }

    /// Files in `pack/` that are not a pack with its index or a known companion of one.
    pub fn pack_garbage(&self) -> Result<Vec<Garbage>> {
        let dir = self.dir().join("pack");
        let Ok(rd) = fs::read_dir(&dir) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry.map_err(io_at(&dir))?;
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let named = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("pack-"));
            let partner = |e: &str| path.with_extension(e).is_file();
            let ok = named
                && match ext {
                    "idx" => partner("pack"),
                    "pack" => partner("idx"),
                    e if PACK_EXTRAS.contains(&e) => partner("pack"),
                    _ => false,
                };
            if !ok {
                out.push(Garbage {
                    bytes: entry.metadata().map_err(io_at(&path))?.len(),
                    path,
                });
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// The object stores named in `info/alternates`.
    pub fn alternate_dirs(&self) -> Vec<PathBuf> {
        self.alternates_list()
            .iter()
            .map(|a| a.dir().to_path_buf())
            .collect()
    }

    /// Whether `id` is in a pack here or in an alternate.
    pub fn is_packed(&self, id: &ObjectId) -> bool {
        self.packs().iter().any(|p| p.contains(id))
            || self.alternates_list().iter().any(|a| a.is_packed(id))
    }

    /// Whether a pack of this store itself holds `id`.
    pub fn is_local_packed(&self, id: &ObjectId) -> bool {
        self.packs().iter().any(|p| p.contains(id))
    }

    /// Whether `id` is stored in this store itself (loose or packed), ignoring alternates.
    pub fn is_local(&self, id: &ObjectId) -> bool {
        self.packs().iter().any(|p| p.contains(id)) || self.loose_file(id).is_file()
    }

    pub fn loose_file(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.dir().join(&hex[..2]).join(&hex[2..])
    }

    /// Deletes a loose object and its fan-out folder when that empties it.
    pub fn remove_loose(&self, id: &ObjectId) -> Result<bool> {
        let path = self.loose_file(id);
        match make_writable_then_remove(&path) {
            Ok(()) => {
                if let Some(dir) = path.parent() {
                    let _ = fs::remove_dir(dir);
                }
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_at(&path)(e)),
        }
    }

    /// Writes `items` as one pack in `pack/`, with deltas when `options` ask; returns the pack's name hash.
    pub fn write_packfile(
        &self,
        items: &[PackItem],
        options: &PackOptions,
    ) -> Result<Option<String>> {
        let mut seen = HashSet::new();
        let items: Vec<PackItem> = items
            .iter()
            .copied()
            .filter(|i| seen.insert(i.id))
            .collect();
        if items.is_empty() {
            return Ok(None);
        }
        let count = u32::try_from(items.len()).map_err(|_| Error::Limit {
            what: "pack object count",
            limit: u64::from(u32::MAX),
        })?;
        let mut order = Vec::with_capacity(items.len());
        for item in &items {
            let o = self.read(&item.id)?;
            order.push((rank(o.kind), item.hint, o.data.len(), item.id));
        }
        order.sort_by(|a, b| {
            (a.0, a.1, std::cmp::Reverse(a.2), a.3.as_bytes()).cmp(&(
                b.0,
                b.1,
                std::cmp::Reverse(b.2),
                b.3.as_bytes(),
            ))
        });
        let dir = self.dir().join("pack");
        fs::create_dir_all(&dir).map_err(io_at(&dir))?;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = dir.join(format!(
            "tmp_pack_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = self.write_pack_into(&tmp, &order, count, options);
        let summary = match result {
            Ok(s) => s,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let name = summary.checksum.to_hex();
        let pack = dir.join(format!("pack-{name}.pack"));
        let idx = dir.join(format!("pack-{name}.idx"));
        let tmp_idx = tmp.with_extension("idx");
        let finish = (|| -> std::io::Result<()> {
            fs::write(&tmp_idx, summary.index())?;
            make_readonly(&tmp)?;
            make_readonly(&tmp_idx)?;
            fs::rename(&tmp, &pack)?;
            fs::rename(&tmp_idx, &idx)
        })();
        if let Err(e) = finish {
            let _ = fs::remove_file(&tmp);
            let _ = fs::remove_file(&tmp_idx);
            return Err(io_at(&pack)(e));
        }
        self.rescan_packs()?;
        if let Some(pack) = self.packs().into_iter().find(|p| p.idx_path() == idx) {
            replace_readonly(&idx.with_extension("rev"), &pack.reverse_index()?)?;
        }
        Ok(Some(name))
    }

    fn write_pack_into(
        &self,
        path: &Path,
        order: &[(u8, u32, usize, ObjectId)],
        count: u32,
        options: &PackOptions,
    ) -> Result<crate::PackSummary> {
        let file = File::create(path).map_err(io_at(path))?;
        let mut w = PackWriter::new(BufWriter::new(file), self.algorithm(), count)?;
        let mut window: VecDeque<Candidate> = VecDeque::new();
        let (order, mut reusable) = self.reuse_plan(order, options)?;
        let mut written: HashMap<ObjectId, (u64, usize)> = HashMap::new();
        for id in order {
            if let Some((base, delta)) = reusable.remove(&id)
                && let Some(&(base_offset, depth)) = written.get(&base)
                && depth < options.depth
            {
                let at = w.add(
                    id,
                    PackEntry::OfsDelta {
                        base_offset,
                        delta: &delta,
                    },
                )?;
                written.insert(id, (at, depth + 1));
                continue;
            }
            let o = self.read(&id)?;
            let mut best: Option<(usize, Vec<u8>)> = None;
            let try_delta = options.deltas
                && options.window > 0
                && matches!(o.kind, Kind::Tree | Kind::Blob)
                && o.data.len() >= 64;
            if try_delta {
                for (n, c) in window.iter().enumerate() {
                    if c.kind != o.kind || c.depth >= options.depth {
                        continue;
                    }
                    let limit = best
                        .as_ref()
                        .map_or(o.data.len() / 2, |(_, d)| d.len())
                        .saturating_sub(1);
                    if let Some(d) = delta::create(&c.data, &o.data, limit) {
                        best = Some((n, d));
                    }
                }
            }
            let (offset, depth) = match &best {
                Some((n, d)) => {
                    let base = &window[*n];
                    let at = w.add(
                        id,
                        PackEntry::OfsDelta {
                            base_offset: base.offset,
                            delta: d,
                        },
                    )?;
                    (at, base.depth + 1)
                }
                None => (
                    w.add(
                        id,
                        PackEntry::Whole {
                            kind: o.kind,
                            data: &o.data,
                        },
                    )?,
                    0,
                ),
            };
            written.insert(id, (offset, depth));
            if options.deltas && matches!(o.kind, Kind::Tree | Kind::Blob) {
                window.push_front(Candidate {
                    kind: o.kind,
                    offset,
                    depth,
                    data: o.data,
                });
                window.truncate(options.window);
            }
        }
        w.finish()
    }
}

impl ObjectDb {
    /// Orders `order` so a delta follows its base, and returns the existing deltas that can be kept.
    #[allow(clippy::type_complexity)]
    fn reuse_plan(
        &self,
        order: &[(u8, u32, usize, ObjectId)],
        options: &PackOptions,
    ) -> Result<(Vec<ObjectId>, HashMap<ObjectId, (ObjectId, Vec<u8>)>)> {
        let ids: Vec<ObjectId> = order.iter().map(|o| o.3).collect();
        let mut reusable = HashMap::new();
        if options.reuse {
            let members: HashSet<ObjectId> = ids.iter().copied().collect();
            let packs = self.packs();
            for id in &ids {
                for pack in &packs {
                    if let Some((base, delta)) = pack.stored_delta(id)? {
                        if members.contains(&base) {
                            reusable.insert(*id, (base, delta));
                        }
                        break;
                    }
                }
            }
        }
        let mut placed: HashSet<ObjectId> = HashSet::new();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let mut chain = Vec::new();
            let mut cur = id;
            while !placed.contains(&cur) {
                chain.push(cur);
                match reusable.get(&cur) {
                    Some((base, _)) => cur = *base,
                    None => break,
                }
            }
            for c in chain.into_iter().rev() {
                placed.insert(c);
                out.push(c);
            }
        }
        Ok((out, reusable))
    }

    /// Writes `.mtimes` for the pack named `name`, giving each object in index order the time `mtime` returns.
    pub fn write_mtimes(&self, name: &str, mtime: impl Fn(&ObjectId) -> u32) -> Result<()> {
        self.rescan_packs()?;
        let idx = self.dir().join("pack").join(format!("pack-{name}.idx"));
        let pack = self
            .packs()
            .into_iter()
            .find(|p| p.idx_path() == idx)
            .ok_or_else(|| crate::error::invalid("pack", format!("{name} is not in the store")))?;
        let times: Vec<u32> = pack.ids().map(|id| mtime(&id)).collect();
        replace_readonly(&idx.with_extension("mtimes"), &pack.mtimes_file(&times))
    }
}

struct Candidate {
    kind: Kind,
    offset: u64,
    depth: usize,
    data: Vec<u8>,
}

fn rank(kind: Kind) -> u8 {
    match kind {
        Kind::Commit => 0,
        Kind::Tag => 1,
        Kind::Tree => 2,
        Kind::Blob => 3,
    }
}

/// Writes `bytes` to `path` through a temporary file, read-only, replacing what is there.
fn replace_readonly(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp_meta");
    fs::write(&tmp, bytes).map_err(io_at(&tmp))?;
    make_readonly(&tmp).map_err(io_at(&tmp))?;
    fs::rename(&tmp, path).map_err(io_at(path))
}

fn make_readonly(path: &Path) -> std::io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    fs::set_permissions(path, perms)
}

fn make_writable_then_remove(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let mut perms = fs::metadata(path)?.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            fs::set_permissions(path, perms)?;
            fs::remove_file(path)
        }
        Err(e) => Err(e),
    }
}

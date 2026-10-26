use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::Compression;
use flate2::bufread::ZlibDecoder;
use flate2::write::ZlibEncoder;
use pyn_object::{Algorithm, Kind, ObjectHasher, ObjectId, hash_object};

use crate::config::Config;
use crate::delta;
use crate::error::{Error, Result, corrupt, invalid, io_at};
use crate::odb::ObjectDb;
use crate::pack::{Pack, PosReader, Raw, read_entry_header};
use crate::pack_write::{PackIndexEntry, build_idx};
use crate::util::Checksummer;

/// Deepest delta chain accepted when ingesting and when reading a stored pack.
pub const MAX_DELTA_DEPTH: usize = 4096;

/// Bounds on what an incoming pack may ask for.
///
/// Start from [`IngestLimits::default`] and adjust with the `with_*` methods, or read the
/// `pyn.ingest*` keys of a git config with [`IngestLimits::from_config`].
#[derive(Debug, Clone, Copy)]
pub struct IngestLimits {
    pub max_objects: u32,
    /// Largest object, and largest delta, after inflation.
    pub max_object_size: u64,
    pub max_delta_depth: usize,
    pub max_pack_size: u64,
    /// Sum of every inflated entry and every resolved delta result in one ingest.
    pub max_total_inflated: u64,
    /// A delta result may be at most this many times its base, unless under `delta_size_floor`.
    pub max_delta_ratio: u64,
    pub delta_size_floor: u64,
}

impl Default for IngestLimits {
    fn default() -> Self {
        Self {
            max_objects: 20_000_000,
            max_object_size: 256 << 20,
            max_delta_depth: MAX_DELTA_DEPTH,
            max_pack_size: 64 << 30,
            max_total_inflated: 128 << 30,
            max_delta_ratio: 256,
            delta_size_floor: 4 << 20,
        }
    }
}

impl IngestLimits {
    pub fn with_max_objects(mut self, n: u32) -> Self {
        self.max_objects = n;
        self
    }

    pub fn with_max_object_size(mut self, n: u64) -> Self {
        self.max_object_size = n;
        self
    }

    pub fn with_max_delta_depth(mut self, n: usize) -> Self {
        self.max_delta_depth = n;
        self
    }

    pub fn with_max_pack_size(mut self, n: u64) -> Self {
        self.max_pack_size = n;
        self
    }

    pub fn with_max_total_inflated(mut self, n: u64) -> Self {
        self.max_total_inflated = n;
        self
    }

    pub fn with_delta_ratio(mut self, ratio: u64, floor: u64) -> Self {
        self.max_delta_ratio = ratio;
        self.delta_size_floor = floor;
        self
    }

    /// Defaults overridden by `pyn.ingestMaxObjects`, `ingestMaxObjectSize`, `ingestMaxDeltaDepth`,
    /// `ingestMaxPackSize`, `ingestMaxTotalInflated`, `ingestMaxDeltaRatio` and `ingestDeltaSizeFloor`.
    /// Sizes accept a `k`, `m` or `g` suffix.
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut l = Self::default();
        let num = |key: &str| -> Result<Option<u64>> {
            match config.get(key) {
                None => Ok(None),
                Some(v) => parse_count(v)
                    .map(Some)
                    .ok_or_else(|| invalid("config", format!("{key} is not a number: {v:?}"))),
            }
        };
        if let Some(n) = num("pyn.ingestmaxobjects")? {
            l.max_objects = u32::try_from(n).unwrap_or(u32::MAX);
        }
        if let Some(n) = num("pyn.ingestmaxobjectsize")? {
            l.max_object_size = n;
        }
        if let Some(n) = num("pyn.ingestmaxdeltadepth")? {
            l.max_delta_depth = usize::try_from(n).unwrap_or(usize::MAX);
        }
        if let Some(n) = num("pyn.ingestmaxpacksize")? {
            l.max_pack_size = n;
        }
        if let Some(n) = num("pyn.ingestmaxtotalinflated")? {
            l.max_total_inflated = n;
        }
        if let Some(n) = num("pyn.ingestmaxdeltaratio")? {
            l.max_delta_ratio = n;
        }
        if let Some(n) = num("pyn.ingestdeltasizefloor")? {
            l.delta_size_floor = n;
        }
        Ok(l)
    }

    /// Largest result a delta over a base of `base_len` bytes may produce.
    fn delta_cap(&self, base_len: u64) -> u64 {
        base_len
            .saturating_mul(self.max_delta_ratio)
            .max(self.delta_size_floor)
            .min(self.max_object_size)
    }
}

fn parse_count(v: &str) -> Option<u64> {
    let v = v.trim();
    let (digits, mult) = match v.chars().last()?.to_ascii_lowercase() {
        'k' => (&v[..v.len() - 1], 1u64 << 10),
        'm' => (&v[..v.len() - 1], 1 << 20),
        'g' => (&v[..v.len() - 1], 1 << 30),
        _ => (v, 1),
    };
    digits.trim().parse::<u64>().ok()?.checked_mul(mult)
}

/// A pack stored by [`index_pack`] and now visible to the object database.
pub struct IngestedPack {
    pub pack: Arc<Pack>,
    /// Bases found in the object database and appended to complete a thin pack.
    pub thin_bases: usize,
}

impl IngestedPack {
    /// Records that the pack came from a promisor remote, by creating its `.promisor` file.
    pub fn mark_promisor(&self) -> Result<()> {
        let path = self.pack.pack_path().with_extension("promisor");
        fs::write(&path, b"").map_err(io_at(&path))
    }
}

impl std::fmt::Debug for IngestedPack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IngestedPack")
            .field("idx", &self.pack.idx_path())
            .field("thin_bases", &self.thin_bases)
            .finish()
    }
}

/// Buffered reader that hashes, checksums and stores the bytes consumed from it.
struct Tee<R> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    cap: usize,
    consumed: u64,
    max: u64,
    hash: Option<Checksummer>,
    crc: crc32fast::Hasher,
    sink: BufWriter<File>,
    failure: Option<Error>,
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = {
            let avail = self.fill_buf()?;
            let n = avail.len().min(out.len());
            out[..n].copy_from_slice(&avail[..n]);
            n
        };
        self.consume(n);
        Ok(n)
    }
}

impl<R: Read> BufRead for Tee<R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        if self.failure.is_some() {
            return Err(std::io::Error::other("aborted"));
        }
        if self.pos >= self.cap {
            loop {
                match self.inner.read(&mut self.buf) {
                    Ok(n) => {
                        self.cap = n;
                        self.pos = 0;
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(&self.buf[self.pos..self.cap])
    }

    fn consume(&mut self, n: usize) {
        let bytes = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        self.consumed += n as u64;
        if self.consumed > self.max {
            self.failure = Some(Error::Limit {
                what: "pack size",
                limit: self.max,
            });
            return;
        }
        if let Some(h) = &mut self.hash {
            h.update(bytes);
        }
        self.crc.update(bytes);
        if let Err(e) = self.sink.write_all(bytes) {
            self.failure = Some(Error::Write(e));
        }
    }
}

fn map_io(e: std::io::Error) -> Error {
    match e.kind() {
        std::io::ErrorKind::UnexpectedEof => Error::Truncated("pack"),
        std::io::ErrorKind::InvalidData | std::io::ErrorKind::InvalidInput => {
            corrupt("pack", format!("inflate failed: {e}"))
        }
        _ => Error::Read(e),
    }
}

/// Inflates one zlib stream that must hold exactly `size` bytes, passing the output on in chunks.
fn inflate<R: BufRead>(src: R, size: u64, mut f: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
    let mut z = ZlibDecoder::new(src);
    let mut buf = [0u8; 16 * 1024];
    let mut total = 0u64;
    loop {
        let n = z.read(&mut buf).map_err(map_io)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > size {
            return Err(corrupt(
                "pack",
                "inflated size differs from the entry header",
            ));
        }
        f(&buf[..n])?;
    }
    if total != size {
        return Err(corrupt(
            "pack",
            "inflated size differs from the entry header",
        ));
    }
    Ok(())
}

struct Entry {
    offset: u64,
    crc: u32,
    id: Option<ObjectId>,
    kind: Option<Kind>,
}

struct Work {
    idx: usize,
    parent: Rc<Vec<u8>>,
    kind: Kind,
    depth: usize,
}

fn charge(spent: &mut u64, n: u64, limits: &IngestLimits) -> Result<()> {
    *spent = spent.saturating_add(n);
    if *spent > limits.max_total_inflated {
        return Err(Error::Limit {
            what: "total inflated bytes",
            limit: limits.max_total_inflated,
        });
    }
    Ok(())
}

struct Resolver<'a> {
    db: &'a ObjectDb,
    limits: IngestLimits,
    spent: u64,
    file: File,
    entries: Vec<Entry>,
    ids: HashMap<ObjectId, usize>,
    by_idx: HashMap<usize, Vec<usize>>,
    by_id: HashMap<ObjectId, Vec<usize>>,
    stack: Vec<Work>,
}

impl Resolver<'_> {
    fn algorithm(&self) -> Algorithm {
        self.db.algorithm()
    }

    fn load(&self, idx: usize) -> Result<Vec<u8>> {
        let offset = self.entries[idx].offset;
        let mut r = BufReader::with_capacity(
            16 * 1024,
            PosReader {
                file: &self.file,
                pos: offset,
            },
        );
        let (_, size) = read_entry_header(&mut r, offset, self.algorithm())?;
        let mut data = Vec::with_capacity(size.min(64 << 20) as usize);
        inflate(&mut r, size, |c| {
            data.extend_from_slice(c);
            Ok(())
        })?;
        Ok(data)
    }

    fn record(&mut self, idx: usize, id: ObjectId, kind: Kind) -> Result<()> {
        if self.ids.insert(id, idx).is_some() {
            return Err(corrupt("pack", format!("object {id} appears twice")));
        }
        self.entries[idx].id = Some(id);
        self.entries[idx].kind = Some(kind);
        Ok(())
    }

    /// Queues the deltas that name entry `idx` or object `id` as their base.
    fn expand(
        &mut self,
        idx: usize,
        id: ObjectId,
        kind: Kind,
        data: Option<Rc<Vec<u8>>>,
        depth: usize,
    ) -> Result<()> {
        let mut kids = self.by_idx.remove(&idx).unwrap_or_default();
        kids.extend(self.by_id.remove(&id).unwrap_or_default());
        if kids.is_empty() {
            return Ok(());
        }
        let data = match data {
            Some(d) => d,
            None => Rc::new(self.load(idx)?),
        };
        for k in kids {
            self.stack.push(Work {
                idx: k,
                parent: Rc::clone(&data),
                kind,
                depth,
            });
        }
        Ok(())
    }

    fn drain(&mut self) -> Result<()> {
        while let Some(w) = self.stack.pop() {
            if w.depth > self.limits.max_delta_depth {
                return Err(Error::Limit {
                    what: "delta chain depth",
                    limit: self.limits.max_delta_depth as u64,
                });
            }
            let d = self.load(w.idx)?;
            let (_, dst) = delta::sizes(&d)?;
            let cap = self.limits.delta_cap(w.parent.len() as u64);
            if dst > cap {
                return Err(Error::Limit {
                    what: "delta result size",
                    limit: cap,
                });
            }
            charge(&mut self.spent, dst, &self.limits)?;
            let data = delta::apply_capped(&w.parent, &d, cap)?;
            let id = hash_object(self.algorithm(), w.kind, &data)?;
            self.record(w.idx, id, w.kind)?;
            self.expand(w.idx, id, w.kind, Some(Rc::new(data)), w.depth + 1)?;
        }
        Ok(())
    }
}

fn write_entry(file: &mut File, kind: Kind, data: &[u8]) -> Result<(u64, u32)> {
    let type_bits: u8 = match kind {
        Kind::Commit => 1,
        Kind::Tree => 2,
        Kind::Blob => 3,
        Kind::Tag => 4,
    };
    let mut bytes = Vec::new();
    let mut size = data.len() as u64;
    let mut byte = (type_bits << 4) | (size & 0x0f) as u8;
    size >>= 4;
    while size != 0 {
        bytes.push(byte | 0x80);
        byte = (size & 0x7f) as u8;
        size >>= 7;
    }
    bytes.push(byte);
    let mut z = ZlibEncoder::new(bytes, Compression::default());
    z.write_all(data).map_err(Error::Write)?;
    let bytes = z.finish().map_err(Error::Write)?;
    let at = file.stream_position().map_err(Error::Write)?;
    file.write_all(&bytes).map_err(Error::Write)?;
    Ok((at, crc32fast::hash(&bytes)))
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_name(dir: &Path, what: &str) -> PathBuf {
    dir.join(format!(
        "tmp_{what}_{}_{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Verifies and stores a pack from `input` with its index in `objects/pack`, completing a thin pack from `db`.
/// On error nothing is left behind.
pub fn index_pack<R: Read>(db: &ObjectDb, input: R, limits: &IngestLimits) -> Result<IngestedPack> {
    let pack_dir = db.dir().join("pack");
    fs::create_dir_all(&pack_dir).map_err(io_at(&pack_dir))?;
    let tmp = tmp_name(&pack_dir, "pack");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(io_at(&tmp))?;
    let result = ingest(db, input, limits, &pack_dir, &tmp, file);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn ingest<R: Read>(
    db: &ObjectDb,
    input: R,
    limits: &IngestLimits,
    pack_dir: &Path,
    tmp: &Path,
    file: File,
) -> Result<IngestedPack> {
    let algorithm = db.algorithm();
    let hl = algorithm.digest_len();
    let mut tee = Tee {
        inner: input,
        buf: vec![0u8; 64 * 1024].into_boxed_slice(),
        pos: 0,
        cap: 0,
        consumed: 0,
        max: limits.max_pack_size,
        hash: Some(Checksummer::new(algorithm)),
        crc: crc32fast::Hasher::new(),
        sink: BufWriter::new(file.try_clone().map_err(io_at(tmp))?),
        failure: None,
    };
    let mut spent = 0u64;
    let streamed = stream(&mut tee, algorithm, limits, &mut spent);
    let parsed = match streamed {
        Ok(p) => match tee.failure.take() {
            Some(e) => return Err(e),
            None => p,
        },
        Err(e) => return Err(tee.failure.take().unwrap_or(e)),
    };
    tee.sink.flush().map_err(Error::Write)?;
    drop(tee.sink);
    let Parsed {
        mut entries,
        by_idx,
        by_id,
        ids,
        count,
        trailer,
    } = parsed;
    debug_assert_eq!(entries.len() as u32, count);
    let body_end = tee.consumed - hl as u64;

    let mut r = Resolver {
        db,
        limits: *limits,
        spent,
        file,
        entries: Vec::new(),
        ids,
        by_idx,
        by_id,
        stack: Vec::new(),
    };
    std::mem::swap(&mut r.entries, &mut entries);
    let roots: Vec<usize> = (0..r.entries.len())
        .filter(|i| r.entries[*i].id.is_some())
        .collect();
    for i in roots {
        let (id, kind) = (r.entries[i].id.unwrap(), r.entries[i].kind.unwrap());
        r.expand(i, id, kind, None, 1)?;
        r.drain()?;
    }

    let mut thin = 0usize;
    let mut end = body_end;
    // A base may itself be a delta waiting on another base, so retry until a pass makes no progress.
    loop {
        let mut keys: Vec<ObjectId> = r.by_id.keys().copied().collect();
        keys.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        let mut progress = false;
        for base in keys {
            if !r.by_id.contains_key(&base) {
                continue;
            }
            let Some(object) = r.db.read_opt(&base)? else {
                continue;
            };
            if object.data.len() as u64 > limits.max_object_size {
                return Err(Error::Limit {
                    what: "object size",
                    limit: limits.max_object_size,
                });
            }
            charge(&mut r.spent, object.data.len() as u64, limits)?;
            r.file.seek(SeekFrom::Start(end)).map_err(io_at(tmp))?;
            let (offset, crc) = write_entry(&mut r.file, object.kind, &object.data)?;
            end = r.file.stream_position().map_err(io_at(tmp))?;
            let idx = r.entries.len();
            r.entries.push(Entry {
                offset,
                crc,
                id: None,
                kind: None,
            });
            r.record(idx, base, object.kind)?;
            thin += 1;
            progress = true;
            r.expand(idx, base, object.kind, Some(Rc::new(object.data)), 1)?;
            r.drain()?;
        }
        if !progress {
            break;
        }
    }
    if let Some(missing) = r.by_id.keys().min_by(|a, b| a.as_bytes().cmp(b.as_bytes())) {
        return Err(Error::ObjectNotFound(*missing));
    }
    let unresolved = r.entries.iter().filter(|e| e.id.is_none()).count();
    if unresolved > 0 {
        return Err(corrupt(
            "pack",
            format!("{unresolved} deltas have an unresolved or cyclic base"),
        ));
    }

    let checksum = if thin > 0 {
        r.file.set_len(end).map_err(io_at(tmp))?;
        let total = u32::try_from(r.entries.len()).map_err(|_| Error::Limit {
            what: "pack object count",
            limit: u64::from(u32::MAX),
        })?;
        r.file.seek(SeekFrom::Start(8)).map_err(io_at(tmp))?;
        r.file.write_all(&total.to_be_bytes()).map_err(io_at(tmp))?;
        let sum = rehash(&mut r.file, end, algorithm, tmp)?;
        r.file.seek(SeekFrom::Start(end)).map_err(io_at(tmp))?;
        r.file.write_all(&sum).map_err(io_at(tmp))?;
        sum
    } else {
        trailer
    };
    r.file.sync_all().map_err(io_at(tmp))?;
    let mut perms = r.file.metadata().map_err(io_at(tmp))?.permissions();
    perms.set_readonly(true);
    r.file.set_permissions(perms).map_err(io_at(tmp))?;

    let index: Vec<PackIndexEntry> = r
        .entries
        .iter()
        .map(|e| PackIndexEntry {
            id: e.id.expect("every entry resolved"),
            crc32: e.crc,
            offset: e.offset,
        })
        .collect();
    let idx_bytes = build_idx(algorithm, &index, &checksum);
    let stem = format!("pack-{}", hex_of(&checksum));
    let pack_path = pack_dir.join(format!("{stem}.pack"));
    let idx_path = pack_dir.join(format!("{stem}.idx"));
    let idx_tmp = tmp_name(pack_dir, "idx");
    let stored = (|| -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&idx_tmp)?;
        f.write_all(&idx_bytes)?;
        f.sync_all()?;
        let mut perms = f.metadata()?.permissions();
        perms.set_readonly(true);
        f.set_permissions(perms)?;
        fs::rename(tmp, &pack_path)?;
        fs::rename(&idx_tmp, &idx_path)
    })();
    if let Err(e) = stored {
        let _ = fs::remove_file(&idx_tmp);
        return Err(io_at(&idx_path)(e));
    }
    db.rescan_packs()?;
    let pack = db
        .packs()
        .into_iter()
        .find(|p| p.idx_path() == idx_path)
        .ok_or_else(|| corrupt("pack", "stored pack was not picked up"))?;
    Ok(IngestedPack {
        pack,
        thin_bases: thin,
    })
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn rehash(file: &mut File, end: u64, algorithm: Algorithm, tmp: &Path) -> Result<Vec<u8>> {
    let mut h = Checksummer::new(algorithm);
    file.seek(SeekFrom::Start(0)).map_err(io_at(tmp))?;
    let mut left = end;
    let mut buf = vec![0u8; 64 * 1024];
    while left > 0 {
        let n = (left as usize).min(buf.len());
        file.read_exact(&mut buf[..n]).map_err(io_at(tmp))?;
        h.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(h.finish())
}

struct Parsed {
    entries: Vec<Entry>,
    by_idx: HashMap<usize, Vec<usize>>,
    by_id: HashMap<ObjectId, Vec<usize>>,
    ids: HashMap<ObjectId, usize>,
    count: u32,
    trailer: Vec<u8>,
}

/// Parses the header and every entry from the stream, then verifies the trailer.
fn stream<R: Read>(
    tee: &mut Tee<R>,
    algorithm: Algorithm,
    limits: &IngestLimits,
    spent: &mut u64,
) -> Result<Parsed> {
    let io = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Truncated("pack header")
        } else {
            Error::Read(e)
        }
    };
    let mut header = [0u8; 12];
    tee.read_exact(&mut header).map_err(io)?;
    if &header[..4] != b"PACK" {
        return Err(corrupt("pack", "bad signature"));
    }
    let version = u32::from_be_bytes(header[4..8].try_into().unwrap());
    if version != 2 && version != 3 {
        return Err(Error::Unsupported(format!("pack version {version}")));
    }
    let count = u32::from_be_bytes(header[8..12].try_into().unwrap());
    if count > limits.max_objects {
        return Err(Error::Limit {
            what: "pack object count",
            limit: u64::from(limits.max_objects),
        });
    }
    let mut p = Parsed {
        entries: Vec::with_capacity((count as usize).min(1 << 20)),
        by_idx: HashMap::new(),
        by_id: HashMap::new(),
        ids: HashMap::new(),
        count,
        trailer: Vec::new(),
    };
    let mut by_offset: HashMap<u64, usize> = HashMap::new();
    for i in 0..count as usize {
        let offset = tee.consumed;
        tee.crc = crc32fast::Hasher::new();
        let (raw, size) = read_entry_header(tee, offset, algorithm)?;
        if size > limits.max_object_size {
            return Err(Error::Limit {
                what: "object size",
                limit: limits.max_object_size,
            });
        }
        charge(spent, size, limits)?;
        let mut id = None;
        let mut kind = None;
        match raw {
            Raw::Base(k) => {
                let mut h = ObjectHasher::new(algorithm, k, size);
                inflate(&mut *tee, size, |c| Ok(h.update(c)?))?;
                let oid = h.finish()?;
                if p.ids.insert(oid, i).is_some() {
                    return Err(corrupt("pack", format!("object {oid} appears twice")));
                }
                id = Some(oid);
                kind = Some(k);
            }
            Raw::Ofs(base) => {
                let b = *by_offset
                    .get(&base)
                    .ok_or_else(|| corrupt("pack", "delta base is not an entry of the pack"))?;
                p.by_idx.entry(b).or_default().push(i);
                inflate(&mut *tee, size, |_| Ok(()))?;
            }
            Raw::Ref(base) => {
                p.by_id.entry(base).or_default().push(i);
                inflate(&mut *tee, size, |_| Ok(()))?;
            }
        }
        by_offset.insert(offset, i);
        p.entries.push(Entry {
            offset,
            crc: tee.crc.clone().finalize(),
            id,
            kind,
        });
    }
    let expected = tee.hash.take().expect("hashing until the trailer").finish();
    let mut trailer = vec![0u8; algorithm.digest_len()];
    tee.read_exact(&mut trailer).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Truncated("pack trailer")
        } else {
            Error::Read(e)
        }
    })?;
    if trailer != expected {
        return Err(Error::Checksum { what: "pack" });
    }
    p.trailer = trailer;
    Ok(p)
}

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;
use pyn_object::{Algorithm, Kind, ObjectId, hash_object};

use crate::Object;
use crate::delta;
use crate::error::{Error, Result, corrupt, io_at};
use crate::util::{Checksummer, checksum};

const IDX_MAGIC: [u8; 4] = [0xff, 0x74, 0x4f, 0x63];
const MAX_DELTA_DEPTH: usize = 10_000;

/// How an object is stored inside a pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Base(Kind),
    OfsDelta,
    RefDelta,
}

pub(crate) enum Raw {
    Base(Kind),
    Ofs(u64),
    Ref(ObjectId),
}

/// A pack file with its version 2 index.
pub struct Pack {
    algorithm: Algorithm,
    pack_path: PathBuf,
    idx_path: PathBuf,
    file: File,
    idx: Vec<u8>,
    count: usize,
    pack_len: u64,
}

pub(crate) struct PosReader<'a> {
    pub(crate) file: &'a File,
    pub(crate) pos: u64,
}

impl Read for PosReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = read_at(self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, pos)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, pos)
}

impl Pack {
    /// Opens `<name>.idx` and `<name>.pack` and checks their headers and that both name the same pack checksum.
    pub fn open(idx_path: &Path, algorithm: Algorithm) -> Result<Self> {
        let pack_path = idx_path.with_extension("pack");
        let idx = std::fs::read(idx_path).map_err(io_at(idx_path))?;
        let hl = algorithm.digest_len();
        let min = 8 + 256 * 4 + 2 * hl;
        if idx.len() < min {
            return Err(corrupt("pack index", "truncated"));
        }
        if idx[..4] != IDX_MAGIC {
            return Err(Error::Unsupported("pack index version 1".into()));
        }
        let version = be32(&idx[4..]);
        if version != 2 {
            return Err(Error::Unsupported(format!("pack index version {version}")));
        }
        let count = be32(&idx[8 + 255 * 4..]) as usize;
        let fixed = min
            .checked_add(
                count
                    .checked_mul(hl + 8)
                    .ok_or_else(|| corrupt("pack index", "count"))?,
            )
            .ok_or_else(|| corrupt("pack index", "count"))?;
        if idx.len() < fixed {
            return Err(corrupt("pack index", "truncated tables"));
        }
        let mut prev = 0;
        for i in 0..256 {
            let v = be32(&idx[8 + i * 4..]);
            if v < prev || v as usize > count {
                return Err(corrupt("pack index", "fan-out table is not monotonic"));
            }
            prev = v;
        }

        let file = File::open(&pack_path).map_err(io_at(&pack_path))?;
        let pack_len = file.metadata().map_err(io_at(&pack_path))?.len();
        let mut header = [0u8; 12];
        read_exact_at(&file, &mut header, 0, &pack_path)?;
        if &header[..4] != b"PACK" {
            return Err(corrupt("pack", "bad signature"));
        }
        let pack_version = be32(&header[4..]);
        if pack_version != 2 && pack_version != 3 {
            return Err(Error::Unsupported(format!("pack version {pack_version}")));
        }
        if be32(&header[8..]) as usize != count {
            return Err(corrupt("pack", "object count differs from the index"));
        }
        if pack_len < (12 + hl) as u64 {
            return Err(corrupt("pack", "truncated"));
        }
        let mut trailer = vec![0u8; hl];
        read_exact_at(&file, &mut trailer, pack_len - hl as u64, &pack_path)?;
        if trailer != idx[idx.len() - 2 * hl..idx.len() - hl] {
            return Err(corrupt(
                "pack",
                "trailer differs from the index (truncated or replaced)",
            ));
        }
        Ok(Self {
            algorithm,
            pack_path,
            idx_path: idx_path.to_path_buf(),
            file,
            idx,
            count,
            pack_len,
        })
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn pack_path(&self) -> &Path {
        &self.pack_path
    }

    pub fn idx_path(&self) -> &Path {
        &self.idx_path
    }

    fn hl(&self) -> usize {
        self.algorithm.digest_len()
    }

    fn ids_start(&self) -> usize {
        8 + 256 * 4
    }

    fn id_at(&self, i: usize) -> &[u8] {
        let s = self.ids_start() + i * self.hl();
        &self.idx[s..s + self.hl()]
    }

    fn crc_at(&self, i: usize) -> u32 {
        be32(&self.idx[self.ids_start() + self.count * self.hl() + i * 4..])
    }

    fn offset_at(&self, i: usize) -> Result<u64> {
        let base = self.ids_start() + self.count * (self.hl() + 4);
        let raw = be32(&self.idx[base + i * 4..]);
        if raw & 0x8000_0000 == 0 {
            return Ok(u64::from(raw));
        }
        let slot = (raw & 0x7fff_ffff) as usize;
        let large = base + self.count * 4 + slot * 8;
        let end = large + 8;
        if end > self.idx.len() - 2 * self.hl() {
            return Err(corrupt("pack index", "large offset out of range"));
        }
        Ok(u64::from_be_bytes(self.idx[large..end].try_into().unwrap()))
    }

    /// Position of `id` in the sorted id table.
    fn find(&self, id: &ObjectId) -> Option<usize> {
        if id.algorithm() != self.algorithm {
            return None;
        }
        let want = id.as_bytes();
        let first = want[0] as usize;
        let hi = be32(&self.idx[8 + first * 4..]) as usize;
        let lo = if first == 0 {
            0
        } else {
            be32(&self.idx[8 + (first - 1) * 4..]) as usize
        };
        let (mut lo, mut hi) = (lo, hi);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.id_at(mid).cmp(want) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }

    pub fn contains(&self, id: &ObjectId) -> bool {
        self.find(id).is_some()
    }

    pub fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        (0..self.count)
            .map(|i| ObjectId::from_bytes(self.algorithm, self.id_at(i)).expect("length checked"))
    }

    /// How `id` is stored, without resolving deltas.
    pub fn entry_kind(&self, id: &ObjectId) -> Result<Option<EntryKind>> {
        let Some(i) = self.find(id) else {
            return Ok(None);
        };
        let (raw, _) = self.read_raw(self.offset_at(i)?)?;
        Ok(Some(match raw {
            Raw::Base(k) => EntryKind::Base(k),
            Raw::Ofs(_) => EntryKind::OfsDelta,
            Raw::Ref(_) => EntryKind::RefDelta,
        }))
    }

    /// Reads and fully resolves `id`, verifying its hash.
    pub fn read(&self, id: &ObjectId) -> Result<Option<Object>> {
        let Some(i) = self.find(id) else {
            return Ok(None);
        };
        let object = self.read_at(self.offset_at(i)?)?;
        if hash_object(self.algorithm, object.kind, &object.data)? != *id {
            return Err(corrupt(
                "pack",
                format!("object {id} does not match its hash"),
            ));
        }
        Ok(Some(object))
    }

    fn read_at(&self, offset: u64) -> Result<Object> {
        let mut chain = Vec::new();
        let mut at = offset;
        let (kind, mut data) = loop {
            let (raw, body) = self.read_raw(at)?;
            match raw {
                Raw::Base(kind) => break (kind, body),
                Raw::Ofs(base) => {
                    if base >= at {
                        return Err(corrupt("pack", "delta base is not earlier in the pack"));
                    }
                    chain.push(body);
                    at = base;
                }
                Raw::Ref(base) => {
                    chain.push(body);
                    let i = self.find(&base).ok_or(Error::ObjectNotFound(base))?;
                    at = self.offset_at(i)?;
                }
            }
            if chain.len() > MAX_DELTA_DEPTH {
                return Err(corrupt("pack", "delta chain too deep or cyclic"));
            }
        };
        for d in chain.iter().rev() {
            data = delta::apply(&data, d)?;
        }
        Ok(Object { kind, data })
    }

    /// Parses the entry header at `offset` and inflates its payload (the delta bytes for a delta entry).
    fn read_raw(&self, offset: u64) -> Result<(Raw, Vec<u8>)> {
        if offset < 12 || offset >= self.pack_len - self.hl() as u64 {
            return Err(corrupt("pack", "entry offset out of range"));
        }
        let mut r = BufReader::with_capacity(
            4096,
            PosReader {
                file: &self.file,
                pos: offset,
            },
        );
        let (raw, size) =
            read_entry_header(&mut r, offset, self.algorithm).map_err(|e| match e {
                Error::Truncated(_) => corrupt("pack", "truncated entry"),
                Error::Read(e) => corrupt("pack", format!("truncated entry: {e}")),
                e => e,
            })?;
        let mut data = Vec::with_capacity(size.min(64 << 20) as usize);
        ZlibDecoder::new(&mut r)
            .take(size + 1)
            .read_to_end(&mut data)
            .map_err(|e| corrupt("pack", format!("inflate failed: {e}")))?;
        if data.len() as u64 != size {
            return Err(corrupt(
                "pack",
                "inflated size differs from the entry header",
            ));
        }
        Ok((raw, data))
    }

    /// Checks both file checksums and every entry's CRC32 against the index.
    pub fn verify(&self) -> Result<()> {
        let hl = self.hl();
        let idx_body = self.idx.len() - hl;
        if checksum(self.algorithm, &self.idx[..idx_body]) != self.idx[idx_body..] {
            return Err(Error::Checksum { what: "pack index" });
        }
        let mut hasher = Checksummer::new(self.algorithm);
        let body_len = self.pack_len - hl as u64;
        let mut pos = 0u64;
        let mut buf = vec![0u8; 1 << 16];
        while pos < body_len {
            let n = ((body_len - pos) as usize).min(buf.len());
            read_exact_at(&self.file, &mut buf[..n], pos, &self.pack_path)?;
            hasher.update(&buf[..n]);
            pos += n as u64;
        }
        let mut trailer = vec![0u8; hl];
        read_exact_at(&self.file, &mut trailer, body_len, &self.pack_path)?;
        if hasher.finish() != trailer {
            return Err(Error::Checksum { what: "pack" });
        }

        let mut spans: Vec<(u64, usize)> = Vec::with_capacity(self.count);
        for i in 0..self.count {
            spans.push((self.offset_at(i)?, i));
        }
        spans.sort_unstable();
        for (n, &(start, i)) in spans.iter().enumerate() {
            let end = spans.get(n + 1).map_or(body_len, |s| s.0);
            if end <= start || end > body_len {
                return Err(corrupt("pack", "entry offsets overlap or leave the pack"));
            }
            let mut bytes = vec![0u8; (end - start) as usize];
            read_exact_at(&self.file, &mut bytes, start, &self.pack_path)?;
            if crc32fast::hash(&bytes) != self.crc_at(i) {
                return Err(Error::Checksum { what: "pack entry" });
            }
        }
        Ok(())
    }
}

/// Parses the type, size and delta base of the entry that starts at `offset` in a pack.
pub(crate) fn read_entry_header<R: Read>(
    r: &mut R,
    offset: u64,
    algorithm: Algorithm,
) -> Result<(Raw, u64)> {
    let io = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Error::Truncated("pack entry")
        } else {
            Error::Read(e)
        }
    };
    let mut byte = [0u8; 1];
    r.read_exact(&mut byte).map_err(io)?;
    let type_bits = (byte[0] >> 4) & 7;
    let mut size = u64::from(byte[0] & 0x0f);
    let mut shift = 4;
    let mut b = byte[0];
    while b & 0x80 != 0 {
        r.read_exact(&mut byte).map_err(io)?;
        b = byte[0];
        if shift > 57 {
            return Err(corrupt("pack", "entry size overflow"));
        }
        size |= u64::from(b & 0x7f) << shift;
        shift += 7;
    }
    let raw = match type_bits {
        1 => Raw::Base(Kind::Commit),
        2 => Raw::Base(Kind::Tree),
        3 => Raw::Base(Kind::Blob),
        4 => Raw::Base(Kind::Tag),
        6 => {
            r.read_exact(&mut byte).map_err(io)?;
            let mut c = byte[0];
            let mut back = u64::from(c & 0x7f);
            while c & 0x80 != 0 {
                r.read_exact(&mut byte).map_err(io)?;
                c = byte[0];
                back = back
                    .checked_add(1)
                    .and_then(|v| v.checked_mul(128))
                    .ok_or_else(|| corrupt("pack", "offset overflow"))?
                    + u64::from(c & 0x7f);
            }
            Raw::Ofs(
                offset
                    .checked_sub(back)
                    .filter(|b| *b < offset)
                    .ok_or_else(|| corrupt("pack", "delta base before the pack"))?,
            )
        }
        7 => {
            let mut raw_id = vec![0u8; algorithm.digest_len()];
            r.read_exact(&mut raw_id).map_err(io)?;
            Raw::Ref(ObjectId::from_bytes(algorithm, &raw_id)?)
        }
        t => return Err(corrupt("pack", format!("unknown entry type {t}"))),
    };
    Ok((raw, size))
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

fn read_exact_at(file: &File, buf: &mut [u8], mut pos: u64, path: &Path) -> Result<()> {
    let mut done = 0;
    while done < buf.len() {
        let n = read_at(file, &mut buf[done..], pos).map_err(io_at(path))?;
        if n == 0 {
            return Err(corrupt("pack", "unexpected end of file"));
        }
        done += n;
        pos += n as u64;
    }
    Ok(())
}

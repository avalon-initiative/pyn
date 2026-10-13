use std::collections::HashSet;
use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use pyn_object::{Algorithm, Kind, ObjectId};

use crate::error::{Error, Result, invalid};
use crate::odb::ObjectDb;
use crate::util::{Checksummer, checksum};

const IDX_MAGIC: [u8; 8] = [0xff, 0x74, 0x4f, 0x63, 0, 0, 0, 2];

/// Where one object sits in a pack and the CRC32 of its stored bytes: one row of a pack index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackIndexEntry {
    pub id: ObjectId,
    pub crc32: u32,
    pub offset: u64,
}

/// Builds a version 2 pack index for the entries of the pack whose trailer is `pack_checksum`.
pub(crate) fn build_idx(
    algorithm: Algorithm,
    entries: &[PackIndexEntry],
    pack_checksum: &[u8],
) -> Vec<u8> {
    let mut sorted: Vec<&PackIndexEntry> = entries.iter().collect();
    sorted.sort_unstable_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    let mut out = Vec::with_capacity(8 + 1024 + sorted.len() * (algorithm.digest_len() + 12));
    out.extend_from_slice(&IDX_MAGIC);
    let mut fanout = [0u32; 256];
    for e in &sorted {
        fanout[usize::from(e.id.as_bytes()[0])] += 1;
    }
    let mut total = 0;
    for n in fanout {
        total += n;
        out.extend_from_slice(&total.to_be_bytes());
    }
    for e in &sorted {
        out.extend_from_slice(e.id.as_bytes());
    }
    for e in &sorted {
        out.extend_from_slice(&e.crc32.to_be_bytes());
    }
    let mut large = Vec::new();
    for e in &sorted {
        if e.offset < 0x8000_0000 {
            out.extend_from_slice(&(e.offset as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&(0x8000_0000 | large.len() as u32).to_be_bytes());
            large.push(e.offset);
        }
    }
    for o in large {
        out.extend_from_slice(&o.to_be_bytes());
    }
    out.extend_from_slice(pack_checksum);
    let sum = checksum(algorithm, &out);
    out.extend_from_slice(&sum);
    out
}

/// The encoded form of one pack entry; new storage forms (deltas) are new variants.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum PackEntry<'a> {
    Whole { kind: Kind, data: &'a [u8] },
}

struct HashingWriter<W> {
    inner: W,
    hasher: Checksummer,
    written: u64,
}

impl<W: Write> HashingWriter<W> {
    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        self.inner.write_all(bytes).map_err(write_err)?;
        self.hasher.update(bytes);
        self.written += bytes.len() as u64;
        Ok(())
    }
}

fn write_err(e: std::io::Error) -> Error {
    Error::Write(e)
}

fn entry_header(type_bits: u8, size: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(10);
    let mut byte = (type_bits << 4) | (size & 0x0f) as u8;
    let mut rest = size >> 4;
    while rest != 0 {
        out.push(byte | 0x80);
        byte = (rest & 0x7f) as u8;
        rest >>= 7;
    }
    out.push(byte);
    out
}

/// Writes a version 2 pack to `out` one entry at a time, for a count known up front.
pub struct PackWriter<W: Write> {
    out: HashingWriter<W>,
    algorithm: Algorithm,
    expected: u32,
    entries: Vec<PackIndexEntry>,
}

/// What a finished pack contains.
#[derive(Debug, Clone)]
pub struct PackSummary {
    algorithm: Algorithm,
    pub checksum: ObjectId,
    pub entries: Vec<PackIndexEntry>,
}

impl PackSummary {
    /// The version 2 `.idx` that matches the pack.
    pub fn index(&self) -> Vec<u8> {
        build_idx(self.algorithm, &self.entries, self.checksum.as_bytes())
    }
}

impl<W: Write> PackWriter<W> {
    pub fn new(out: W, algorithm: Algorithm, count: u32) -> Result<Self> {
        let mut w = Self {
            out: HashingWriter {
                inner: out,
                hasher: Checksummer::new(algorithm),
                written: 0,
            },
            algorithm,
            expected: count,
            entries: Vec::with_capacity(count as usize),
        };
        w.out.put(b"PACK")?;
        w.out.put(&2u32.to_be_bytes())?;
        w.out.put(&count.to_be_bytes())?;
        Ok(w)
    }

    /// Appends `entry` as object `id` and returns its offset in the pack. `id` is not re-hashed.
    pub fn add(&mut self, id: ObjectId, entry: PackEntry<'_>) -> Result<u64> {
        if self.entries.len() as u32 >= self.expected {
            return Err(invalid("pack", "more objects than the header declares"));
        }
        if id.algorithm() != self.algorithm {
            return Err(invalid(
                "object id",
                format!("{id:?} in a different format"),
            ));
        }
        let PackEntry::Whole { kind, data } = entry;
        let type_bits = match kind {
            Kind::Commit => 1,
            Kind::Tree => 2,
            Kind::Blob => 3,
            Kind::Tag => 4,
        };
        let mut bytes = entry_header(type_bits, data.len() as u64);
        let mut z = ZlibEncoder::new(bytes, Compression::default());
        z.write_all(data).map_err(write_err)?;
        bytes = z.finish().map_err(write_err)?;
        let offset = self.out.written;
        self.out.put(&bytes)?;
        self.entries.push(PackIndexEntry {
            id,
            crc32: crc32fast::hash(&bytes),
            offset,
        });
        Ok(offset)
    }

    pub fn finish(mut self) -> Result<PackSummary> {
        if self.entries.len() as u32 != self.expected {
            return Err(invalid("pack", "fewer objects than the header declares"));
        }
        let sum = self.out.hasher.finish();
        self.out.inner.write_all(&sum).map_err(write_err)?;
        self.out.inner.flush().map_err(write_err)?;
        Ok(PackSummary {
            algorithm: self.algorithm,
            checksum: ObjectId::from_bytes(self.algorithm, &sum)?,
            entries: self.entries,
        })
    }
}

/// Writes the objects `ids` (duplicates dropped, order kept) from `db` as one undeltified pack.
pub fn write_pack<W: Write>(db: &ObjectDb, ids: &[ObjectId], out: W) -> Result<PackSummary> {
    let mut seen = HashSet::with_capacity(ids.len());
    let unique: Vec<ObjectId> = ids.iter().copied().filter(|i| seen.insert(*i)).collect();
    let count = u32::try_from(unique.len()).map_err(|_| Error::Limit {
        what: "pack object count",
        limit: u64::from(u32::MAX),
    })?;
    let mut w = PackWriter::new(out, db.algorithm(), count)?;
    for id in unique {
        let o = db.read(&id)?;
        w.add(
            id,
            PackEntry::Whole {
                kind: o.kind,
                data: &o.data,
            },
        )?;
    }
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> ObjectId {
        ObjectId::from_bytes(Algorithm::Sha1, &[byte; 20]).unwrap()
    }

    #[test]
    fn entry_headers_use_git_varints() {
        assert_eq!(entry_header(3, 5), [0x35]);
        assert_eq!(entry_header(3, 16), [0xb0, 0x01]);
        assert_eq!(entry_header(1, 0x1234), [0x94, 0xa3, 0x02]);
    }

    #[test]
    fn index_sorts_and_spills_large_offsets() {
        let entries = [
            PackIndexEntry {
                id: id(9),
                crc32: 1,
                offset: 12,
            },
            PackIndexEntry {
                id: id(3),
                crc32: 2,
                offset: 5 << 30,
            },
        ];
        let idx = build_idx(Algorithm::Sha1, &entries, &[7; 20]);
        let fan = 8 + 256 * 4;
        assert_eq!(&idx[..8], &IDX_MAGIC);
        assert_eq!(&idx[8 + 255 * 4..fan], &2u32.to_be_bytes());
        assert_eq!(&idx[fan..fan + 20], &[3; 20]);
        let offsets = fan + 2 * 20 + 2 * 4;
        assert_eq!(&idx[offsets..offsets + 4], &0x8000_0000u32.to_be_bytes());
        assert_eq!(&idx[offsets + 4..offsets + 8], &12u32.to_be_bytes());
        assert_eq!(&idx[offsets + 8..offsets + 16], &(5u64 << 30).to_be_bytes());
        assert_eq!(idx.len(), offsets + 16 + 40);
    }
}

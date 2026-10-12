use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use pyn_object::{Algorithm, ObjectId};

use crate::error::{Error, Result, corrupt, invalid, io_at};
use crate::util::checksum;

const FLAG_ASSUME_VALID: u16 = 0x8000;
const FLAG_EXTENDED: u16 = 0x4000;
const FLAG_STAGE_SHIFT: u16 = 12;
const FLAG_NAME_MASK: u16 = 0x0fff;
const EXT_SKIP_WORKTREE: u16 = 0x4000;
const EXT_INTENT_TO_ADD: u16 = 0x2000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timestamp {
    pub seconds: u32,
    pub nanos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub ctime: Timestamp,
    pub mtime: Timestamp,
    pub dev: u32,
    pub ino: u32,
    /// The full mode word: object type in the high bits, permissions in the low ones (for example `0o100644`).
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u32,
    pub id: ObjectId,
    /// 0 for a normal entry, 1 to 3 for the sides of a conflict.
    pub stage: u8,
    pub assume_valid: bool,
    pub skip_worktree: bool,
    pub intent_to_add: bool,
    pub path: Vec<u8>,
}

impl IndexEntry {
    pub fn new(path: impl Into<Vec<u8>>, mode: u32, id: ObjectId) -> Self {
        Self {
            ctime: Timestamp::default(),
            mtime: Timestamp::default(),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            size: 0,
            id,
            stage: 0,
            assume_valid: false,
            skip_worktree: false,
            intent_to_add: false,
            path: path.into(),
        }
    }

    fn needs_extended(&self) -> bool {
        self.skip_worktree || self.intent_to_add
    }
}

/// The index file. Cache extensions (TREE, UNTR, ...) are skipped on read and never written; split and
/// sparse indexes are refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// 2, 3 or 4. Writing upgrades 2 to 3 when an entry needs extended flags.
    pub version: u32,
    pub entries: Vec<IndexEntry>,
}

impl Index {
    pub fn new(version: u32) -> Self {
        Self {
            version,
            entries: Vec::new(),
        }
    }

    pub fn read(path: &Path, algorithm: Algorithm) -> Result<Self> {
        let bytes = fs::read(path).map_err(io_at(path))?;
        Self::parse(&bytes, algorithm)
    }

    pub fn parse(bytes: &[u8], algorithm: Algorithm) -> Result<Self> {
        let hl = algorithm.digest_len();
        if bytes.len() < 12 + hl {
            return Err(corrupt("index", "truncated"));
        }
        let (body, trailer) = bytes.split_at(bytes.len() - hl);
        if checksum(algorithm, body) != trailer {
            return Err(Error::Checksum { what: "index" });
        }
        if &body[..4] != b"DIRC" {
            return Err(corrupt("index", "bad signature"));
        }
        let version = be32(&body[4..]);
        if !(2..=4).contains(&version) {
            return Err(Error::Unsupported(format!("index version {version}")));
        }
        let count = be32(&body[8..]) as usize;
        let mut pos = 12;
        let mut entries: Vec<IndexEntry> = Vec::with_capacity(count.min(1 << 20));
        for _ in 0..count {
            let prev = entries.last().map(|e| e.path.as_slice());
            let (entry, next) = parse_entry(body, pos, version, algorithm, prev)?;
            pos = next;
            entries.push(entry);
        }
        while pos < body.len() {
            let head = body
                .get(pos..pos + 8)
                .ok_or_else(|| corrupt("index", "truncated extension header"))?;
            let len = be32(&head[4..]) as usize;
            if !head[0].is_ascii_uppercase() {
                return Err(Error::Unsupported(format!(
                    "required index extension {:?}",
                    String::from_utf8_lossy(&head[..4])
                )));
            }
            pos = pos
                .checked_add(8 + len)
                .filter(|e| *e <= body.len())
                .ok_or_else(|| corrupt("index", "extension runs past the end"))?;
        }
        Ok(Self { version, entries })
    }

    /// Serializes in `version`, sorting entries by path then stage as git requires.
    pub fn encode(&self, algorithm: Algorithm) -> Result<Vec<u8>> {
        let mut version = self.version;
        if !(2..=4).contains(&version) {
            return Err(Error::Unsupported(format!("index version {version}")));
        }
        if version == 2 && self.entries.iter().any(IndexEntry::needs_extended) {
            version = 3;
        }
        let mut sorted: Vec<&IndexEntry> = self.entries.iter().collect();
        sorted.sort_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
        if sorted
            .windows(2)
            .any(|w| w[0].path == w[1].path && w[0].stage == w[1].stage)
        {
            return Err(invalid("index", "duplicate path and stage"));
        }

        let mut out = Vec::new();
        out.extend_from_slice(b"DIRC");
        out.extend_from_slice(&version.to_be_bytes());
        out.extend_from_slice(
            &u32::try_from(sorted.len())
                .map_err(|_| invalid("index", "too many entries"))?
                .to_be_bytes(),
        );
        let mut prev: &[u8] = &[];
        for e in sorted {
            if e.id.algorithm() != algorithm {
                return Err(invalid(
                    "index",
                    "entry id algorithm differs from the repository",
                ));
            }
            if e.path.is_empty() || e.path.contains(&0) || e.stage > 3 {
                return Err(invalid("index", "bad path or stage"));
            }
            let start = out.len();
            for t in [e.ctime, e.mtime] {
                out.extend_from_slice(&t.seconds.to_be_bytes());
                out.extend_from_slice(&t.nanos.to_be_bytes());
            }
            for v in [e.dev, e.ino, e.mode, e.uid, e.gid, e.size] {
                out.extend_from_slice(&v.to_be_bytes());
            }
            out.extend_from_slice(e.id.as_bytes());
            let extended = version >= 3 && e.needs_extended();
            let mut flags =
                u16::from(e.stage) << FLAG_STAGE_SHIFT | (e.path.len().min(0xfff) as u16);
            if e.assume_valid {
                flags |= FLAG_ASSUME_VALID;
            }
            if extended {
                flags |= FLAG_EXTENDED;
            }
            out.extend_from_slice(&flags.to_be_bytes());
            if extended {
                let mut ext = 0;
                if e.skip_worktree {
                    ext |= EXT_SKIP_WORKTREE;
                }
                if e.intent_to_add {
                    ext |= EXT_INTENT_TO_ADD;
                }
                out.extend_from_slice(&u16::to_be_bytes(ext));
            }
            if version == 4 {
                let common = prev.iter().zip(&e.path).take_while(|(a, b)| a == b).count();
                write_offset_varint(&mut out, (prev.len() - common) as u64);
                out.extend_from_slice(&e.path[common..]);
                out.push(0);
                prev = &e.path;
            } else {
                out.extend_from_slice(&e.path);
                let len = out.len() - start;
                out.resize(start + (len + 8) / 8 * 8, 0);
            }
        }
        let sum = checksum(algorithm, &out);
        out.extend_from_slice(&sum);
        Ok(out)
    }

    /// Writes through `index.lock` and a rename, as git does.
    pub fn write(&self, path: &Path, algorithm: Algorithm) -> Result<()> {
        let bytes = self.encode(algorithm)?;
        let lock = PathBuf::from(format!("{}.lock", path.display()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .map_err(io_at(&lock))?;
        let result = file
            .write_all(&bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&lock, path));
        if let Err(e) = result {
            let _ = fs::remove_file(&lock);
            return Err(io_at(path)(e));
        }
        Ok(())
    }
}

fn parse_entry(
    body: &[u8],
    start: usize,
    version: u32,
    algorithm: Algorithm,
    prev: Option<&[u8]>,
) -> Result<(IndexEntry, usize)> {
    let hl = algorithm.digest_len();
    let trunc = || corrupt("index", "truncated entry");
    let fixed = body.get(start..start + 40 + hl + 2).ok_or_else(trunc)?;
    let word = |i: usize| be32(&fixed[i * 4..]);
    let id = ObjectId::from_bytes(algorithm, &fixed[40..40 + hl])?;
    let flags = u16::from_be_bytes([fixed[40 + hl], fixed[41 + hl]]);
    let mut pos = start + 40 + hl + 2;
    let mut ext = 0;
    if flags & FLAG_EXTENDED != 0 {
        if version < 3 {
            return Err(corrupt("index", "extended flags in a version 2 index"));
        }
        let b = body.get(pos..pos + 2).ok_or_else(trunc)?;
        ext = u16::from_be_bytes([b[0], b[1]]);
        pos += 2;
    }

    let path;
    if version == 4 {
        let (strip, used) = read_offset_varint(&body[pos..])?;
        pos += used;
        let prev = prev.unwrap_or(&[]);
        let keep = prev
            .len()
            .checked_sub(usize::try_from(strip).map_err(|_| trunc())?)
            .ok_or_else(|| corrupt("index", "path prefix longer than the previous path"))?;
        let nul = body[pos..].iter().position(|b| *b == 0).ok_or_else(trunc)?;
        path = [&prev[..keep], &body[pos..pos + nul]].concat();
        pos += nul + 1;
    } else {
        let declared = usize::from(flags & FLAG_NAME_MASK);
        let nul = body[pos..].iter().position(|b| *b == 0).ok_or_else(trunc)?;
        if declared < 0xfff && declared != nul {
            return Err(corrupt("index", "path length differs from its flags"));
        }
        path = body[pos..pos + nul].to_vec();
        let len = pos + nul - start;
        pos = start + (len + 8) / 8 * 8;
        if pos > body.len() {
            return Err(trunc());
        }
    }
    if path.is_empty() {
        return Err(corrupt("index", "empty path"));
    }
    Ok((
        IndexEntry {
            ctime: Timestamp {
                seconds: word(0),
                nanos: word(1),
            },
            mtime: Timestamp {
                seconds: word(2),
                nanos: word(3),
            },
            dev: word(4),
            ino: word(5),
            mode: word(6),
            uid: word(7),
            gid: word(8),
            size: word(9),
            id,
            stage: ((flags >> FLAG_STAGE_SHIFT) & 3) as u8,
            assume_valid: flags & FLAG_ASSUME_VALID != 0,
            skip_worktree: ext & EXT_SKIP_WORKTREE != 0,
            intent_to_add: ext & EXT_INTENT_TO_ADD != 0,
            path,
        },
        pos,
    ))
}

/// Git's offset varint: big-endian groups of seven bits, each continuation adding one.
fn read_offset_varint(data: &[u8]) -> Result<(u64, usize)> {
    let trunc = || corrupt("index", "truncated varint");
    let mut i = 0;
    let mut c = *data.get(i).ok_or_else(trunc)?;
    i += 1;
    let mut value = u64::from(c & 0x7f);
    while c & 0x80 != 0 {
        c = *data.get(i).ok_or_else(trunc)?;
        i += 1;
        value = value
            .checked_add(1)
            .and_then(|v| v.checked_mul(128))
            .ok_or_else(|| corrupt("index", "varint overflow"))?
            | u64::from(c & 0x7f);
    }
    Ok((value, i))
}

fn write_offset_varint(out: &mut Vec<u8>, mut value: u64) {
    let mut buf = [0u8; 10];
    let mut pos = buf.len() - 1;
    buf[pos] = (value & 0x7f) as u8;
    value >>= 7;
    while value != 0 {
        value -= 1;
        pos -= 1;
        buf[pos] = 0x80 | (value & 0x7f) as u8;
        value >>= 7;
    }
    out.extend_from_slice(&buf[pos..]);
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips() {
        for v in [
            0u64,
            1,
            127,
            128,
            129,
            16511,
            16512,
            1 << 21,
            u32::MAX as u64,
        ] {
            let mut b = Vec::new();
            write_offset_varint(&mut b, v);
            assert_eq!(read_offset_varint(&b).unwrap(), (v, b.len()), "{v}");
        }
    }
}

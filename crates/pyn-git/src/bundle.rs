//! Git bundle files (v2 and v3): the header of ref lines and prerequisites, then one pack.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;

use pyn_object::{Algorithm, ObjectId};

use crate::error::{Error, Result, invalid, io_at};
use crate::odb::ObjectDb;
use crate::util::Checksummer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prerequisite {
    pub id: ObjectId,
    pub comment: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub version: u8,
    pub algorithm: Algorithm,
    /// The `@filter=` capability of a partial bundle.
    pub filter: Option<String>,
    pub prerequisites: Vec<Prerequisite>,
    pub refs: Vec<(String, ObjectId)>,
    /// Where the pack starts in the file.
    pub pack_offset: u64,
}

fn not_a_bundle() -> Error {
    invalid("bundle", "does not look like a v2 or v3 bundle file")
}

impl Bundle {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(io_at(path))?;
        Self::read(BufReader::new(file))
    }

    pub fn read<R: BufRead>(mut input: R) -> Result<Self> {
        let mut offset = 0u64;
        let mut line = String::new();
        let mut next = |input: &mut R, line: &mut String| -> Result<bool> {
            line.clear();
            let n = input.read_line(line).map_err(Error::Read)?;
            offset += n as u64;
            if n == 0 {
                return Ok(false);
            }
            if !line.ends_with('\n') {
                return Err(not_a_bundle());
            }
            line.pop();
            Ok(true)
        };
        if !next(&mut input, &mut line)? {
            return Err(not_a_bundle());
        }
        let version = match line.as_str() {
            "# v2 git bundle" => 2,
            "# v3 git bundle" => 3,
            _ => return Err(not_a_bundle()),
        };
        let mut algorithm = Algorithm::Sha1;
        let mut filter = None;
        let mut prerequisites = Vec::new();
        let mut refs = Vec::new();
        let mut in_caps = version == 3;
        loop {
            if !next(&mut input, &mut line)? {
                return Err(invalid("bundle", "header is truncated"));
            }
            if line.is_empty() {
                break;
            }
            if in_caps && let Some(cap) = line.strip_prefix('@') {
                let (key, value) = cap.split_once('=').unwrap_or((cap, ""));
                match key {
                    "object-format" => {
                        algorithm = Algorithm::from_name(value).ok_or_else(|| {
                            invalid("bundle", format!("unknown hash algorithm '{value}'"))
                        })?;
                    }
                    "filter" => filter = Some(value.to_owned()),
                    other => {
                        return Err(invalid("bundle", format!("unknown capability '{other}'")));
                    }
                }
                continue;
            }
            in_caps = false;
            if let Some(rest) = line.strip_prefix('-') {
                let (hex, comment) = rest.split_once(' ').unwrap_or((rest, ""));
                prerequisites.push(Prerequisite {
                    id: ObjectId::from_hex(algorithm, hex)?,
                    comment: comment.to_owned(),
                });
            } else {
                let (hex, name) = line.split_once(' ').ok_or_else(not_a_bundle)?;
                refs.push((name.to_owned(), ObjectId::from_hex(algorithm, hex)?));
            }
        }
        Ok(Self {
            version,
            algorithm,
            filter,
            prerequisites,
            refs,
            pack_offset: offset,
        })
    }

    /// The pack, positioned at its first byte.
    pub fn pack(&self, path: &Path) -> Result<File> {
        let mut file = File::open(path).map_err(io_at(path))?;
        file.seek(SeekFrom::Start(self.pack_offset))
            .map_err(io_at(path))?;
        Ok(file)
    }

    /// The prerequisites `db` does not hold, in header order.
    pub fn missing(&self, db: &ObjectDb) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        for p in &self.prerequisites {
            if !db.contains(&p.id)? {
                out.push(p.id);
            }
        }
        Ok(out)
    }

    /// Checks the pack's signature, object count field and trailing checksum.
    pub fn check_pack(&self, path: &Path) -> Result<()> {
        let mut pack = BufReader::new(self.pack(path)?);
        let mut head = [0u8; 12];
        pack.read_exact(&mut head)
            .map_err(|_| Error::Truncated("pack"))?;
        if &head[..4] != b"PACK" || !matches!(head[7], 2 | 3) || head[4..7] != [0, 0, 0] {
            return Err(invalid("bundle", "the pack header is not valid"));
        }
        let digest = self.algorithm.digest_len();
        let mut sum = Checksummer::new(self.algorithm);
        sum.update(&head);
        let mut tail: Vec<u8> = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = pack.read(&mut buf).map_err(Error::Read)?;
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
            if tail.len() > digest {
                let keep = tail.len() - digest;
                sum.update(&tail[..keep]);
                tail.drain(..keep);
            }
        }
        if tail.len() != digest {
            return Err(Error::Truncated("pack"));
        }
        if sum.finish() != tail {
            return Err(Error::Checksum { what: "pack" });
        }
        Ok(())
    }
}

/// Writes a bundle header. A `version` of 2 needs SHA-1; 3 records the object format.
pub fn write_header<W: Write>(
    out: &mut W,
    version: u8,
    algorithm: Algorithm,
    prerequisites: &[Prerequisite],
    refs: &[(String, ObjectId)],
) -> Result<()> {
    let mut text = format!("# v{version} git bundle\n");
    if version == 3 {
        text.push_str(&format!("@object-format={}\n", algorithm.name()));
    }
    for p in prerequisites {
        text.push_str(&format!("-{} {}\n", p.id, p.comment));
    }
    for (name, id) in refs {
        text.push_str(&format!("{id} {name}\n"));
    }
    text.push('\n');
    out.write_all(text.as_bytes()).map_err(Error::Write)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(Algorithm::Sha1, &[n; 20]).unwrap()
    }

    #[test]
    fn a_header_round_trips_and_marks_where_the_pack_starts() {
        let prereqs = [Prerequisite {
            id: id(1),
            comment: "subject".into(),
        }];
        let refs = [("refs/heads/main".to_owned(), id(2))];
        for version in [2, 3] {
            let mut bytes = Vec::new();
            write_header(&mut bytes, version, Algorithm::Sha1, &prereqs, &refs).unwrap();
            let len = bytes.len() as u64;
            bytes.extend_from_slice(b"PACK");
            let b = Bundle::read(&bytes[..]).unwrap();
            assert_eq!((b.version, b.pack_offset), (version, len));
            assert_eq!(b.prerequisites, prereqs);
            assert_eq!(b.refs, refs);
        }
    }

    #[test]
    fn other_files_and_unknown_capabilities_are_rejected() {
        assert!(Bundle::read(&b"PACK\n"[..]).is_err());
        assert!(Bundle::read(&b"# v3 git bundle\n@mystery\n\n"[..]).is_err());
        assert!(Bundle::read(&b"# v2 git bundle\n"[..]).is_err());
    }
}

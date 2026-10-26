//! What each object points at, read tolerantly, for the storage commands (`fsck`, `prune`, `repack`, `gc`).

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use pyn_git::{ObjectDb, name_hash};
use pyn_object::{Algorithm, Kind, ObjectId};

/// An object another one names, the kind the reference expects, and the path component for tree entries.
pub(super) struct Link {
    pub id: ObjectId,
    pub kind: Kind,
    pub name: Option<Vec<u8>>,
}

fn hex_id(alg: Algorithm, text: &[u8]) -> Option<ObjectId> {
    std::str::from_utf8(text)
        .ok()
        .and_then(|h| ObjectId::from_hex(alg, h).ok())
}

/// A raw tree entry: the mode text, the name and the id.
pub(super) struct RawEntry<'a> {
    pub mode: &'a [u8],
    pub name: &'a [u8],
    pub id: ObjectId,
}

/// The entries of a tree body, or git's complaint about the first malformed one.
pub(super) fn tree_entries(
    alg: Algorithm,
    body: &[u8],
) -> std::result::Result<Vec<RawEntry<'_>>, &'static str> {
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let sp = rest
            .iter()
            .position(|b| *b == b' ')
            .ok_or("too-short tree object")?;
        let mode = &rest[..sp];
        if mode.is_empty() || !mode.iter().all(|b| (b'0'..=b'7').contains(b)) {
            return Err("malformed mode in tree entry");
        }
        rest = &rest[sp + 1..];
        let nul = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or("too-short tree object")?;
        let name = &rest[..nul];
        if name.is_empty() {
            return Err("empty filename in tree entry");
        }
        rest = &rest[nul + 1..];
        let n = alg.digest_len();
        let raw = rest.get(..n).ok_or("too-short tree object")?;
        let id = ObjectId::from_bytes(alg, raw).map_err(|_| "too-short tree object")?;
        rest = &rest[n..];
        out.push(RawEntry { mode, name, id });
    }
    Ok(out)
}

/// The objects `body` of kind `kind` refers to, or `None` when it cannot be parsed. Submodule entries are skipped.
pub(super) fn links(alg: Algorithm, kind: Kind, body: &[u8]) -> Option<Vec<Link>> {
    match kind {
        Kind::Blob => Some(Vec::new()),
        Kind::Tree => {
            let entries = tree_entries(alg, body).ok()?;
            Some(
                entries
                    .into_iter()
                    .filter_map(|e| {
                        let mode =
                            u32::from_str_radix(std::str::from_utf8(e.mode).ok()?, 8).ok()?;
                        let kind = match mode & 0o170000 {
                            0o040000 => Kind::Tree,
                            0o160000 => return None,
                            _ => Kind::Blob,
                        };
                        Some(Link {
                            id: e.id,
                            kind,
                            name: Some(e.name.to_vec()),
                        })
                    })
                    .collect(),
            )
        }
        Kind::Commit => {
            let mut lines = body.split(|b| *b == b'\n');
            let tree = hex_id(alg, lines.next()?.strip_prefix(b"tree ")?)?;
            let mut out = vec![Link {
                id: tree,
                kind: Kind::Tree,
                name: None,
            }];
            for line in lines {
                let Some(p) = line.strip_prefix(b"parent ") else {
                    break;
                };
                out.push(Link {
                    id: hex_id(alg, p)?,
                    kind: Kind::Commit,
                    name: None,
                });
            }
            Some(out)
        }
        Kind::Tag => {
            let mut lines = body.split(|b| *b == b'\n');
            let id = hex_id(alg, lines.next()?.strip_prefix(b"object ")?)?;
            let kind = Kind::from_name(lines.next()?.strip_prefix(b"type ")?)?;
            Some(vec![Link {
                id,
                kind,
                name: None,
            }])
        }
    }
}

/// Everything reachable from `roots`, with a delta-grouping hint per tree entry.
pub(super) struct Reach {
    pub seen: HashSet<ObjectId>,
    pub hints: HashMap<ObjectId, u32>,
}

/// Walks from `roots`. A missing or unparsable object is skipped when `lenient`, else an error.
pub(super) fn mark(db: &ObjectDb, roots: &[ObjectId], lenient: bool) -> Result<Reach> {
    let mut reach = Reach {
        seen: HashSet::new(),
        hints: HashMap::new(),
    };
    let mut stack: Vec<ObjectId> = roots.to_vec();
    while let Some(id) = stack.pop() {
        if !reach.seen.insert(id) {
            continue;
        }
        let object = match db.read_opt(&id) {
            Ok(Some(o)) => o,
            _ if lenient => continue,
            Ok(None) => bail!("bad object {id}"),
            Err(e) => bail!("bad object {id}: {e}"),
        };
        let Some(found) = links(db.algorithm(), object.kind, &object.data) else {
            if lenient {
                continue;
            }
            bail!("bad object {id}");
        };
        let shallow = object.kind == Kind::Commit && db.is_shallow(&id);
        for link in found {
            if shallow && link.kind == Kind::Commit {
                continue;
            }
            if let Some(name) = &link.name {
                reach
                    .hints
                    .entry(link.id)
                    .or_insert_with(|| name_hash(name));
            }
            stack.push(link.id);
        }
    }
    Ok(reach)
}

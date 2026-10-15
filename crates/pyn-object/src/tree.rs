use std::cmp::Ordering;
use std::collections::HashSet;

use crate::{Algorithm, Error, ObjectId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileMode {
    Regular,
    Executable,
    Symlink,
    Tree,
    Gitlink,
}

impl FileMode {
    /// The octal text git writes in a tree: no leading zero, so a tree is `40000`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
            Self::Tree => "40000",
            Self::Gitlink => "160000",
        }
    }

    pub fn parse(s: &[u8]) -> Result<Self, Error> {
        match s {
            b"100644" => Ok(Self::Regular),
            b"100755" => Ok(Self::Executable),
            b"120000" => Ok(Self::Symlink),
            b"40000" => Ok(Self::Tree),
            b"160000" => Ok(Self::Gitlink),
            _ => Err(Error::InvalidMode(String::from_utf8_lossy(s).into_owned())),
        }
    }

    pub fn is_tree(self) -> bool {
        self == Self::Tree
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: FileMode,
    pub name: Vec<u8>,
    pub id: ObjectId,
}

/// Git's order: bytewise on the name, with a directory compared as if its name ended in `/`.
fn cmp_entries(a: &TreeEntry, b: &TreeEntry) -> Ordering {
    let key = |e: &TreeEntry| {
        let mut k = e.name.clone();
        if e.mode.is_tree() {
            k.push(b'/');
        }
        k
    };
    key(a).cmp(&key(b))
}

fn valid_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.iter().any(|&b| b == b'/' || b == 0)
}

/// Sorts the entries into git's order and encodes the tree body. Every id must use `algorithm`.
pub fn encode_tree(algorithm: Algorithm, entries: &[TreeEntry]) -> Result<Vec<u8>, Error> {
    let mut seen = HashSet::new();
    for e in entries {
        e.id.expect(algorithm)?;
        if !valid_name(&e.name) {
            return Err(Error::InvalidName);
        }
        if !seen.insert(e.name.as_slice()) {
            return Err(Error::DuplicateName);
        }
    }
    let mut sorted: Vec<&TreeEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| cmp_entries(a, b));
    let mut out = Vec::new();
    for e in sorted {
        out.extend_from_slice(e.mode.as_str().as_bytes());
        out.push(b' ');
        out.extend_from_slice(&e.name);
        out.push(0);
        out.extend_from_slice(e.id.as_bytes());
    }
    Ok(out)
}

/// Parses a tree body, in stored order. Rejects unsorted or duplicate entries so encoding round-trips.
pub fn parse_tree(algorithm: Algorithm, body: &[u8]) -> Result<Vec<TreeEntry>, Error> {
    let bad = |why| Error::Malformed("tree", why);
    let mut entries: Vec<TreeEntry> = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let sp = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or(bad("missing mode"))?;
        let mode = FileMode::parse(&rest[..sp])?;
        rest = &rest[sp + 1..];
        let nul = rest
            .iter()
            .position(|&b| b == 0)
            .ok_or(bad("missing name end"))?;
        let name = rest[..nul].to_vec();
        if !valid_name(&name) {
            return Err(Error::InvalidName);
        }
        rest = &rest[nul + 1..];
        let n = algorithm.digest_len();
        if rest.len() < n {
            return Err(bad("truncated id"));
        }
        let id = ObjectId::from_bytes(algorithm, &rest[..n])?;
        rest = &rest[n..];
        let entry = TreeEntry { mode, name, id };
        if let Some(prev) = entries.last()
            && cmp_entries(prev, &entry) != Ordering::Less
        {
            return Err(bad("entries not in order"));
        }
        entries.push(entry);
    }
    let names: HashSet<&[u8]> = entries.iter().map(|e| e.name.as_slice()).collect();
    if names.len() != entries.len() {
        return Err(Error::DuplicateName);
    }
    Ok(entries)
}

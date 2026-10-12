use std::fmt;

use crate::{Error, ObjectHasher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Algorithm {
    Sha1,
    Sha256,
}

impl Algorithm {
    pub fn digest_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
        }
    }

    /// The name used by `extensions.objectFormat` and the `object-format` capability.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "sha1" => Some(Self::Sha1),
            "sha256" => Some(Self::Sha256),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Blob,
    Tree,
    Commit,
    Tag,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
            Self::Tag => "tag",
        }
    }

    pub fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"blob" => Some(Self::Blob),
            b"tree" => Some(Self::Tree),
            b"commit" => Some(Self::Commit),
            b"tag" => Some(Self::Tag),
            _ => None,
        }
    }
}

/// A digest tagged with the algorithm that produced it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId {
    algorithm: Algorithm,
    bytes: [u8; 32],
}

impl ObjectId {
    pub fn from_bytes(algorithm: Algorithm, raw: &[u8]) -> Result<Self, Error> {
        if raw.len() != algorithm.digest_len() {
            return Err(Error::InvalidId(format!(
                "{} digest needs {} bytes, got {}",
                algorithm.name(),
                algorithm.digest_len(),
                raw.len()
            )));
        }
        let mut bytes = [0; 32];
        bytes[..raw.len()].copy_from_slice(raw);
        Ok(Self { algorithm, bytes })
    }

    pub fn from_hex(algorithm: Algorithm, hex: &str) -> Result<Self, Error> {
        let raw = hex::decode(hex).map_err(|e| Error::InvalidId(e.to_string()))?;
        Self::from_bytes(algorithm, &raw)
    }

    /// Parses lowercase or uppercase hex, inferring the algorithm from the length (40 or 64 digits).
    pub fn parse(hex: &str) -> Result<Self, Error> {
        let algorithm = match hex.len() {
            40 => Algorithm::Sha1,
            64 => Algorithm::Sha256,
            n => return Err(Error::InvalidId(format!("{n} hex digits"))),
        };
        Self::from_hex(algorithm, hex)
    }

    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.algorithm.digest_len()]
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.as_bytes())
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.name(), self.to_hex())
    }
}

/// Hashes `<kind> <len>\0` followed by `content`.
pub fn hash_object(algorithm: Algorithm, kind: Kind, content: &[u8]) -> Result<ObjectId, Error> {
    let mut h = ObjectHasher::new(algorithm, kind, content.len() as u64);
    h.update(content)?;
    h.finish()
}

use sha1_checked::{CollisionResult, Digest as _, Sha1};
use sha2::{Digest as _, Sha256};

use crate::{Algorithm, Error, Kind, ObjectId};

enum State {
    Sha1(Box<Sha1>),
    Sha256(Sha256),
}

/// Streaming object hasher: the header is hashed from the declared length, the content is fed incrementally.
pub struct ObjectHasher {
    state: State,
    algorithm: Algorithm,
    declared: u64,
    seen: u64,
}

impl ObjectHasher {
    pub fn new(algorithm: Algorithm, kind: Kind, len: u64) -> Self {
        let header = format!("{} {len}\0", kind.name());
        let state = match algorithm {
            Algorithm::Sha1 => {
                let mut h = Sha1::new();
                h.update(header.as_bytes());
                State::Sha1(Box::new(h))
            }
            Algorithm::Sha256 => {
                let mut h = Sha256::new();
                h.update(header.as_bytes());
                State::Sha256(h)
            }
        };
        Self {
            state,
            algorithm,
            declared: len,
            seen: 0,
        }
    }

    pub fn blob(algorithm: Algorithm, len: u64) -> Self {
        Self::new(algorithm, Kind::Blob, len)
    }

    /// Fails as soon as more than the declared length has been supplied.
    pub fn update(&mut self, data: &[u8]) -> Result<(), Error> {
        self.seen = self.seen.saturating_add(data.len() as u64);
        if self.seen > self.declared {
            return Err(Error::LengthMismatch {
                declared: self.declared,
                actual: self.seen,
            });
        }
        match &mut self.state {
            State::Sha1(h) => h.update(data),
            State::Sha256(h) => h.update(data),
        }
        Ok(())
    }

    /// Fails if fewer bytes than declared were supplied, or if SHA-1 collision detection fires.
    pub fn finish(self) -> Result<ObjectId, Error> {
        if self.seen != self.declared {
            return Err(Error::LengthMismatch {
                declared: self.declared,
                actual: self.seen,
            });
        }
        match self.state {
            State::Sha1(h) => match h.try_finalize() {
                CollisionResult::Ok(d) => ObjectId::from_bytes(self.algorithm, &d),
                CollisionResult::Collision(_) | CollisionResult::Mitigated(_) => {
                    Err(Error::Collision)
                }
            },
            State::Sha256(h) => ObjectId::from_bytes(self.algorithm, &h.finalize()),
        }
    }
}

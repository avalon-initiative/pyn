use pyn_object::Algorithm;
use sha1_checked::{Digest as _, Sha1};
use sha2::{Digest as _, Sha256};

/// The raw digest git uses for the trailers of packs, pack indexes and the index file.
pub(crate) fn checksum(algorithm: Algorithm, data: &[u8]) -> Vec<u8> {
    match algorithm {
        Algorithm::Sha1 => Sha1::digest(data).to_vec(),
        Algorithm::Sha256 => Sha256::digest(data).to_vec(),
    }
}

pub(crate) enum Checksummer {
    Sha1(Box<Sha1>),
    Sha256(Sha256),
}

impl Checksummer {
    pub(crate) fn new(algorithm: Algorithm) -> Self {
        match algorithm {
            Algorithm::Sha1 => Self::Sha1(Box::default()),
            Algorithm::Sha256 => Self::Sha256(Sha256::new()),
        }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha1(h) => h.update(data),
            Self::Sha256(h) => h.update(data),
        }
    }

    pub(crate) fn finish(self) -> Vec<u8> {
        match self {
            Self::Sha1(h) => h.finalize().to_vec(),
            Self::Sha256(h) => h.finalize().to_vec(),
        }
    }
}

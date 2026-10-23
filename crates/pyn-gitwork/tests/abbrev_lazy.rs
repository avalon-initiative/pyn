//! The real abbreviation policy against an operation-counting object database.

extern crate pyn_git as git;
extern crate self as pyn_git;

use std::cell::Cell;
use std::sync::{Arc, OnceLock};

use pyn_gitwork::{Error, Result};
use pyn_object::{Algorithm, ObjectId};

#[path = "../src/abbrev.rs"]
mod abbrev;

pub struct ObjectDb {
    ids: Vec<ObjectId>,
    full_scans: Cell<usize>,
    lookups: Cell<usize>,
    counts: Cell<usize>,
}

impl ObjectDb {
    pub fn algorithm(&self) -> Algorithm {
        self.ids[0].algorithm()
    }

    pub fn ids_with_prefix(&self, prefix: &str) -> Result<Vec<ObjectId>> {
        self.full_scans.set(self.full_scans.get() + 1);
        Ok(self
            .ids
            .iter()
            .filter(|id| id.to_hex().starts_with(prefix))
            .copied()
            .collect())
    }

    pub fn approximate_object_count(&self) -> Result<usize> {
        self.counts.set(self.counts.get() + 1);
        Ok(0)
    }

    pub fn abbreviation_len(&self, id: &ObjectId, minimum: usize) -> usize {
        self.lookups.set(self.lookups.get() + 1);
        let hex = id.to_hex();
        self.ids
            .iter()
            .filter(|other| *other != id)
            .fold(minimum, |len, other| {
                len.max(
                    hex.bytes()
                        .zip(other.to_hex().bytes())
                        .take_while(|(a, b)| a == b)
                        .count()
                        + 1,
                )
            })
            .min(hex.len())
    }
}

pub struct Repo {
    objects: Arc<ObjectDb>,
    config: git::Config,
    abbrev: OnceLock<std::result::Result<usize, String>>,
    config_reads: Cell<usize>,
}

impl Repo {
    fn algorithm(&self) -> Algorithm {
        self.objects.algorithm()
    }

    fn objects(&self) -> &ObjectDb {
        &self.objects
    }

    fn config(&self) -> &git::Config {
        self.config_reads.set(self.config_reads.get() + 1);
        &self.config
    }
}

#[test]
fn every_render_uses_lazy_lookup_and_cached_default() {
    // Reintroducing either an eager context or an eager one-id render must fail this assertion.
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let width = algorithm.digest_len() * 2;
        let id =
            ObjectId::from_hex(algorithm, &format!("abcd1234{}", "0".repeat(width - 8))).unwrap();
        let other =
            ObjectId::from_hex(algorithm, &format!("abcd1235{}", "0".repeat(width - 8))).unwrap();
        let repo = Repo {
            objects: Arc::new(ObjectDb {
                ids: vec![id, other],
                full_scans: Cell::new(0),
                lookups: Cell::new(0),
                counts: Cell::new(0),
            }),
            config: git::Config::default(),
            abbrev: OnceLock::new(),
            config_reads: Cell::new(0),
        };
        let context = repo.abbreviations(None).unwrap();
        assert_eq!(
            repo.objects.full_scans.get(),
            0,
            "creating a renderer must not enumerate object ids"
        );
        assert_eq!(
            repo.objects.lookups.get(),
            0,
            "creating a renderer must not look up ids"
        );
        for _ in 0..3 {
            assert_eq!(context.short(&id), "abcd1234");
            assert_eq!(context.short_hex(&other.to_hex()), "abcd1235");
            assert_eq!(context.short_null(), "0000000");
            assert_eq!(repo.abbreviate(&id).unwrap(), "abcd1234");
            assert_eq!(repo.abbreviations(Some(4)).unwrap().short(&id), "abcd1234");
        }
        assert_eq!(
            repo.objects.full_scans.get(),
            0,
            "rendering must not enumerate object ids"
        );
        assert!(
            repo.objects.lookups.get() >= 15,
            "every rendering path must query the object database"
        );
        assert_eq!(
            repo.objects.counts.get(),
            1,
            "default length must be computed once"
        );
        assert_eq!(
            repo.config_reads.get(),
            1,
            "configuration must be read once"
        );
    }
}

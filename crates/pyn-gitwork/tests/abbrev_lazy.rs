//! The real abbreviation policy and object database against counting filesystem and pack fakes.

extern crate pyn_git as git;
extern crate self as pyn_git;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use pyn_gitwork::{Error, Result};
use pyn_object::{Algorithm, ObjectId};

#[path = "../src/abbrev.rs"]
mod abbrev;
#[path = "../../pyn-git/src/error.rs"]
pub mod error;

thread_local! {
    static PACK_ENUMERATIONS: Cell<usize> = const { Cell::new(0) };
    static PACK_OPENS: Cell<usize> = const { Cell::new(0) };
    static LOOSE_READS: Cell<usize> = const { Cell::new(0) };
}

pub mod pack {
    use super::*;

    pub struct Pack(git::Pack);

    impl Pack {
        pub fn open(path: &Path, algorithm: Algorithm) -> error::Result<Self> {
            PACK_OPENS.with(|n| n.set(n.get() + 1));
            Ok(Self(
                git::Pack::open(path, algorithm).expect("a valid fixture pack"),
            ))
        }
        pub fn idx_path(&self) -> &Path {
            self.0.idx_path()
        }
        pub fn pack_path(&self) -> &Path {
            self.0.pack_path()
        }
        pub fn len(&self) -> usize {
            self.0.len()
        }
        pub fn is_empty(&self) -> bool {
            self.0.is_empty()
        }
        pub fn ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
            PACK_ENUMERATIONS.with(|n| n.set(n.get() + 1));
            self.0.ids()
        }
        pub fn abbreviation_len(&self, id: &ObjectId, len: usize) -> usize {
            self.0.abbreviation_len(id, len)
        }
        pub fn read(&self, _: &ObjectId) -> error::Result<Option<instrumented_odb::Object>> {
            panic!("abbreviation lookup must not read object bodies")
        }
    }

    pub fn distinguishing_len(a: &[u8], b: &[u8]) -> usize {
        let a: String = a.iter().map(|b| format!("{b:02x}")).collect();
        let b: String = b.iter().map(|b| format!("{b:02x}")).collect();
        if a == b {
            0
        } else {
            a.bytes().zip(b.bytes()).take_while(|(a, b)| a == b).count() + 1
        }
    }
}

pub mod walk {
    use super::*;

    pub fn object_links(_: Algorithm, _: &ObjectId, _: &instrumented_odb::Object) -> Vec<ObjectId> {
        unreachable!("the fixtures have no promisor packs")
    }
}

// Crate-private helpers of the included database are called only by other pyn-git modules.
#[allow(dead_code)]
pub mod instrumented_odb {
    // Include the production database unchanged; only its filesystem and pack dependencies count operations.
    mod std {
        pub use ::std::{cell, collections, io, path, process, result, str, sync, time};
        pub mod fs {
            pub use ::std::fs::{
                File, OpenOptions, canonicalize, create_dir_all, read_to_string, remove_file,
                rename,
            };
            pub fn read_dir(
                path: impl AsRef<::std::path::Path>,
            ) -> ::std::io::Result<::std::fs::ReadDir> {
                let path = path.as_ref();
                if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                {
                    crate::LOOSE_READS.with(|n| n.set(n.get() + 1));
                }
                ::std::fs::read_dir(path)
            }
        }
    }
    include!("../../pyn-git/src/odb.rs");
}

pub struct ObjectDb {
    inner: instrumented_odb::ObjectDb,
    full_scans: AtomicUsize,
    lookups: AtomicUsize,
    counts: AtomicUsize,
}

impl ObjectDb {
    pub fn algorithm(&self) -> Algorithm {
        self.inner.algorithm()
    }
    pub fn ids_with_prefix(&self, prefix: &str) -> Result<Vec<ObjectId>> {
        if prefix.is_empty() {
            self.full_scans.fetch_add(1, Ordering::Relaxed);
        }
        Ok(self
            .inner
            .ids_with_prefix(prefix)
            .expect("valid fixture metadata"))
    }
    pub fn approximate_object_count(&self) -> Result<usize> {
        self.counts.fetch_add(1, Ordering::Relaxed);
        Ok(self
            .inner
            .approximate_object_count()
            .expect("valid fixture metadata"))
    }
    pub fn abbreviation_len(&self, id: &ObjectId, minimum: usize) -> usize {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        self.inner.abbreviation_len(id, minimum)
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

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

fn fixture(algorithm: Algorithm, layout: &str, root: &Path) -> (Repo, ObjectId, usize) {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pyn-git/tests/fixtures")
        .join(format!("{}-packed/dot-git/objects", algorithm.name()));
    let original = git::ObjectDb::open(&fixture, algorithm).unwrap();
    let id = original.packs()[0].ids().next().unwrap();
    let local = root.join("objects");
    std::fs::create_dir_all(local.join("info")).unwrap();
    let alternate = layout.starts_with("alternate");
    let store = if alternate {
        let store = root.join("alternate");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            local.join("info/alternates"),
            format!("{}\n", store.display()),
        )
        .unwrap();
        store
    } else {
        local.clone()
    };
    if layout.ends_with("packed") {
        copy_tree(&fixture, &store);
    } else {
        let hex = id.to_hex();
        std::fs::create_dir_all(store.join(&hex[..2])).unwrap();
        std::fs::write(store.join(&hex[..2]).join(&hex[2..]), b"").unwrap();
    }
    let repo = Repo {
        objects: Arc::new(ObjectDb {
            inner: instrumented_odb::ObjectDb::open(&local, algorithm).unwrap(),
            full_scans: AtomicUsize::new(0),
            lookups: AtomicUsize::new(0),
            counts: AtomicUsize::new(0),
        }),
        config: git::Config::default(),
        abbrev: OnceLock::new(),
        config_reads: Cell::new(0),
    };
    (repo, id, if alternate { 2 } else { 1 })
}

#[test]
fn every_render_uses_lazy_lookup_and_cached_default() {
    // Each store limb drives the same production policy, database traversal and rendered-result assertions.
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        for layout in ["loose", "packed", "alternate-loose", "alternate-packed"] {
            let tmp = tempfile::tempdir().unwrap();
            let (repo, id, stores) = fixture(algorithm, layout, tmp.path());
            PACK_ENUMERATIONS.with(|n| n.set(0));
            PACK_OPENS.with(|n| n.set(0));
            LOOSE_READS.with(|n| n.set(0));
            let context = repo.abbreviations(None).unwrap();
            assert_eq!(
                repo.objects.full_scans.load(Ordering::Relaxed),
                0,
                "creating a renderer must not enumerate object ids"
            );
            assert_eq!(
                repo.objects.lookups.load(Ordering::Relaxed),
                0,
                "creating a renderer must not look up ids"
            );
            let mut bytes = id.as_bytes().to_vec();
            bytes[algorithm.digest_len() - 1] ^= 1;
            let absent = ObjectId::from_bytes(algorithm, &bytes).unwrap();
            for _ in 0..3 {
                assert_eq!(context.short(&id), &id.to_hex()[..7], "{layout}: hit");
                assert_eq!(
                    context.short_hex(&absent.to_hex()),
                    absent.to_hex(),
                    "{layout}: miss must extend to full width"
                );
                assert_eq!(context.short_null(), "0000000", "{layout}: null miss");
                assert_eq!(
                    repo.abbreviate(&id).unwrap(),
                    &id.to_hex()[..7],
                    "{layout}: one-id renderer"
                );
                assert_eq!(
                    repo.abbreviations(Some(4)).unwrap().short(&absent),
                    absent.to_hex(),
                    "{layout}: explicit renderer"
                );
            }
            assert_eq!(
                repo.objects.full_scans.load(Ordering::Relaxed),
                0,
                "rendering must not enumerate object ids"
            );
            PACK_ENUMERATIONS
                .with(|n| assert_eq!(n.get(), 0, "{layout}: rendering must not enumerate a pack"));
            PACK_OPENS.with(|n| {
                assert_eq!(
                    n.get(),
                    0,
                    "{layout}: rendering must reuse loaded pack indexes"
                )
            });
            LOOSE_READS.with(|n| {
                assert!(
                    n.get() <= stores * 15,
                    "{layout}: at most one loose fan-out read per store per lookup, got {}",
                    n.get()
                )
            });
            assert_eq!(
                repo.objects.lookups.load(Ordering::Relaxed),
                15,
                "every rendering path must query the object database"
            );
            assert_eq!(
                repo.objects.counts.load(Ordering::Relaxed),
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
}

//! Git LFS pointer files and the local object store (`.git/lfs/objects/<aa>/<bb>/<oid>`), as `filter=lfs` uses
//! them. Transfers to and from an LFS server are not part of this module.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result, io_at};

const VERSION: &str = "https://git-lfs.github.com/spec/v1";
const LEGACY_VERSIONS: [&str; 2] = [
    "https://hawser.github.com/spec/v1",
    "http://git-media.io/v/2",
];
const POINTER_LIMIT: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pointer {
    pub oid: String,
    pub size: u64,
}

impl Pointer {
    /// The pointer file for `content`.
    pub fn of(content: &[u8]) -> Self {
        Self {
            oid: sha256_hex(content),
            size: content.len() as u64,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        format!(
            "version {VERSION}\noid sha256:{}\nsize {}\n",
            self.oid, self.size
        )
        .into_bytes()
    }

    /// Parses a pointer file; anything else, including an oversized file, is `None`.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() > POINTER_LIMIT || !data.ends_with(b"\n") {
            return None;
        }
        let text = std::str::from_utf8(data).ok()?;
        let mut lines = text.strip_suffix('\n')?.split('\n');
        let version = lines.next()?.strip_prefix("version ")?;
        if version != VERSION && !LEGACY_VERSIONS.contains(&version) {
            return None;
        }
        let (mut oid, mut size) = (None, None);
        for line in lines {
            let (key, value) = line.split_once(' ')?;
            match key {
                "oid" => {
                    let hex = value.strip_prefix("sha256:")?;
                    let valid = hex.len() == 64
                        && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
                    if !valid {
                        return None;
                    }
                    oid = Some(hex.to_owned());
                }
                "size" => {
                    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    size = Some(value.parse().ok()?);
                }
                _ => {}
            }
        }
        Some(Self {
            oid: oid?,
            size: size?,
        })
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The object store under a repository's common directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

static TMP: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn new(common_dir: &Path) -> Self {
        Self {
            dir: common_dir.join("lfs"),
        }
    }

    pub fn object_path(&self, oid: &str) -> PathBuf {
        self.dir
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(oid)
    }

    /// Stores `content` unless an object of that size is already there.
    pub fn put(&self, content: &[u8]) -> Result<Pointer> {
        let pointer = Pointer::of(content);
        let target = self.object_path(&pointer.oid);
        if std::fs::metadata(&target).is_ok_and(|m| m.len() == pointer.size) {
            return Ok(pointer);
        }
        let tmp_dir = self.dir.join("tmp");
        std::fs::create_dir_all(&tmp_dir).map_err(io_at(&tmp_dir))?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        let tmp = tmp_dir.join(format!(
            "{}-{}",
            std::process::id(),
            TMP.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&tmp, content).map_err(io_at(&tmp))?;
        std::fs::rename(&tmp, &target).map_err(io_at(&target))?;
        Ok(pointer)
    }

    /// The content of an object, `None` when it is not stored; a stored object of the wrong size or hash is an error.
    pub fn get(&self, pointer: &Pointer) -> Result<Option<Vec<u8>>> {
        let path = self.object_path(&pointer.oid);
        let content = match std::fs::read(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_at(&path)(e)),
        };
        if content.len() as u64 != pointer.size || sha256_hex(&content) != pointer.oid {
            return Err(Error::Invalid(format!(
                "Git LFS object {} in the local store is corrupt",
                pointer.oid
            )));
        }
        Ok(Some(content))
    }

    /// The pointer file for work tree content; a pointer file or empty content is returned as it is.
    pub fn clean(&self, data: &[u8]) -> Result<Vec<u8>> {
        if data.is_empty() || Pointer::parse(data).is_some() {
            return Ok(data.to_vec());
        }
        Ok(self.put(data)?.encode())
    }

    /// The content a pointer file stands for; the pointer itself (with a warning) when the object is not stored.
    pub fn smudge(&self, data: &[u8]) -> Result<Vec<u8>> {
        let skip = std::env::var("GIT_LFS_SKIP_SMUDGE").is_ok_and(|v| v == "1" || v == "true");
        let Some(pointer) = Pointer::parse(data).filter(|_| !skip) else {
            return Ok(data.to_vec());
        };
        match self.get(&pointer)? {
            Some(content) => Ok(content),
            None => {
                warn_pointers_kept();
                Ok(data.to_vec())
            }
        }
    }
}

static WARNED: AtomicBool = AtomicBool::new(false);

/// Prints, once per process, that pointer files stay in the work tree because their objects are not stored.
pub fn warn_pointers_kept() {
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "warning: some Git LFS objects are not in the local store (.git/lfs/objects) and pyn cannot download them yet; \
             their pointer files are kept as they are"
        );
    }
}

/// Whether the warning has been printed in this process.
pub fn pointers_warned() -> bool {
    WARNED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_bytes_are_exact() {
        let p = Pointer::of(b"hello\n");
        assert_eq!(
            String::from_utf8(p.encode()).unwrap(),
            "version https://git-lfs.github.com/spec/v1\n\
             oid sha256:5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03\n\
             size 6\n"
        );
        assert_eq!(Pointer::parse(&p.encode()), Some(p));
    }

    #[test]
    fn parse_rejects_what_is_not_a_pointer() {
        let good = Pointer::of(b"x").encode();
        assert!(Pointer::parse(&good).is_some());
        assert!(Pointer::parse(&good[..good.len() - 1]).is_none());
        assert!(Pointer::parse(b"version nope\noid sha256:00\nsize 1\n").is_none());
        let upper = String::from_utf8(good.clone()).unwrap().to_uppercase();
        assert!(Pointer::parse(upper.as_bytes()).is_none());
        assert!(Pointer::parse(&[good, vec![b'x'; 1100]].concat()).is_none());
    }

    #[test]
    fn store_round_trips_and_detects_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let pointer_file = store.clean(b"payload").unwrap();
        assert_eq!(store.clean(&pointer_file).unwrap(), pointer_file);
        assert_eq!(store.smudge(&pointer_file).unwrap(), b"payload");
        let p = Pointer::parse(&pointer_file).unwrap();
        let path = store.object_path(&p.oid);
        assert!(path.ends_with(format!(
            "objects/{}/{}/{}",
            &p.oid[..2],
            &p.oid[2..4],
            p.oid
        )));
        std::fs::write(&path, b"PAYLOAD").unwrap();
        assert!(store.smudge(&pointer_file).is_err());
        assert_eq!(store.clean(b"").unwrap(), b"");
    }
}

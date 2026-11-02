//! Git LFS pointer files and the local object store (`<git dir>/lfs/objects/<aa>/<bb>/<oid>`), as `filter=lfs` uses
//! them. Transfers to and from an LFS server are done by the downloader installed with [`set_downloader`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pyn_git::Config;
use sha2::{Digest as _, Sha256};

use crate::error::{Error, Result, io_at};
use crate::wildmatch::matches as wildmatch;

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
    pub extensions: Vec<Extension>,
}

/// An `ext-<priority>-<name> sha256:<oid>` line: the object id before the extension `name` ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    pub priority: u8,
    pub name: String,
    pub oid: String,
}

fn is_oid(hex: &str) -> bool {
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Pointer {
    /// The pointer file for `content`.
    pub fn of(content: &[u8]) -> Self {
        Self {
            oid: sha256_hex(content),
            size: content.len() as u64,
            extensions: Vec::new(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = format!("version {VERSION}\n");
        let mut exts: Vec<&Extension> = self.extensions.iter().collect();
        exts.sort_by_key(|e| e.priority);
        for e in exts {
            out.push_str(&format!("ext-{}-{} sha256:{}\n", e.priority, e.name, e.oid));
        }
        out.push_str(&format!("oid sha256:{}\nsize {}\n", self.oid, self.size));
        out.into_bytes()
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
        let (mut oid, mut size, mut extensions) = (None, None, Vec::new());
        for line in lines {
            let (key, value) = line.split_once(' ')?;
            match key {
                "oid" => {
                    let hex = value.strip_prefix("sha256:")?;
                    if !is_oid(hex) {
                        return None;
                    }
                    oid = Some(hex.to_owned());
                }
                _ if key.starts_with("ext-") => {
                    let hex = value.strip_prefix("sha256:").filter(|h| is_oid(h))?;
                    let (priority, name) = key["ext-".len()..].split_once('-')?;
                    if priority.len() != 1 || name.is_empty() {
                        return None;
                    }
                    extensions.push(Extension {
                        priority: priority.parse().ok()?,
                        name: name.to_owned(),
                        oid: hex.to_owned(),
                    });
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
            extensions,
        })
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Where the store lives and which paths are downloaded: `lfs.storage`, `lfs.fetchinclude` and `lfs.fetchexclude`.
#[derive(Debug, Clone)]
pub struct Options {
    dir: PathBuf,
    include: Vec<String>,
    exclude: Vec<String>,
}

impl Options {
    /// A relative `lfs.storage` is relative to the common directory.
    pub fn new(common_dir: &Path, config: &Config) -> Self {
        let dir = match config.get("lfs.storage").filter(|v| !v.is_empty()) {
            Some(v) => common_dir.join(v),
            None => common_dir.join("lfs"),
        };
        let list = |key: &str| {
            config
                .get(key)
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_owned)
                .collect()
        };
        Self {
            dir,
            include: list("lfs.fetchinclude"),
            exclude: list("lfs.fetchexclude"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether `path` passes `lfs.fetchinclude` (when set) and is not in `lfs.fetchexclude`.
    pub fn allows(&self, path: &[u8]) -> bool {
        (self.include.is_empty() || self.include.iter().any(|p| path_matches(p, path)))
            && !self.exclude.iter().any(|p| path_matches(p, path))
    }
}

/// A pattern without a slash matches at any depth; a trailing slash matches everything below the folder.
fn path_matches(pattern: &str, path: &[u8]) -> bool {
    let anchored = pattern.starts_with('/');
    let pattern = pattern.trim_start_matches('/');
    let folder = pattern.ends_with('/');
    let pattern = pattern.trim_end_matches('/');
    let deep = |p: &str| format!("{p}/**");
    let mut candidates = vec![pattern.to_owned(), deep(pattern)];
    if !anchored && !pattern.contains('/') {
        candidates.push(format!("**/{pattern}"));
        candidates.push(format!("**/{}", deep(pattern)));
    }
    candidates
        .iter()
        .enumerate()
        .filter(|(i, _)| !folder || i % 2 == 1)
        .any(|(_, c)| wildmatch(c.as_bytes(), path, true))
}

/// The keys `.lfsconfig` may set: the LFS settings, never anything that runs a program.
pub fn lfsconfig_layer(work_tree: &Path) -> Config {
    let Ok(config) = Config::read(&work_tree.join(".lfsconfig")) else {
        return Config::default();
    };
    config.filtered(|e| {
        e.section == "lfs"
            || (e.section == "remote" && (e.key == "lfsurl" || e.key == "lfspushurl"))
    })
}

/// What a downloader needs to know about the repository a smudge runs in.
#[derive(Debug, Clone)]
pub struct Context {
    pub common_dir: PathBuf,
    pub work_tree: Option<PathBuf>,
    pub options: Options,
}

/// Fetches the objects into the store; the error text is shown to the user.
pub type Downloader = fn(&Context, &[Pointer]) -> std::result::Result<(), String>;

static DOWNLOADER: OnceLock<Downloader> = OnceLock::new();

/// Lets a smudge download absent objects through `download`.
pub fn set_downloader(download: Downloader) {
    let _ = DOWNLOADER.set(download);
}

/// The object store under a repository's common directory.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
    context: Context,
}

static TMP: AtomicU64 = AtomicU64::new(0);

impl Store {
    pub fn new(root: Option<&Path>, common_dir: &Path, options: Options) -> Self {
        Self {
            dir: options.dir.clone(),
            context: Context {
                common_dir: common_dir.to_path_buf(),
                work_tree: root.map(Path::to_path_buf),
                options,
            },
        }
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.dir.join("tmp")
    }

    pub fn object_path(&self, oid: &str) -> PathBuf {
        self.dir
            .join("objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(oid)
    }

    /// Whether the store has any objects directory yet.
    pub fn exists(&self) -> bool {
        self.dir.join("objects").is_dir()
    }

    /// Whether the store holds an object of `pointer`'s size.
    pub fn has(&self, pointer: &Pointer) -> bool {
        std::fs::metadata(self.object_path(&pointer.oid)).is_ok_and(|m| m.len() == pointer.size)
    }

    /// Moves the verified file `from` into the store as `oid`.
    pub fn adopt(&self, oid: &str, from: &Path) -> Result<()> {
        let target = self.object_path(oid);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        std::fs::rename(from, &target).map_err(io_at(&target))
    }

    /// Stores `content` unless an object of that size is already there.
    pub fn put(&self, content: &[u8]) -> Result<Pointer> {
        let pointer = Pointer::of(content);
        let target = self.object_path(&pointer.oid);
        if self.has(&pointer) {
            return Ok(pointer);
        }
        let tmp_dir = self.tmp_dir();
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

    /// The content a pointer file stands for, downloading it first when `path` is allowed and the store lacks it;
    /// the pointer itself (with a warning) when the object cannot be had.
    pub fn smudge(&self, path: &[u8], data: &[u8]) -> Result<Vec<u8>> {
        let skip = std::env::var("GIT_LFS_SKIP_SMUDGE").is_ok_and(|v| v == "1" || v == "true");
        let Some(pointer) = Pointer::parse(data).filter(|_| !skip) else {
            return Ok(data.to_vec());
        };
        if !pointer.extensions.is_empty() {
            warn_pointers_kept(Some("the pointer uses extensions, which pyn does not run"));
            return Ok(data.to_vec());
        }
        if !self.has(&pointer)
            && self.context.options.allows(path)
            && let Some(download) = DOWNLOADER.get()
            && let Err(why) = download(&self.context, std::slice::from_ref(&pointer))
        {
            warn_pointers_kept(Some(&why));
            return Ok(data.to_vec());
        }
        match self.get(&pointer)? {
            Some(content) => Ok(content),
            None => {
                warn_pointers_kept(None);
                Ok(data.to_vec())
            }
        }
    }
}

static WARNED: AtomicBool = AtomicBool::new(false);

/// Prints, once per process, that pointer files stay in the work tree because their objects are not stored.
pub fn warn_pointers_kept(why: Option<&str>) {
    if !WARNED.swap(true, Ordering::Relaxed) {
        let why = why.map_or(String::new(), |w| format!(": {w}"));
        eprintln!(
            "warning: some Git LFS objects are not in the local store and were not downloaded{why}; \
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
        let store = Store::new(
            None,
            dir.path(),
            Options::new(dir.path(), &Config::default()),
        );
        let pointer_file = store.clean(b"payload").unwrap();
        assert_eq!(store.clean(&pointer_file).unwrap(), pointer_file);
        assert_eq!(store.smudge(b"f", &pointer_file).unwrap(), b"payload");
        let p = Pointer::parse(&pointer_file).unwrap();
        let path = store.object_path(&p.oid);
        assert!(path.ends_with(format!(
            "objects/{}/{}/{}",
            &p.oid[..2],
            &p.oid[2..4],
            p.oid
        )));
        std::fs::write(&path, b"PAYLOAD").unwrap();
        assert!(store.smudge(b"f", &pointer_file).is_err());
        assert_eq!(store.clean(b"").unwrap(), b"");
    }

    #[test]
    fn extension_lines_round_trip_in_order() {
        let oid = "b".repeat(64);
        let text = format!(
            "version {VERSION}\next-0-foo sha256:{0}\next-1-bar sha256:{0}\noid sha256:{oid}\nsize 9\n",
            "a".repeat(64)
        );
        let p = Pointer::parse(text.as_bytes()).unwrap();
        assert_eq!(p.extensions.len(), 2);
        assert_eq!(p.extensions[1].name, "bar");
        assert_eq!(p.encode(), text.as_bytes());
        let bad = text.replace("ext-1-bar sha256:", "ext-1-bar md5:");
        assert!(Pointer::parse(bad.as_bytes()).is_none());
        assert!(Pointer::parse(text.replace("ext-1-", "ext-x-").as_bytes()).is_none());
    }

    fn options(config: &str) -> Options {
        Options::new(Path::new("/g"), &Config::parse(config.as_bytes()).unwrap())
    }

    #[test]
    fn storage_and_fetch_filters_come_from_config() {
        assert_eq!(options("").dir(), Path::new("/g/lfs"));
        assert_eq!(
            options("[lfs]\n\tstorage = s/x\n").dir(),
            Path::new("/g/s/x")
        );
        assert_eq!(
            options("[lfs]\n\tstorage = /abs\n").dir(),
            Path::new("/abs")
        );
        assert!(options("").allows(b"any/file.bin"));
        let o = options("[lfs]\n\tfetchinclude = assets/, *.psd\n\tfetchexclude = assets/old/\n");
        assert!(o.allows(b"assets/a.bin"));
        assert!(o.allows(b"deep/er/x.psd"));
        assert!(!o.allows(b"assets/old/a.bin"));
        assert!(!o.allows(b"docs/a.bin"));
        let o = options("[lfs]\n\tfetchinclude = /top.bin\n");
        assert!(o.allows(b"top.bin") && !o.allows(b"sub/top.bin"));
    }

    #[test]
    fn lfsconfig_keeps_only_lfs_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".lfsconfig"),
            "[lfs]\n\turl = http://x\n[remote \"o\"]\n\tlfsurl = http://y\n\turl = http://z\n[credential]\n\thelper = evil\n",
        )
        .unwrap();
        let c = lfsconfig_layer(dir.path());
        assert_eq!(c.get("lfs.url"), Some("http://x"));
        assert_eq!(c.get("remote.o.lfsurl"), Some("http://y"));
        assert_eq!(c.get("remote.o.url"), None);
        assert_eq!(c.get("credential.helper"), None);
        assert!(
            lfsconfig_layer(Path::new("/nonexistent"))
                .entries()
                .is_empty()
        );
    }
}

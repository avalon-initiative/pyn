//! A workspace is a directory with a `.pyn/` folder: shared files directly in it, and everything local to this
//! machine (settings, which revision each file is at, caches) in `.pyn/local_only/`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use globset::{Glob, GlobSet, GlobSetBuilder};
use pyn_object::{Algorithm, Kind};
use pyn_proto as api;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DIR: &str = ".pyn";
const LOCAL_ONLY: &str = "local_only";
const STATE_VERSION: u32 = 2;

/// Settings that can be set per workspace or per user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub server: Option<String>,
    pub user: Option<String>,
    /// The repository as `owner/name`.
    pub repo: Option<String>,
}

pub const SETTING_KEYS: [&str; 3] = ["server", "user", "repo"];

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("reading {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn get(&self, key: &str) -> Result<Option<&String>> {
        match key {
            "server" => Ok(self.server.as_ref()),
            "user" => Ok(self.user.as_ref()),
            "repo" => Ok(self.repo.as_ref()),
            other => bail!(
                "unknown setting {other:?}; the settings are {}",
                SETTING_KEYS.join(", ")
            ),
        }
    }

    pub fn set(&mut self, key: &str, value: String) -> Result<()> {
        match key {
            "server" => self.server = Some(value),
            "user" => self.user = Some(value),
            "repo" => {
                crate::address::split_repo(&value)?;
                self.repo = Some(value);
            }
            other => bail!(
                "unknown setting {other:?}; the settings are {}",
                SETTING_KEYS.join(", ")
            ),
        }
        Ok(())
    }
}

/// What the workspace knows about a file it has downloaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileState {
    pub revision: u64,
    /// Git blob id of the content at that revision, to tell whether the local copy changed.
    pub hash: String,
    /// Set for a state recorded before blob ids, whose `hash` is the SHA-256 of the bare content, until it is next updated.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub legacy: bool,
    /// `shared` or `exclusive`, as the server last said.
    pub mode: String,
    /// Git's file mode of the content at that revision.
    #[serde(default)]
    pub file_mode: api::FileMode,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StateFile {
    /// 2 once hashes are blob ids; absent in older files.
    #[serde(default)]
    version: u32,
    #[serde(default)]
    files: BTreeMap<String, FileState>,
}

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
    pub dir: PathBuf,
}

/// The git blob id of `bytes` as the server computes it.
pub fn hash_of(bytes: &[u8]) -> String {
    pyn_object::hash_object(Algorithm::Sha256, Kind::Blob, bytes)
        .expect("hashing a whole buffer cannot fail")
        .to_hex()
}

impl FileState {
    /// Whether the file on disk, with this mode and content, differs from what this state recorded.
    pub fn changed(&self, file_mode: api::FileMode, bytes: &[u8]) -> bool {
        !self.matches(bytes) || crate::localfile::mode_differs(self.file_mode, file_mode)
    }

    /// Whether `bytes` are the content this state recorded.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        if self.legacy {
            hex::encode(Sha256::digest(bytes)) == self.hash
        } else {
            hash_of(bytes) == self.hash
        }
    }
}

impl Workspace {
    /// `PYN_DIR` names the `.pyn` folder directly; otherwise the nearest one above `start`.
    pub fn discover(start: &Path) -> Option<Self> {
        if let Ok(dir) = std::env::var("PYN_DIR") {
            let dir = PathBuf::from(dir);
            let root = dir.parent()?.to_path_buf();
            return dir.is_dir().then_some(Self { root, dir });
        }
        start.ancestors().find_map(|p| {
            let dir = p.join(DIR);
            dir.is_dir().then(|| Self {
                root: p.to_path_buf(),
                dir,
            })
        })
    }

    pub fn create(root: &Path) -> Result<Self> {
        let dir = std::env::var("PYN_DIR").map_or_else(|_| root.join(DIR), PathBuf::from);
        for sub in ["state", "cache"] {
            let path = dir.join(LOCAL_ONLY).join(sub);
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating {}", path.display()))?;
        }
        Ok(Self {
            root: root.to_path_buf(),
            dir,
        })
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join(LOCAL_ONLY).join("config.toml")
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(LOCAL_ONLY).join("state").join("files.toml")
    }

    fn ignore_path(&self) -> PathBuf {
        self.dir.join("ignore")
    }

    fn personal_ignore_path(&self) -> PathBuf {
        self.dir.join(LOCAL_ONLY).join("ignore")
    }

    pub fn settings(&self) -> Settings {
        Settings::load(&self.config_path()).unwrap_or_default()
    }

    pub fn load_state(&self) -> Result<BTreeMap<String, FileState>> {
        match std::fs::read_to_string(self.state_path()) {
            Ok(text) => {
                let state: StateFile =
                    toml::from_str(&text).context("reading the workspace state")?;
                let mut files = state.files;
                if state.version < STATE_VERSION {
                    self.rekey_state(&mut files);
                }
                Ok(files)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Moves state recorded with SHA-256 content hashes to blob ids where the file on disk is still that content.
    fn rekey_state(&self, files: &mut BTreeMap<String, FileState>) {
        for (path, known) in files.iter_mut() {
            known.legacy = true;
            if let Ok(bytes) = std::fs::read(self.abs(path))
                && known.matches(&bytes)
            {
                known.hash = hash_of(&bytes);
                known.legacy = false;
            }
        }
    }

    pub fn save_state(&self, files: &BTreeMap<String, FileState>) -> Result<()> {
        let path = self.state_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(&StateFile {
            version: STATE_VERSION,
            files: files.clone(),
        })?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))
    }

    pub fn abs(&self, repo_path: &str) -> PathBuf {
        self.root.join(repo_path)
    }

    /// Turns a path typed relative to `cwd` into the repository path (`/`-separated, from the workspace root).
    pub fn repo_path(&self, cwd: &Path, arg: &str) -> Result<String> {
        let joined = if Path::new(arg).is_absolute() {
            PathBuf::from(arg)
        } else {
            cwd.join(arg)
        };
        let mut parts: Vec<String> = Vec::new();
        for component in joined.components() {
            match component {
                Component::ParentDir => {
                    parts.pop();
                }
                Component::CurDir => {}
                other => parts.push(other.as_os_str().to_string_lossy().into_owned()),
            }
        }
        let normalized: PathBuf = parts.iter().collect::<PathBuf>();
        let normalized = if joined.has_root() {
            PathBuf::from("/").join(normalized)
        } else {
            normalized
        };
        let rel = normalized.strip_prefix(&self.root).map_err(|_| {
            anyhow::anyhow!("{arg} is outside the workspace at {}", self.root.display())
        })?;
        let repo: Vec<_> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if repo.is_empty() {
            bail!("{arg} is the workspace root, not a file");
        }
        Ok(repo.join("/"))
    }

    /// Patterns from `.pyn/ignore` plus the personal `.pyn/local_only/ignore`, one per line; `#` starts a comment.
    /// `dir/` ignores a folder anywhere, a pattern without a `/` matches at any depth, and a pattern with one is
    /// relative to the workspace root.
    pub fn ignore_set(&self) -> Result<GlobSet> {
        let mut text = std::fs::read_to_string(self.ignore_path()).unwrap_or_default();
        text.push('\n');
        text.push_str(&std::fs::read_to_string(self.personal_ignore_path()).unwrap_or_default());
        build_ignore(&text)
    }

    /// Every file in the workspace that is not ignored and not local-only, as repository paths.
    pub fn scan(&self) -> Result<Vec<String>> {
        let ignore = self.ignore_set()?;
        let local_only = self.dir.join(LOCAL_ONLY);
        let mut found = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in
                std::fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if path == local_only {
                    continue;
                }
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    stack.push(path);
                } else if kind.is_file() || kind.is_symlink() {
                    let rel = path.strip_prefix(&self.root)?;
                    let repo = rel
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    if !ignore.is_match(&repo) {
                        found.push(repo);
                    }
                }
            }
        }
        found.sort();
        Ok(found)
    }
}

pub fn build_ignore(text: &str) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        let globs: Vec<String> = if let Some(dir) = line.strip_suffix('/') {
            if dir.contains('/') {
                vec![format!("{dir}/**")]
            } else {
                vec![format!("{dir}/**"), format!("**/{dir}/**")]
            }
        } else if line.contains('/') {
            vec![line.trim_start_matches('/').to_string()]
        } else {
            vec![format!("**/{line}")]
        };
        for glob in globs {
            builder.add(Glob::new(&glob).with_context(|| format!("bad ignore pattern {line:?}"))?);
        }
    }
    Ok(builder.build()?)
}

/// Exclusive files are read-only unless the person holds the lock; shared files are always writable.
pub fn set_read_only(path: &Path, read_only: bool) -> Result<()> {
    if crate::localfile::is_symlink(path) {
        return Ok(());
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    let mut perms = meta.permissions();
    perms.set_readonly(read_only);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("setting permissions on {}", path.display()))
}

/// The first of: the flag or environment, the workspace's own setting, the user's, then the default.
pub fn resolve(
    flag: Option<String>,
    workspace: Option<&String>,
    user: Option<&String>,
    default: Option<&str>,
) -> Option<String> {
    flag.or_else(|| workspace.cloned())
        .or_else(|| user.cloned())
        .or_else(|| default.map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(root: &str) -> Workspace {
        Workspace {
            root: PathBuf::from(root),
            dir: PathBuf::from(root).join(DIR),
        }
    }

    #[test]
    fn paths_are_taken_relative_to_where_you_stand() {
        let w = ws("/work/game");
        let at = |cwd: &str, arg: &str| w.repo_path(Path::new(cwd), arg).unwrap();
        assert_eq!(at("/work/game", "Source/a.cpp"), "Source/a.cpp");
        assert_eq!(
            at("/work/game/Content", "World/Main.umap"),
            "Content/World/Main.umap"
        );
        assert_eq!(
            at("/work/game/Content/World", "../Chars/k.uasset"),
            "Content/Chars/k.uasset"
        );
        assert_eq!(at("/work/game/Source", "./a.cpp"), "Source/a.cpp");
        assert_eq!(at("/elsewhere", "/work/game/Source/a.cpp"), "Source/a.cpp");
    }

    #[test]
    fn paths_outside_the_workspace_or_the_root_itself_are_refused() {
        let w = ws("/work/game");
        assert!(
            w.repo_path(Path::new("/work/game"), "../other/a.cpp")
                .is_err()
        );
        assert!(
            w.repo_path(Path::new("/work/game/Source"), "../..")
                .is_err()
        );
        assert!(w.repo_path(Path::new("/work/game"), ".").is_err());
    }

    #[test]
    fn ignore_patterns_follow_the_documented_rules() {
        let set = build_ignore(
            "# build output\n*.log\nnode_modules/\n/Saved/cache.bin\nIntermediate/Build/\n\n",
        )
        .unwrap();
        for ignored in [
            "a.log",
            "dir/deep/b.log",
            "node_modules/x/y.js",
            "pkg/node_modules/z.js",
            "Saved/cache.bin",
            "Intermediate/Build/o.obj",
        ] {
            assert!(set.is_match(ignored), "{ignored} should be ignored");
        }
        for kept in [
            "a.cpp",
            "Source/log.cpp",
            "other/Saved/cache.bin",
            "Intermediate/keep.txt",
            "x/Intermediate/Build/o.obj",
        ] {
            assert!(!set.is_match(kept), "{kept} should be kept");
        }
    }

    #[test]
    fn settings_layers_apply_in_order() {
        let (ws_v, user_v) = ("ws".to_string(), "user".to_string());
        assert_eq!(
            resolve(Some("flag".into()), Some(&ws_v), Some(&user_v), Some("d")).as_deref(),
            Some("flag")
        );
        assert_eq!(
            resolve(None, Some(&ws_v), Some(&user_v), Some("d")).as_deref(),
            Some("ws")
        );
        assert_eq!(
            resolve(None, None, Some(&user_v), Some("d")).as_deref(),
            Some("user")
        );
        assert_eq!(resolve(None, None, None, Some("d")).as_deref(), Some("d"));
        assert_eq!(resolve(None, None, None, None), None);
    }

    #[test]
    fn settings_reject_unknown_keys_and_round_trip() {
        let mut s = Settings::default();
        s.set("server", "http://x".into()).unwrap();
        assert!(s.set("colour", "red".into()).is_err());
        assert!(s.get("colour").is_err());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        std::fs::write(&path, "colour = \"red\"\n").unwrap();
        assert!(
            Settings::load(&path).is_err(),
            "a typo in the file is an error"
        );
    }

    #[test]
    fn scan_skips_local_only_and_ignored_files() {
        let dir = tempfile::tempdir().unwrap();
        let w = Workspace::create(dir.path()).unwrap();
        for f in [
            "a.cpp",
            "Source/b.cpp",
            "debug.log",
            "build/out.o",
            ".pyn/ignore",
            ".pyn/local_only/cache/x",
        ] {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        std::fs::write(dir.path().join(".pyn/ignore"), "*.log\nbuild/\n").unwrap();
        assert_eq!(w.scan().unwrap(), [".pyn/ignore", "Source/b.cpp", "a.cpp"]);
    }

    #[test]
    fn personal_ignore_adds_to_the_shared_file_without_listing_local_only() {
        let dir = tempfile::tempdir().unwrap();
        let w = Workspace::create(dir.path()).unwrap();
        for f in ["a.cpp", "notes.txt", "debug.log", "scratch/x"] {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x").unwrap();
        }
        std::fs::write(dir.path().join(".pyn/ignore"), "*.log\n").unwrap();
        std::fs::write(
            dir.path().join(".pyn/local_only/ignore"),
            "# mine\nnotes.txt\nscratch/\n",
        )
        .unwrap();
        assert_eq!(w.scan().unwrap(), [".pyn/ignore", "a.cpp"]);
    }

    #[test]
    fn state_with_sha256_hashes_is_rekeyed_where_the_file_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::create(dir.path()).unwrap();
        std::fs::write(dir.path().join("same.txt"), b"hello").unwrap();
        std::fs::write(dir.path().join("edited.txt"), b"changed").unwrap();
        let old = |content: &[u8]| FileState {
            revision: 1,
            hash: hex::encode(Sha256::digest(content)),
            legacy: false,
            mode: "shared".into(),
            file_mode: api::FileMode::Regular,
        };
        let text = format!(
            "[files.\"same.txt\"]\nrevision = 1\nhash = \"{}\"\nmode = \"shared\"\n\n[files.\"edited.txt\"]\nrevision = 1\nhash = \"{}\"\nmode = \"shared\"\n",
            old(b"hello").hash,
            old(b"original").hash
        );
        std::fs::write(ws.state_path(), text).unwrap();

        let state = ws.load_state().unwrap();
        assert_eq!(state["same.txt"].hash, hash_of(b"hello"));
        assert!(state["same.txt"].matches(b"hello") && !state["same.txt"].legacy);
        assert!(state["edited.txt"].legacy);
        assert!(!state["edited.txt"].matches(b"changed"));
        assert!(state["edited.txt"].matches(b"original"));

        ws.save_state(&state).unwrap();
        assert_eq!(ws.load_state().unwrap(), state);
    }
}

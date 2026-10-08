use std::collections::HashMap;

use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};

use crate::error::{PynError, Result};
use crate::types::RepoPath;

/// Collaboration policy for a path, not a file-type classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Shared,
    Exclusive,
}

/// Parsed `pyn.toml`; see docs/concepts/pyn-toml.md.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default)]
    meta: Meta,
    #[serde(default)]
    exclusive: Section,
    #[serde(default)]
    shared: Section,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    #[serde(default = "default_mode")]
    default: Mode,
}

impl Default for Meta {
    fn default() -> Self {
        Self {
            default: default_mode(),
        }
    }
}

fn default_mode() -> Mode {
    Mode::Exclusive
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    #[serde(default)]
    paths: Vec<String>,
}

/// Precedence, most specific first: exact file, glob (`exclusive` wins a conflict), deepest folder,
/// `meta.default`. The same entry in both lists is a load error.
#[derive(Debug, Clone)]
pub struct Rules {
    default: Mode,
    exact: HashMap<String, Mode>,
    /// Folder entries stored with a trailing `/`.
    dirs: Vec<(String, Mode)>,
    globs: Vec<(GlobMatcher, Mode)>,
}

enum Entry<'a> {
    Exact(&'a str),
    Dir(&'a str),
    Glob(&'a str),
}

fn classify(raw: &str) -> Result<Entry<'_>> {
    let bad = |why: &str| PynError::InvalidRules(format!("{raw:?}: {why}"));
    if raw.is_empty() {
        return Err(bad("empty entry"));
    }
    if raw.starts_with('/') || raw.starts_with("./") || raw.contains('\\') || raw.contains('\0') {
        return Err(bad(
            "paths are repo-relative with '/' separators and no leading '/' or './'",
        ));
    }
    if raw.split('/').any(|seg| seg == ".." || seg == ".") {
        return Err(bad("'.' and '..' segments are not allowed"));
    }
    let is_glob = raw.contains(['*', '?', '[', '{']);
    if raw.ends_with('/') {
        if is_glob || raw == "/" {
            return Err(bad("a folder entry cannot contain glob characters"));
        }
        return Ok(Entry::Dir(raw));
    }
    Ok(if is_glob {
        Entry::Glob(raw)
    } else {
        Entry::Exact(raw)
    })
}

impl Rules {
    pub fn with_default(default: Mode) -> Self {
        Self {
            default,
            exact: HashMap::new(),
            dirs: Vec::new(),
            globs: Vec::new(),
        }
    }

    pub fn empty() -> Self {
        Self::with_default(default_mode())
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let file: ConfigFile =
            toml::from_str(text).map_err(|e| PynError::InvalidRules(e.to_string()))?;
        let mut rules = Self::with_default(file.meta.default);
        for (mode, section) in [
            (Mode::Exclusive, &file.exclusive),
            (Mode::Shared, &file.shared),
        ] {
            for raw in &section.paths {
                rules.add(raw, mode)?;
            }
        }
        rules.dirs.sort_by_key(|(d, _)| std::cmp::Reverse(d.len()));
        Ok(rules)
    }

    fn add(&mut self, raw: &str, mode: Mode) -> Result<()> {
        let dup = |other: Mode| {
            (other != mode).then(|| {
                PynError::InvalidRules(format!("{raw:?} is listed as both exclusive and shared"))
            })
        };
        match classify(raw)? {
            Entry::Exact(p) => {
                if let Some(prev) = self.exact.insert(p.to_string(), mode) {
                    dup(prev).map_or(Ok(()), Err)?;
                }
            }
            Entry::Dir(d) => {
                if let Some((_, prev)) = self.dirs.iter().find(|(x, _)| x == d) {
                    dup(*prev).map_or(Ok(()), Err)?;
                } else {
                    self.dirs.push((d.to_string(), mode));
                }
            }
            Entry::Glob(g) => {
                let glob = GlobBuilder::new(g)
                    .literal_separator(true)
                    .build()
                    .map_err(|e| PynError::InvalidRules(format!("{raw:?}: {e}")))?;
                self.globs.push((glob.compile_matcher(), mode));
            }
        }
        Ok(())
    }

    pub fn default_mode(&self) -> Mode {
        self.default
    }

    pub fn mode_for(&self, path: &RepoPath) -> Mode {
        let p = path.as_str();
        if let Some(mode) = self.exact.get(p) {
            return *mode;
        }
        let mut glob_hit = None;
        for (matcher, mode) in &self.globs {
            if matcher.is_match(p) {
                if *mode == Mode::Exclusive {
                    return Mode::Exclusive;
                }
                glob_hit = Some(*mode);
            }
        }
        if let Some(mode) = glob_hit {
            return mode;
        }
        // `dirs` is sorted deepest-first.
        self.dirs
            .iter()
            .find(|(d, _)| p.starts_with(d.as_str()))
            .map_or(self.default, |(_, m)| *m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: &str = r#"
[meta]
default = "exclusive"

[exclusive]
paths = [
  "Content/",
  "Config/ProductionConfig.cpp",
]

[shared]
paths = [
  "Source/",
  "docs/",
  "README.md",
]
"#;

    fn p(s: &str) -> RepoPath {
        RepoPath::new(s).unwrap()
    }

    fn rules(text: &str) -> Rules {
        Rules::from_toml(text).unwrap()
    }

    #[test]
    fn files_and_folders_classify_as_listed() {
        let r = rules(GAME);
        assert_eq!(r.mode_for(&p("Source/Player.cpp")), Mode::Shared);
        assert_eq!(r.mode_for(&p("Source/deep/er/Enemy.cpp")), Mode::Shared);
        assert_eq!(r.mode_for(&p("README.md")), Mode::Shared);
        assert_eq!(r.mode_for(&p("Content/World/Main.umap")), Mode::Exclusive);
        assert_eq!(
            r.mode_for(&p("Config/ProductionConfig.cpp")),
            Mode::Exclusive
        );
    }

    #[test]
    fn unlisted_paths_use_the_default_which_is_exclusive_unless_flipped() {
        assert_eq!(
            rules(GAME).mode_for(&p("Config/Development.cpp")),
            Mode::Exclusive
        );
        assert_eq!(Rules::empty().mode_for(&p("anything.txt")), Mode::Exclusive);
        assert_eq!(rules("").mode_for(&p("anything.txt")), Mode::Exclusive);

        let flipped = rules("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n");
        assert_eq!(flipped.mode_for(&p("Source/a.cpp")), Mode::Shared);
        assert_eq!(flipped.mode_for(&p("Content/a.umap")), Mode::Exclusive);
    }

    #[test]
    fn folder_entries_match_whole_segments_only() {
        let r = rules("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n");
        assert_eq!(r.mode_for(&p("Content/a.bin")), Mode::Exclusive);
        assert_eq!(r.mode_for(&p("ContentExtra/a.bin")), Mode::Shared);
        assert_eq!(
            r.mode_for(&p("Content")),
            Mode::Shared,
            "a file named like the folder is not inside it"
        );
    }

    #[test]
    fn exact_beats_glob_beats_folder_and_deeper_folder_wins() {
        let r = rules(
            r#"
[exclusive]
paths = ["Content/", "**/*.psd", "docs/keep.md"]
[shared]
paths = ["Content/docs/", "**/*.md", "docs/"]
"#,
        );
        assert_eq!(
            r.mode_for(&p("Content/docs/a.bin")),
            Mode::Shared,
            "deeper folder beats shallower"
        );
        assert_eq!(
            r.mode_for(&p("Content/a.md")),
            Mode::Shared,
            "glob beats folder"
        );
        assert_eq!(
            r.mode_for(&p("docs/keep.md")),
            Mode::Exclusive,
            "exact beats glob"
        );
        assert_eq!(
            r.mode_for(&p("docs/art/x.psd")),
            Mode::Exclusive,
            "glob beats folder"
        );
    }

    #[test]
    fn conflicting_globs_resolve_to_exclusive() {
        let r = rules("[exclusive]\npaths = [\"**/*.dat\"]\n[shared]\npaths = [\"data/**\"]\n");
        assert_eq!(r.mode_for(&p("data/a.dat")), Mode::Exclusive);
        assert_eq!(r.mode_for(&p("data/a.txt")), Mode::Shared);
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        let r = rules("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"*.bin\"]\n");
        assert_eq!(r.mode_for(&p("a.bin")), Mode::Exclusive);
        assert_eq!(r.mode_for(&p("dir/a.bin")), Mode::Shared);
    }

    #[test]
    fn mistakes_are_rejected_rather_than_ignored() {
        for (why, text) in [
            ("typo'd table", "[exlusive]\npaths = [\"a\"]\n"),
            ("typo'd key", "[exclusive]\npath = [\"a\"]\n"),
            ("bad default", "[meta]\ndefault = \"sideways\"\n"),
            (
                "same file in both lists",
                "[exclusive]\npaths = [\"a.txt\"]\n[shared]\npaths = [\"a.txt\"]\n",
            ),
            (
                "same folder in both lists",
                "[exclusive]\npaths = [\"d/\"]\n[shared]\npaths = [\"d/\"]\n",
            ),
            ("bad glob", "[exclusive]\npaths = [\"[\"]\n"),
            ("absolute path", "[exclusive]\npaths = [\"/etc/passwd\"]\n"),
            ("dotdot", "[exclusive]\npaths = [\"a/../b\"]\n"),
            ("empty entry", "[exclusive]\npaths = [\"\"]\n"),
            ("glob folder", "[exclusive]\npaths = [\"a*/\"]\n"),
            ("not toml", "rules: []"),
        ] {
            assert!(Rules::from_toml(text).is_err(), "{why} should be rejected");
        }
    }

    #[test]
    fn repeating_an_entry_in_the_same_list_is_harmless() {
        let r = rules("[exclusive]\npaths = [\"a.txt\", \"a.txt\", \"d/\", \"d/\"]\n");
        assert_eq!(r.mode_for(&p("a.txt")), Mode::Exclusive);
    }
}

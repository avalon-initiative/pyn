//! The collaboration policy (`.pyn/pyn.toml`) as a client reads it from a commit in the local store, to give unpublished
//! content the mode the server will give it. The server stays the authority; this mirrors its matching rules.

use std::collections::HashMap;

use globset::{GlobBuilder, GlobMatcher};
use pyn_proto as api;
use serde::Deserialize;

pub const POLICY_PATH: &str = ".pyn/pyn.toml";

#[derive(Debug, Default, Deserialize)]
struct File {
    #[serde(default)]
    meta: Meta,
    #[serde(default)]
    exclusive: Section,
    #[serde(default)]
    shared: Section,
}

#[derive(Debug, Default, Deserialize)]
struct Meta {
    default: Option<api::Mode>,
}

#[derive(Debug, Default, Deserialize)]
struct Section {
    #[serde(default)]
    paths: Vec<String>,
}

/// Most specific first: exact file, glob (`exclusive` wins), deepest folder, then `meta.default`.
pub struct Policy {
    default: api::Mode,
    exact: HashMap<String, api::Mode>,
    dirs: Vec<(String, api::Mode)>,
    globs: Vec<(GlobMatcher, api::Mode)>,
}

impl Policy {
    /// The policy of a repository that has no policy file: everything exclusive.
    pub fn none() -> Self {
        Self::build(File::default()).expect("the empty policy is valid")
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::build(toml::from_str(text).ok()?)
    }

    fn build(file: File) -> Option<Self> {
        let mut policy = Self {
            default: file.meta.default.unwrap_or(api::Mode::Exclusive),
            exact: HashMap::new(),
            dirs: Vec::new(),
            globs: Vec::new(),
        };
        for (mode, section) in [
            (api::Mode::Exclusive, &file.exclusive),
            (api::Mode::Shared, &file.shared),
        ] {
            for raw in &section.paths {
                if raw.contains('*') || raw.contains('?') || raw.contains('[') || raw.contains('{')
                {
                    let glob = GlobBuilder::new(raw).literal_separator(true).build().ok()?;
                    policy.globs.push((glob.compile_matcher(), mode));
                } else if raw.ends_with('/') {
                    policy.dirs.push((raw.clone(), mode));
                } else {
                    policy.exact.insert(raw.clone(), mode);
                }
            }
        }
        policy.dirs.sort_by_key(|(d, _)| std::cmp::Reverse(d.len()));
        Some(policy)
    }

    pub fn mode_for(&self, path: &str) -> api::Mode {
        if let Some(mode) = self.exact.get(path) {
            return *mode;
        }
        let mut hit = None;
        for (matcher, mode) in &self.globs {
            if matcher.is_match(path) {
                if *mode == api::Mode::Exclusive {
                    return *mode;
                }
                hit = Some(*mode);
            }
        }
        if let Some(mode) = hit {
            return mode;
        }
        if let Some((_, mode)) = self.dirs.iter().find(|(d, _)| path.starts_with(d.as_str())) {
            return *mode;
        }
        if path == POLICY_PATH {
            return api::Mode::Exclusive;
        }
        self.default
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use api::Mode::{Exclusive, Shared};

    #[test]
    fn a_missing_policy_makes_everything_exclusive() {
        assert_eq!(Policy::none().mode_for("a/b.txt"), Exclusive);
    }

    #[test]
    fn the_default_and_the_listed_paths_decide() {
        let p = Policy::parse(
            "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\", \"x.bin\", \"**/*.uasset\"]\n[shared]\npaths = [\"Content/notes/\"]\n",
        )
        .unwrap();
        assert_eq!(p.mode_for("Source/a.cpp"), Shared);
        assert_eq!(p.mode_for("Content/m.umap"), Exclusive);
        assert_eq!(p.mode_for("Content/notes/a.txt"), Shared);
        assert_eq!(p.mode_for("x.bin"), Exclusive);
        assert_eq!(p.mode_for("deep/er/a.uasset"), Exclusive);
        assert_eq!(p.mode_for(".pyn/pyn.toml"), Exclusive);
    }

    #[test]
    fn a_broken_file_is_not_a_policy() {
        assert!(Policy::parse("[meta\n").is_none());
    }
}

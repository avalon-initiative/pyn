use crate::wildmatch;

/// Repository paths (`/`-separated, from the root) that limit a comparison to themselves and what is under them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pathspec {
    entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Path(Vec<u8>),
    /// A pattern whose `*` also crosses `/`, as in a plain git pathspec; `base` is the folder it is rooted in.
    Glob {
        pattern: Vec<u8>,
        base: Vec<u8>,
    },
}

impl Pathspec {
    pub fn new(paths: impl IntoIterator<Item = impl Into<Vec<u8>>>) -> Self {
        Self::build(paths, false)
    }

    /// Like [`Self::new`], but a path with `*`, `?`, `[` or a backslash is a glob.
    pub fn with_globs(paths: impl IntoIterator<Item = impl Into<Vec<u8>>>) -> Self {
        Self::build(paths, true)
    }

    fn build(paths: impl IntoIterator<Item = impl Into<Vec<u8>>>, globs: bool) -> Self {
        let entries = paths
            .into_iter()
            .map(Into::into)
            .map(|p: Vec<u8>| trim_slashes(&p).to_vec())
            .filter(|p| !p.is_empty())
            .map(|p| match p.iter().position(|b| b"*?[\\".contains(b)) {
                Some(at) if globs => {
                    let end = p[..at].iter().rposition(|b| *b == b'/').unwrap_or(0);
                    Entry::Glob {
                        base: p[..end].to_vec(),
                        pattern: p,
                    }
                }
                _ => Entry::Path(p),
            })
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn matches(&self, path: &[u8]) -> bool {
        self.matching(path, false)
    }

    /// Like [`Self::matches`] for a folder, which a glob also matches with a trailing slash (`sub/*` takes `sub`).
    pub fn matches_dir(&self, path: &[u8]) -> bool {
        self.matching(path, true)
    }

    fn matching(&self, path: &[u8], dir: bool) -> bool {
        self.entries.is_empty()
            || self.entries.iter().any(|e| match e {
                Entry::Path(p) => is_under(path, p),
                Entry::Glob { pattern, .. } => {
                    is_under(path, pattern)
                        || wildmatch::matches(pattern, path, false)
                        || (dir && wildmatch::matches(pattern, &[path, b"/"].concat(), false))
                }
            })
    }

    /// Whether a glob could match something under the folder, judging only by the literal text before its first
    /// wildcard (so `*.log` leads into every folder, `u*` only into folders starting with `u`).
    pub fn glob_leads_into(&self, dir: &[u8]) -> bool {
        self.entries.iter().any(|e| match e {
            Entry::Glob { pattern, .. } => {
                let at = pattern
                    .iter()
                    .position(|b| b"*?[\\".contains(b))
                    .unwrap_or(0);
                let literal = &pattern[..at];
                dir.starts_with(literal) || is_under(literal, dir)
            }
            Entry::Path(_) => false,
        })
    }

    /// Whether anything under the folder `dir` (no trailing slash) can match.
    pub fn may_contain(&self, dir: &[u8]) -> bool {
        self.entries.is_empty()
            || self.entries.iter().any(|e| match e {
                Entry::Path(p) | Entry::Glob { base: p, .. } => {
                    p.is_empty() || is_under(dir, p) || is_under(p, dir)
                }
            })
    }
}

fn trim_slashes(p: &[u8]) -> &[u8] {
    let mut p = p;
    while let Some(rest) = p.strip_suffix(b"/") {
        p = rest;
    }
    p
}

fn is_under(path: &[u8], prefix: &[u8]) -> bool {
    path == prefix || (path.starts_with(prefix) && path.get(prefix.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_files_and_folders_but_not_name_prefixes() {
        let s = Pathspec::new(["src", "a.txt/"]);
        assert!(s.matches(b"src/x.rs") && s.matches(b"src") && s.matches(b"a.txt"));
        assert!(!s.matches(b"srcx/y") && !s.matches(b"a.txt.bak"));
        assert!(Pathspec::default().matches(b"anything"));
    }

    #[test]
    fn globs_cross_folders_and_are_rooted_at_their_literal_folder() {
        let s = Pathspec::with_globs(["*.txt", "src/a*"]);
        assert!(s.matches(b"x.txt") && s.matches(b"deep/x.txt") && s.matches(b"src/abc/y"));
        assert!(!s.matches(b"x.log") && !s.matches(b"src/b"));
        let s = Pathspec::with_globs(["src/*.rs"]);
        assert!(s.may_contain(b"src") && s.may_contain(b"src/deep") && !s.may_contain(b"docs"));
        let s = Pathspec::with_globs(["src/*"]);
        assert!(!s.matches(b"src") && s.matches_dir(b"src") && !s.matches_dir(b"docs"));
        let literal = Pathspec::new(["*.txt"]);
        assert!(literal.matches(b"*.txt") && !literal.matches(b"a.txt"));
    }

    #[test]
    fn folders_on_the_way_to_a_spec_may_contain_it() {
        let s = Pathspec::new(["src/deep/mod.rs"]);
        assert!(s.may_contain(b"src") && s.may_contain(b"src/deep"));
        assert!(!s.may_contain(b"docs") && !s.may_contain(b"src/other"));
    }
}

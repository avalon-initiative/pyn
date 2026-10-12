/// Repository paths (`/`-separated, from the root) that limit a comparison to themselves and what is under them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pathspec {
    prefixes: Vec<Vec<u8>>,
}

impl Pathspec {
    pub fn new(paths: impl IntoIterator<Item = impl Into<Vec<u8>>>) -> Self {
        Self {
            prefixes: paths
                .into_iter()
                .map(Into::into)
                .map(|p: Vec<u8>| trim_slashes(&p).to_vec())
                .filter(|p| !p.is_empty())
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.prefixes.is_empty()
    }

    pub fn matches(&self, path: &[u8]) -> bool {
        self.prefixes.is_empty() || self.prefixes.iter().any(|p| is_under(path, p))
    }

    /// Whether anything under the folder `dir` (no trailing slash) can match.
    pub fn may_contain(&self, dir: &[u8]) -> bool {
        self.prefixes.is_empty()
            || self
                .prefixes
                .iter()
                .any(|p| is_under(dir, p) || is_under(p, dir))
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
    fn folders_on_the_way_to_a_spec_may_contain_it() {
        let s = Pathspec::new(["src/deep/mod.rs"]);
        assert!(s.may_contain(b"src") && s.may_contain(b"src/deep"));
        assert!(!s.may_contain(b"docs") && !s.may_contain(b"src/other"));
    }
}

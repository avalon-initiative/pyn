use std::fmt;

use crate::error::{Result, invalid};

/// A refspec: `[+]<src>[:<dst>]` or the negative `^<src>`, each side with at most one `*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refspec {
    pub force: bool,
    pub negative: bool,
    pub src: String,
    pub dst: Option<String>,
}

fn valid_side(side: &str) -> bool {
    !side.contains("..")
        && !side.contains("//")
        && !side.contains("@{")
        && !side.starts_with('/')
        && !side.ends_with('/')
        && !side.ends_with('.')
        && !side
            .chars()
            .any(|c| c.is_control() || " ~^:?[\\".contains(c))
        && side.matches('*').count() <= 1
}

fn split_glob(side: &str) -> Option<(&str, &str)> {
    side.split_once('*')
}

impl Refspec {
    pub fn parse(spec: &str) -> Result<Self> {
        let bad = |why: &str| Err(invalid("refspec", format!("{spec:?}: {why}")));
        let (negative, rest) = match spec.strip_prefix('^') {
            Some(r) => (true, r),
            None => (false, spec),
        };
        let (force, rest) = match rest.strip_prefix('+') {
            Some(r) if !negative => (true, r),
            _ => (false, rest),
        };
        let (src, dst) = match rest.split_once(':') {
            Some((s, d)) => (s, Some(d)),
            None => (rest, None),
        };
        if negative && (dst.is_some() || src.is_empty()) {
            return bad("a negative refspec names only a source");
        }
        if !valid_side(src) || dst.is_some_and(|d| !valid_side(d)) {
            return bad("not a valid ref name");
        }
        let src_glob = src.contains('*');
        let dst_glob = dst.is_some_and(|d| d.contains('*'));
        if dst_glob && !src_glob || (src_glob && dst.is_some_and(|d| !d.is_empty()) && !dst_glob) {
            return bad("both sides must have a `*`, or neither");
        }
        Ok(Self {
            force,
            negative,
            src: src.to_owned(),
            dst: dst.map(str::to_owned),
        })
    }

    pub fn is_pattern(&self) -> bool {
        self.src.contains('*')
    }

    /// Whether the source side names `name`.
    pub fn matches_src(&self, name: &str) -> bool {
        match split_glob(&self.src) {
            Some((pre, post)) => {
                name.len() >= pre.len() + post.len()
                    && name.starts_with(pre)
                    && name.ends_with(post)
            }
            None => self.src == name,
        }
    }

    /// The destination `name` maps to, for a source ref that matches.
    pub fn map(&self, name: &str) -> Option<String> {
        if self.negative || !self.matches_src(name) {
            return None;
        }
        let dst = self.dst.as_deref().filter(|d| !d.is_empty())?;
        match (split_glob(&self.src), split_glob(dst)) {
            (Some((pre, post)), Some((dpre, dpost))) => {
                let middle = &name[pre.len()..name.len() - post.len()];
                Some(format!("{dpre}{middle}{dpost}"))
            }
            _ => Some(dst.to_owned()),
        }
    }

    /// The source a destination ref came from, for a destination that matches.
    pub fn source_of(&self, dst_name: &str) -> Option<String> {
        if self.negative {
            return None;
        }
        let dst = self.dst.as_deref()?;
        match (split_glob(&self.src), split_glob(dst)) {
            (Some((pre, post)), Some((dpre, dpost))) => {
                let fits = dst_name.len() >= dpre.len() + dpost.len()
                    && dst_name.starts_with(dpre)
                    && dst_name.ends_with(dpost);
                fits.then(|| {
                    let middle = &dst_name[dpre.len()..dst_name.len() - dpost.len()];
                    format!("{pre}{middle}{post}")
                })
            }
            _ => (dst == dst_name).then(|| self.src.clone()),
        }
    }

    /// The refspec `git remote add` writes: `+refs/heads/*:refs/remotes/<name>/*`.
    pub fn default_fetch(remote: &str) -> Self {
        Self {
            force: true,
            negative: false,
            src: "refs/heads/*".into(),
            dst: Some(format!("refs/remotes/{remote}/*")),
        }
    }
}

/// Where the first matching positive refspec sends `name`, unless a negative refspec excludes it.
pub fn map_ref(specs: &[Refspec], name: &str) -> Option<String> {
    if specs.iter().any(|s| s.negative && s.matches_src(name)) {
        return None;
    }
    specs.iter().find_map(|s| s.map(name))
}

impl fmt::Display for Refspec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negative {
            f.write_str("^")?;
        } else if self.force {
            f.write_str("+")?;
        }
        f.write_str(&self.src)?;
        if let Some(dst) = &self.dst {
            write!(f, ":{dst}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_every_form() {
        for text in [
            "+refs/heads/*:refs/remotes/o/*",
            "refs/heads/main:refs/remotes/o/main",
            "refs/heads/main",
            "^refs/heads/wip",
            "^refs/heads/wip/*",
            ":refs/heads/gone",
            "HEAD:",
            "refs/tags/*",
        ] {
            assert_eq!(Refspec::parse(text).unwrap().to_string(), text);
        }
        let r = Refspec::parse("+a/*:b/*").unwrap();
        assert!(r.force && !r.negative && r.is_pattern());
    }

    #[test]
    fn rejects_malformed_specs() {
        for text in [
            "+^a",
            "^a:b",
            "^",
            "a*:b",
            "a:b*",
            "a/*/b/*:c/*",
            "a b:c",
            "a..b:c",
            "a:b:c",
            "a/:b",
        ] {
            assert!(Refspec::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn maps_globs_and_exact_names_both_ways() {
        let r = Refspec::parse("+refs/heads/*:refs/remotes/o/*").unwrap();
        assert_eq!(
            r.map("refs/heads/feat/x").as_deref(),
            Some("refs/remotes/o/feat/x")
        );
        assert_eq!(r.map("refs/tags/v1"), None);
        assert_eq!(
            r.source_of("refs/remotes/o/main").as_deref(),
            Some("refs/heads/main")
        );
        assert_eq!(r.source_of("refs/remotes/p/main"), None);
        let exact = Refspec::parse("refs/heads/a:refs/remotes/o/b").unwrap();
        assert_eq!(
            exact.map("refs/heads/a").as_deref(),
            Some("refs/remotes/o/b")
        );
        assert_eq!(exact.map("refs/heads/ab"), None);
        let suffix = Refspec::parse("refs/*/x:refs/y/*").unwrap();
        assert_eq!(suffix.map("refs/heads/x").as_deref(), Some("refs/y/heads"));
        assert_eq!(suffix.map("refs/heads/y"), None);
    }

    #[test]
    fn negative_specs_exclude_and_a_missing_dst_maps_nowhere() {
        let specs = [
            Refspec::parse("^refs/heads/wip/*").unwrap(),
            Refspec::parse("+refs/heads/*:refs/remotes/o/*").unwrap(),
        ];
        assert_eq!(map_ref(&specs, "refs/heads/wip/a"), None);
        assert_eq!(
            map_ref(&specs, "refs/heads/a").as_deref(),
            Some("refs/remotes/o/a")
        );
        assert_eq!(
            Refspec::parse("refs/heads/a").unwrap().map("refs/heads/a"),
            None
        );
        assert_eq!(
            Refspec::default_fetch("up").to_string(),
            "+refs/heads/*:refs/remotes/up/*"
        );
    }
}

//! Commit references: `HEAD`, a commit hash or an abbreviation of one, each followed by `~N` or `^` steps.

use anyhow::{Result, bail};

use crate::backend::Backend;

/// Hex digits shown for an abbreviated commit id.
pub const ABBREV: usize = 10;

/// The first `ABBREV` digits of a commit id.
pub fn short(id: &str) -> &str {
    &id[..ABBREV.min(id.len())]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    Head,
    /// A full id or an abbreviation of at least four hex digits.
    Hash(String),
}

/// A starting commit and how many parents to walk back from it (`~N` and `^` both count along the one mainline).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef {
    pub base: Base,
    pub back: u64,
}

fn is_hash(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `HEAD` or a hash, then any of `~N`, `~`, `^` and `^1`.
pub fn parse_commit(s: &str) -> Option<CommitRef> {
    let end = s.find(['~', '^']).unwrap_or(s.len());
    let base = match &s[..end] {
        "HEAD" => Base::Head,
        hash if is_hash(hash) => Base::Hash(hash.to_string()),
        _ => return None,
    };
    let mut rest = &s[end..];
    let mut back = 0u64;
    while let Some(c) = rest.chars().next() {
        rest = &rest[1..];
        let digits = rest
            .find(|d: char| !d.is_ascii_digit())
            .unwrap_or(rest.len());
        let n: Option<u64> = rest[..digits].parse().ok();
        rest = &rest[digits..];
        match (c, n) {
            ('~', n) => back = back.checked_add(n.unwrap_or(1))?,
            ('^', None | Some(1)) => back = back.checked_add(1)?,
            ('^', Some(0)) => {}
            _ => return None,
        }
    }
    Some(CommitRef { base, back })
}

/// The full id of the commit `text` names.
pub fn resolve(backend: &dyn Backend, text: &str) -> Result<String> {
    let Some(found) = parse_commit(text) else {
        bail!("{text} is not a commit; use HEAD, HEAD~N, HEAD^ or a commit hash");
    };
    let mut id = match &found.base {
        Base::Head => match backend.head_commit()? {
            Some(head) => head,
            None => bail!("the repository has no commits yet"),
        },
        Base::Hash(hash) => backend.commit(hash)?.id,
    };
    for _ in 0..found.back {
        match backend.commit(&id)?.parents.into_iter().next() {
            Some(parent) => id = parent,
            None => bail!("{text} is before the first commit"),
        }
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(back: u64) -> Option<CommitRef> {
        Some(CommitRef {
            base: Base::Head,
            back,
        })
    }

    #[test]
    fn commit_references_parse_like_git_ones() {
        assert_eq!(parse_commit("HEAD"), head(0));
        assert_eq!(parse_commit("HEAD~3"), head(3));
        assert_eq!(parse_commit("HEAD~"), head(1));
        assert_eq!(parse_commit("HEAD^"), head(1));
        assert_eq!(parse_commit("HEAD^^"), head(2));
        assert_eq!(parse_commit("HEAD^1"), head(1));
        assert_eq!(parse_commit("HEAD^0"), head(0));
        assert_eq!(parse_commit("HEAD~2^"), head(3));
        assert_eq!(parse_commit("HEAD^2"), None, "there is no second parent");
        assert_eq!(parse_commit("HEAD~x"), None);
        assert_eq!(
            parse_commit("1a2b3c4d~2"),
            Some(CommitRef {
                base: Base::Hash("1a2b3c4d".into()),
                back: 2
            })
        );
    }

    #[test]
    fn only_hex_of_four_or_more_digits_is_a_hash() {
        assert!(parse_commit("abcd").is_some());
        assert_eq!(parse_commit("abc"), None);
        assert_eq!(parse_commit("c12"), None, "cN numbers are gone");
        assert_eq!(parse_commit("ABCD"), None);
        assert_eq!(parse_commit("src/a.txt"), None);
        assert_eq!(parse_commit(&"a".repeat(65)), None);
    }

    #[test]
    fn short_ids_keep_ten_digits() {
        assert_eq!(short("0123456789abcdef"), "0123456789");
        assert_eq!(short("abc"), "abc");
    }
}

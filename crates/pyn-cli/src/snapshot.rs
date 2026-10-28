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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `~N`: N times the first parent.
    Back(u64),
    /// `^N`: the Nth parent; `^0` is the commit itself.
    Parent(u64),
}

/// A starting commit and the `~N` and `^N` steps to take from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRef {
    pub base: Base,
    pub steps: Vec<Step>,
}

impl CommitRef {
    /// Applies the steps to `start`, reading a commit's parents with `parents`.
    pub fn follow<T: Clone>(
        &self,
        text: &str,
        start: T,
        mut parents: impl FnMut(&T) -> Result<Vec<T>>,
    ) -> Result<T> {
        let mut at = start;
        for step in &self.steps {
            let (n, times) = match *step {
                Step::Back(n) => (1, n),
                Step::Parent(n) => (n, 1),
            };
            if n == 0 {
                continue;
            }
            for _ in 0..times {
                let all = parents(&at)?;
                let Some(next) = all.get(n as usize - 1) else {
                    if n == 1 {
                        bail!("{text} is before the first commit");
                    }
                    bail!("{text}: a commit has no parent {n}");
                };
                at = next.clone();
            }
        }
        Ok(at)
    }
}

fn is_hash(s: &str) -> bool {
    (4..=64).contains(&s.len()) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `HEAD` or a hash, then any of `~N`, `~`, `^` and `^N`.
pub fn parse_commit(s: &str) -> Option<CommitRef> {
    let end = s.find(['~', '^']).unwrap_or(s.len());
    let base = match &s[..end] {
        "HEAD" => Base::Head,
        hash if is_hash(hash) => Base::Hash(hash.to_string()),
        _ => return None,
    };
    let mut rest = &s[end..];
    let mut steps = Vec::new();
    while let Some(c) = rest.chars().next() {
        rest = &rest[1..];
        let digits = rest
            .find(|d: char| !d.is_ascii_digit())
            .unwrap_or(rest.len());
        let n: Option<u64> = rest[..digits].parse().ok();
        rest = &rest[digits..];
        match (c, n) {
            ('~', n) => steps.push(Step::Back(n.unwrap_or(1))),
            ('^', n) => steps.push(Step::Parent(n.unwrap_or(1))),
            _ => return None,
        }
    }
    Some(CommitRef { base, steps })
}

/// The full id of the commit `text` names.
pub fn resolve(backend: &dyn Backend, text: &str) -> Result<String> {
    let Some(found) = parse_commit(text) else {
        bail!("{text} is not a commit; use HEAD, HEAD~N, HEAD^ or a commit hash");
    };
    let id = match &found.base {
        Base::Head => match backend.head_commit()? {
            Some(head) => head,
            None => bail!("the repository has no commits yet"),
        },
        Base::Hash(hash) => backend.commit(hash)?.id,
    };
    found.follow(text, id, |id| Ok(backend.commit(id)?.parents))
}

#[cfg(test)]
mod tests {
    use super::*;

    use Step::{Back, Parent};

    fn head(steps: &[Step]) -> Option<CommitRef> {
        Some(CommitRef {
            base: Base::Head,
            steps: steps.to_vec(),
        })
    }

    #[test]
    fn commit_references_parse_like_git_ones() {
        assert_eq!(parse_commit("HEAD"), head(&[]));
        assert_eq!(parse_commit("HEAD~3"), head(&[Back(3)]));
        assert_eq!(parse_commit("HEAD~"), head(&[Back(1)]));
        assert_eq!(parse_commit("HEAD^"), head(&[Parent(1)]));
        assert_eq!(parse_commit("HEAD^^"), head(&[Parent(1), Parent(1)]));
        assert_eq!(parse_commit("HEAD^0"), head(&[Parent(0)]));
        assert_eq!(parse_commit("HEAD~2^2"), head(&[Back(2), Parent(2)]));
        assert_eq!(parse_commit("HEAD~x"), None);
        assert_eq!(
            parse_commit("1a2b3c4d~2"),
            Some(CommitRef {
                base: Base::Hash("1a2b3c4d".into()),
                steps: vec![Back(2)]
            })
        );
    }

    #[test]
    fn steps_follow_the_chosen_parent() {
        // 0 is a merge of 1 and 2; 1 has parent 3.
        let parents = |n: &u8| -> Result<Vec<u8>> {
            Ok(match n {
                0 => vec![1, 2],
                1 => vec![3],
                _ => vec![],
            })
        };
        let at = |t: &str| parse_commit(t).unwrap().follow(t, 0, parents);
        assert_eq!(at("HEAD^2").unwrap(), 2);
        assert_eq!(at("HEAD^1^").unwrap(), 3);
        assert_eq!(at("HEAD~2").unwrap(), 3);
        assert_eq!(at("HEAD^0").unwrap(), 0);
        assert!(
            at("HEAD^3")
                .unwrap_err()
                .to_string()
                .contains("no parent 3")
        );
        assert!(at("HEAD^2^").is_err());
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

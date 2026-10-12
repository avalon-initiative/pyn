//! Commit references.

use anyhow::{Result, bail};

use crate::backend::Backend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitRef {
    Number(u64),
    /// `HEAD~n`, counting back from the newest commit.
    Head(u64),
}

/// `HEAD`, `HEAD~n` or `cN`; with `bare`, also a plain number.
pub fn parse_commit(s: &str, bare: bool) -> Option<CommitRef> {
    if s == "HEAD" {
        return Some(CommitRef::Head(0));
    }
    if let Some(n) = s.strip_prefix("HEAD~") {
        return n.parse().ok().map(CommitRef::Head);
    }
    let digits = match s.strip_prefix('c') {
        Some(rest) => rest,
        None if bare => s,
        None => return None,
    };
    digits.parse().ok().map(CommitRef::Number)
}

pub fn resolve(backend: &dyn Backend, text: &str) -> Result<u64> {
    let Some(commit) = parse_commit(text, true) else {
        bail!("{text} is not a commit; use HEAD, HEAD~N or cN");
    };
    match commit {
        CommitRef::Number(n) => Ok(n),
        CommitRef::Head(back) => {
            let head = backend.head_commit()?;
            if head == 0 {
                bail!("the repository has no commits yet");
            }
            if back >= head {
                bail!("{text} is before the first commit (HEAD is c{head})");
            }
            Ok(head - back)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_references_parse_like_git_ones() {
        assert_eq!(parse_commit("HEAD", false), Some(CommitRef::Head(0)));
        assert_eq!(parse_commit("HEAD~3", false), Some(CommitRef::Head(3)));
        assert_eq!(parse_commit("c12", false), Some(CommitRef::Number(12)));
        assert_eq!(parse_commit("12", false), None);
        assert_eq!(parse_commit("12", true), Some(CommitRef::Number(12)));
        assert_eq!(parse_commit("HEAD~x", true), None);
        assert_eq!(parse_commit("src/a.txt", true), None);
    }
}

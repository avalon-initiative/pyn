//! The `lockable` lint: paths where the `.gitattributes` attribute and the `pyn.toml` mode disagree.
//! `pyn.toml` is the authority (decision #775); the lint changes no enforcement.

/// The policy file; always exclusive, so never linted.
pub const POLICY_PATH: &str = ".pyn/pyn.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disagreement {
    /// Marked `lockable`, but the policy makes it shared: git-lfs sees a lock, pyn takes only an advisory one.
    LockableShared,
    /// Exclusive in the policy, but not marked `lockable`: git-lfs leaves it writable.
    ExclusiveNotLockable,
}

impl Disagreement {
    pub fn label(self) -> &'static str {
        match self {
            Self::LockableShared => "lockable, shared",
            Self::ExclusiveNotLockable => "exclusive, not lockable",
        }
    }
}

/// Disagreeing paths in the order given, from `(path, lockable)` pairs. The policy file itself is always exclusive and skipped.
pub fn lint<'a>(
    paths: impl IntoIterator<Item = (&'a str, bool)>,
    is_exclusive: impl Fn(&str) -> bool,
) -> Vec<(&'a str, Disagreement)> {
    paths
        .into_iter()
        .filter(|(p, _)| *p != POLICY_PATH)
        .filter_map(|(p, lockable)| match (lockable, is_exclusive(p)) {
            (true, false) => Some((p, Disagreement::LockableShared)),
            (false, true) => Some((p, Disagreement::ExclusiveNotLockable)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_kinds_are_listed_and_the_policy_file_is_skipped() {
        let found = lint(
            [
                ("a.psd", false),
                ("b.txt", true),
                ("c.rs", false),
                (POLICY_PATH, false),
            ],
            |p| p == "a.psd",
        );
        assert_eq!(
            found,
            [
                ("a.psd", Disagreement::ExclusiveNotLockable),
                ("b.txt", Disagreement::LockableShared)
            ]
        );
    }
}

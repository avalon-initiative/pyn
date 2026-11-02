//! The `lockable` lint: paths where the `.gitattributes` attribute and the `pyn.toml` mode disagree.
//! `pyn.toml` is the authority (decision #775); the lint changes no enforcement.

use pyn_proto::Mode;

use crate::policy::POLICY_PATH;

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
    mode_for: impl Fn(&str) -> Mode,
) -> Vec<(&'a str, Disagreement)> {
    paths
        .into_iter()
        .filter(|(p, _)| *p != POLICY_PATH)
        .filter_map(|(p, lockable)| match (lockable, mode_for(p)) {
            (true, Mode::Shared) => Some((p, Disagreement::LockableShared)),
            (false, Mode::Exclusive) => Some((p, Disagreement::ExclusiveNotLockable)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Policy;
    use Disagreement::*;

    fn policy() -> Policy {
        Policy::parse(
            "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"art/\", \"**/*.uasset\"]\n",
        )
        .unwrap()
    }

    #[test]
    fn agreeing_paths_are_not_listed() {
        let p = policy();
        let found = lint(
            [
                ("art/a.psd", true),
                ("src/a.rs", false),
                ("m/x.uasset", true),
            ],
            |x| p.mode_for(x),
        );
        assert!(found.is_empty());
    }

    #[test]
    fn both_kinds_of_disagreement_are_listed() {
        let p = policy();
        let found = lint(
            [
                ("art/a.psd", false),
                ("docs/a.psd", true),
                ("src/a.rs", false),
            ],
            |x| p.mode_for(x),
        );
        assert_eq!(
            found,
            [
                ("art/a.psd", ExclusiveNotLockable),
                ("docs/a.psd", LockableShared)
            ]
        );
    }

    #[test]
    fn the_policy_file_is_never_listed() {
        let p = Policy::none();
        assert!(lint([(".pyn/pyn.toml", false)], |x| p.mode_for(x)).is_empty());
    }
}

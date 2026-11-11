//! The `lockable` lint over a `Policy`; the lint itself is shared with the server (`pyn-gitattr`).

use pyn_gitattr::lockable;
pub use pyn_gitattr::lockable::Disagreement;
use pyn_proto::Mode;

/// Disagreeing paths in the order given, from `(path, lockable)` pairs.
pub fn lint<'a>(
    paths: impl IntoIterator<Item = (&'a str, bool)>,
    mode_for: impl Fn(&str) -> Mode,
) -> Vec<(&'a str, Disagreement)> {
    lockable::lint(paths, |p| mode_for(p) == Mode::Exclusive)
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

//! Git commands pyn has no equivalent for yet: a clear message instead of a usage error.

const GIT_ONLY: &[&str] = &[
    "init",
    "add",
    "show",
    "branch",
    "switch",
    "merge",
    "fetch",
    "pull",
    "push",
    "tag",
    "stash",
    "remote",
    "reset",
    "rm",
    "mv",
    "rebase",
    "cherry-pick",
    "revert",
    "blame",
    "grep",
    "clean",
];

pub fn message(name: &str) -> String {
    if name == "checkout" {
        return "pyn: 'checkout' is reserved for switching branches and restoring paths, as in git, \
                which pyn does not support yet; to take an exclusive lock use `pyn lock <path>`."
            .into();
    }
    if GIT_ONLY.contains(&name) {
        format!(
            "pyn: '{name}' is a git command pyn does not support yet; see docs/concepts/git-compatibility.md."
        )
    } else {
        format!("pyn: '{name}' is not a pyn command. See 'pyn --help'.")
    }
}

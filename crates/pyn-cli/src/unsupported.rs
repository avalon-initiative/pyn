//! Git commands pyn has no equivalent for yet: a clear message instead of a usage error.

const GIT_ONLY: &[&str] = &[
    "fetch", "pull", "push", "tag", "stash", "reflog", "rebase", "blame", "grep", "clean",
];

/// A git command, or a pyn command that wraps one, that a git home will gain with a named issue.
const PLANNED_ON_GIT: &[(&str, &str)] = &[("update", "git transport (#255)")];

/// The commands a git home or a pyn server workspace has, so anywhere else refuses before parsing their arguments.
pub const REFUSED_OFF_GIT: &[&str] = &["fetch", "pull", "push"];

/// The commands whose arguments only make sense on a pyn server, so a git home refuses before parsing them.
pub const REFUSED_BEFORE_PARSING: &[&str] = &["update", "get"];

/// What to print for `name` in a git repository: which issue will provide it, or the general message.
pub fn message_on_git(name: &str) -> String {
    match PLANNED_ON_GIT.iter().find(|(n, _)| *n == name) {
        Some((_, what)) => {
            format!("pyn: '{name}' is not supported on a git repository yet; it needs {what}.")
        }
        None if name == "get" => {
            "pyn: 'get' reads a file at a pyn revision, which a git repository does not \
                                  have; see docs/concepts/git-compatibility.md."
                .into()
        }
        None => message(name),
    }
}

pub fn message(name: &str) -> String {
    if GIT_ONLY.contains(&name) {
        format!(
            "pyn: '{name}' is a git command pyn does not support yet; see docs/concepts/git-compatibility.md."
        )
    } else {
        format!("pyn: '{name}' is not a pyn command. See 'pyn --help'.")
    }
}

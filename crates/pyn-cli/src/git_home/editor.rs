//! Starts the user's editor on a file, as git does for a commit message and for a rebase todo list.

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};

use super::GitHome;
use super::stage::clean_bytes;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Which {
    Message,
    Sequence,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

fn dumb_terminal() -> bool {
    std::env::var("TERM").map_or(true, |t| t == "dumb")
}

/// The editor command: `GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`, then `vi`; a todo list first asks
/// `GIT_SEQUENCE_EDITOR` and `sequence.editor`.
fn command_for(git: &GitHome, which: Which) -> Result<String> {
    command_from(which, &|key| git.repo.config().get(key).map(str::to_string))
}

/// The editor command with `config` answering the configuration keys; a pyn server workspace has none.
pub(crate) fn command_from(
    which: Which,
    config: &dyn Fn(&str) -> Option<String>,
) -> Result<String> {
    let env = |name: &str| non_empty(std::env::var(name).ok());
    let config = |key: &str| non_empty(config(key));
    if which == Which::Sequence
        && let Some(editor) = env("GIT_SEQUENCE_EDITOR").or_else(|| config("sequence.editor"))
    {
        return Ok(editor);
    }
    if let Some(editor) = env("GIT_EDITOR").or_else(|| config("core.editor")) {
        return Ok(editor);
    }
    if dumb_terminal() {
        bail!("Terminal is dumb, but EDITOR unset");
    }
    Ok(env("VISUAL")
        .or_else(|| env("EDITOR"))
        .unwrap_or_else(|| "vi".into()))
}

/// Runs the editor on `path` through the shell and waits; `:` means "keep the file as it is".
pub(crate) fn run(git: &GitHome, which: Which, path: &Path) -> Result<()> {
    let editor = command_for(git, which)?;
    run_command(&editor, &git.ws.root, path)
}

pub(crate) fn run_command(editor: &str, dir: &Path, path: &Path) -> Result<()> {
    if editor == ":" {
        return Ok(());
    }
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(editor)
        .arg(path)
        .current_dir(dir)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => bail!("There was a problem with the editor '{editor}'."),
    }
}

/// What the editor left in a message file: comment lines removed, whitespace cleaned. `None` when nothing is left.
pub(crate) fn edited_message(text: &[u8]) -> Option<Vec<u8>> {
    let kept: Vec<&[u8]> = text
        .split(|b| *b == b'\n')
        .filter(|l| !l.starts_with(b"#"))
        .collect();
    let clean = clean_bytes(&kept.join(&b'\n'));
    (clean != b"\n").then_some(clean)
}

/// Lets the user edit `message` in `COMMIT_EDITMSG` under `comment` (lines starting with `#`); `None` for an empty result.
pub(crate) fn edit_message(
    git: &GitHome,
    message: &[u8],
    comment: &str,
) -> Result<Option<Vec<u8>>> {
    edit_message_with(git, message, comment, |_| Ok(()))
}

/// [`edit_message`] with `before` run on the message file between writing it and opening the editor.
pub(crate) fn edit_message_with(
    git: &GitHome,
    message: &[u8],
    comment: &str,
    before: impl FnOnce(&Path) -> Result<()>,
) -> Result<Option<Vec<u8>>> {
    let path = git.repo.git().git_dir().join("COMMIT_EDITMSG");
    let mut text = message.to_vec();
    if !text.is_empty() && !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    text.extend_from_slice(
        b"\n# Please enter the commit message for your changes. Lines starting\n# with '#' will be ignored, and an empty message aborts the commit.\n",
    );
    text.extend_from_slice(comment.as_bytes());
    std::fs::write(&path, &text)?;
    before(&path)?;
    run(git, Which::Message, &path)?;
    Ok(edited_message(&std::fs::read(&path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_blank_edges_are_dropped() {
        assert_eq!(
            edited_message(b"# note\n\nsubject  \n\n\nbody\n# more\n\n").unwrap(),
            b"subject\n\nbody\n"
        );
        assert!(edited_message(b"# only\n\n").is_none());
    }
}

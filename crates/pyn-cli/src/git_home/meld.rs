//! The messages of a `squash` / `fixup` chain in `.git/rebase-merge/`, written as git's sequencer writes them.

use std::path::Path;

use anyhow::Result;
use pyn_object::ObjectId;

use super::todo::Cmd;

pub(super) const SQUASH: &str = "message-squash";
pub(super) const FIXUP: &str = "message-fixup";
const FIXUPS: &str = "current-fixups";
const HEADER: &str = "# This is a combination of ";

fn completed(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

/// A squashed commit's own `squash!` / `fixup!` subject line is left out, with the blank lines after it.
fn without_marker(message: &str) -> &str {
    if !(message.starts_with("squash!") || message.starts_with("fixup!")) {
        return message;
    }
    let rest = message.split_once('\n').map_or("", |(_, rest)| rest);
    rest.trim_start_matches('\n')
}

fn commented(text: &str) -> String {
    completed(text)
        .lines()
        .map(|l| {
            if l.is_empty() {
                "#\n".into()
            } else {
                format!("# {l}\n")
            }
        })
        .collect()
}

fn ordinal(n: usize) -> String {
    match n {
        1 => "1st".into(),
        _ => format!("#{n}"),
    }
}

/// Adds a chain member to the message files and returns the combined message.
pub(super) fn add(
    dir: &Path,
    head_message: &str,
    message: &str,
    cmd: Cmd,
    id: &ObjectId,
) -> Result<String> {
    let squash_path = dir.join(SQUASH);
    let mut text = match std::fs::read_to_string(&squash_path) {
        Ok(text) => {
            let (first, rest) = text.split_once('\n').unwrap_or((&text, ""));
            let count = first
                .strip_prefix(HEADER)
                .and_then(|r| r.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(1)
                + 1;
            if cmd == Cmd::Squash {
                let _ = std::fs::remove_file(dir.join(FIXUP));
            }
            format!("{HEADER}{count} commits.\n{rest}")
        }
        Err(_) => {
            if cmd == Cmd::Fixup {
                std::fs::write(dir.join(FIXUP), completed(head_message))?;
            }
            format!(
                "{HEADER}2 commits.\n# This is the 1st commit message:\n\n{}",
                completed(head_message)
            )
        }
    };
    let count = count_of(&text);
    if cmd == Cmd::Squash {
        text.push_str(&format!(
            "\n# This is the commit message {}:\n\n{}",
            ordinal(count),
            completed(without_marker(message))
        ));
    } else {
        text.push_str(&format!(
            "\n# The commit message {} will be skipped:\n\n{}",
            ordinal(count),
            commented(message)
        ));
    }
    std::fs::write(&squash_path, &text)?;
    let mut fixups = std::fs::read_to_string(dir.join(FIXUPS)).unwrap_or_default();
    if !fixups.is_empty() {
        fixups.push('\n');
    }
    fixups.push_str(&format!("{} {id}", cmd.word()));
    std::fs::write(dir.join(FIXUPS), fixups)?;
    Ok(text)
}

fn count_of(text: &str) -> usize {
    text.strip_prefix(HEADER)
        .and_then(|r| r.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1)
}

/// Takes the newest member out of the chain; the chain is gone when only the first commit is left.
pub(super) fn drop_last(dir: &Path) {
    let Ok(text) = std::fs::read_to_string(dir.join(SQUASH)) else {
        return;
    };
    let cut = [
        "\n# This is the commit message #",
        "\n# The commit message #",
    ]
    .iter()
    .filter_map(|m| text.rfind(m))
    .max();
    let count = count_of(&text);
    let (Some(cut), true) = (cut, count > 2) else {
        clear(dir);
        return;
    };
    let rest = text[..cut].split_once('\n').map_or("", |(_, rest)| rest);
    let _ = std::fs::write(
        dir.join(SQUASH),
        format!("{HEADER}{} commits.\n{rest}", count - 1),
    );
    let fixups = std::fs::read_to_string(dir.join(FIXUPS)).unwrap_or_default();
    let kept: Vec<&str> = fixups.lines().collect();
    if let Some((_, init)) = kept.split_last() {
        let _ = std::fs::write(dir.join(FIXUPS), init.join("\n"));
    }
}

pub(super) fn clear(dir: &Path) {
    for name in [SQUASH, FIXUP, FIXUPS] {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ObjectId {
        ObjectId::from_hex(pyn_object::Algorithm::Sha1, &"a".repeat(40)).unwrap()
    }

    #[test]
    fn a_chain_collects_squashed_messages_and_comments_out_fixups() {
        let dir = tempfile::tempdir().unwrap();
        add(dir.path(), "first\n", "second\n", Cmd::Squash, &id()).unwrap();
        let text = add(dir.path(), "first\n", "third\nmore\n", Cmd::Fixup, &id()).unwrap();
        assert_eq!(
            text,
            "# This is a combination of 3 commits.\n# This is the 1st commit message:\n\nfirst\n\n# This is the commit message #2:\n\nsecond\n\n# The commit message #3 will be skipped:\n\n# third\n# more\n"
        );
        assert!(!dir.path().join(FIXUP).exists());
        drop_last(dir.path());
        let back = std::fs::read_to_string(dir.path().join(SQUASH)).unwrap();
        assert!(back.starts_with("# This is a combination of 2 commits.\n"));
        assert!(back.ends_with("second\n"));
    }

    #[test]
    fn a_leading_fixup_keeps_the_first_message_for_reuse() {
        let dir = tempfile::tempdir().unwrap();
        add(dir.path(), "first\n", "x\n", Cmd::Fixup, &id()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FIXUP)).unwrap(),
            "first\n"
        );
    }
}

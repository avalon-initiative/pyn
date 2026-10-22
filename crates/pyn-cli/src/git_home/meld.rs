//! The messages of a `squash` / `fixup` chain in `.git/rebase-merge/`, written as git's sequencer writes them.

use std::path::Path;

use anyhow::Result;
use pyn_object::ObjectId;

use super::todo::{Cmd, Flag};

pub(crate) const SQUASH: &str = "message-squash";
pub(crate) const FIXUP: &str = "message-fixup";
const FIXUPS: &str = "current-fixups";
const HEADER: &str = "# This is a combination of ";

fn completed(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

/// Git's commented lines: `# ` before each, `#` for an empty one, and a newline after each line that had one.
fn commented(text: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let bare = line.strip_suffix('\n');
        match bare.unwrap_or(line) {
            "" => out.push('#'),
            l => {
                out.push_str("# ");
                out.push_str(l);
            }
        }
        if bare.is_some() {
            out.push('\n');
        }
    }
    out
}

/// Bytes of the leading paragraph: the subject that is commented out of a squashed message.
fn subject_len(message: &str) -> usize {
    message
        .find("\n\n")
        .unwrap_or(message.trim_end_matches('\n').len())
}

fn ordinal(n: usize) -> String {
    match n {
        1 => "1st".into(),
        _ => format!("#{n}"),
    }
}

fn skipped_title(n: usize) -> String {
    match n {
        1 => "The 1st commit message will be skipped:".into(),
        _ => format!("The commit message #{n} will be skipped:"),
    }
}

/// Whether the chain so far holds a `squash`, after which `fixup -C` behaves like `squash`.
fn seen_squash(dir: &Path) -> bool {
    let fixups = std::fs::read_to_string(dir.join(FIXUPS)).unwrap_or_default();
    fixups.starts_with("squash") || fixups.contains("\nsquash")
}

fn is_title(line: &str) -> bool {
    let Some(t) = line.strip_prefix("# ") else {
        return false;
    };
    t.ends_with(':')
        && [
            "This is the 1st commit message",
            "This is the commit message #",
            "The 1st commit message will",
            "The commit message #",
        ]
        .iter()
        .any(|p| t.starts_with(p))
}

/// Turns every message still kept into a skipped one, as `fixup -C` and `-c` replace them.
fn skip_kept(text: &str) -> String {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let mut sections: Vec<(&str, String)> = Vec::new();
    for line in rest.split_inclusive('\n') {
        if is_title(line.trim_end_matches('\n')) {
            sections.push((line.trim_end_matches('\n'), String::new()));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push_str(line);
        }
    }
    let mut out = format!("{first}\n");
    let last = sections.len().saturating_sub(1);
    for (i, (title, body)) in sections.iter().enumerate() {
        let body = body.strip_prefix('\n').unwrap_or(body);
        let body = if i < last {
            body.strip_suffix('\n').unwrap_or(body)
        } else {
            body
        };
        if i > 0 {
            out.push('\n');
        }
        if title.starts_with("# This is the ") {
            out.push_str(&format!(
                "# {}\n\n{}",
                skipped_title(i + 1),
                commented(body)
            ));
        } else {
            out.push_str(&format!("{title}\n\n{body}"));
        }
    }
    out
}

/// Adds a chain member to the message files and returns the combined message.
pub(crate) fn add(
    dir: &Path,
    head_message: &str,
    message: &str,
    cmd: Cmd,
    flag: Flag,
    id: &ObjectId,
) -> Result<String> {
    let squash_path = dir.join(SQUASH);
    let replacing = cmd == Cmd::Fixup && flag != Flag::None;
    let had_squash = squash_path.exists();
    let seen = seen_squash(dir);
    let mut text = match std::fs::read_to_string(&squash_path) {
        Ok(text) => {
            let count = count_of(&text) + 1;
            let rest = text.split_once('\n').map_or("", |(_, rest)| rest);
            let mut text = format!("{HEADER}{count} commits.\n{rest}");
            if replacing && !seen {
                text = skip_kept(&text);
            }
            text
        }
        Err(_) => {
            if cmd == Cmd::Fixup && flag == Flag::None {
                std::fs::write(dir.join(FIXUP), completed(head_message))?;
            }
            let (title, body) = if replacing {
                (skipped_title(1), commented(&completed(head_message)))
            } else {
                (
                    "This is the 1st commit message:".into(),
                    completed(head_message),
                )
            };
            format!("{HEADER}2 commits.\n# {title}\n\n{body}")
        }
    };
    let count = count_of(&text);
    if cmd == Cmd::Squash || replacing {
        let hide = message.starts_with("amend!")
            || ((cmd == Cmd::Squash || seen)
                && (message.starts_with("squash!") || message.starts_with("fixup!")));
        let cut = if hide { subject_len(message) } else { 0 };
        text.push_str(&format!(
            "\n# This is the commit message {}:\n\n{}",
            ordinal(count),
            commented(&message[..cut])
        ));
        text.push_str(&completed(&message[cut..]));
        let keeps = flag == Flag::Replace && !seen && (dir.join(FIXUP).exists() || !had_squash);
        if keeps {
            let body = message[cut..].trim_start_matches('\n');
            std::fs::write(dir.join(FIXUP), completed(body))?;
        } else {
            let _ = std::fs::remove_file(dir.join(FIXUP));
        }
    } else {
        text.push_str(&format!(
            "\n# {}\n\n{}",
            skipped_title(count),
            commented(&completed(message))
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
pub(crate) fn drop_last(dir: &Path) {
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

pub(crate) fn clear(dir: &Path) {
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
        add(
            dir.path(),
            "first\n",
            "second\n",
            Cmd::Squash,
            Flag::None,
            &id(),
        )
        .unwrap();
        let text = add(
            dir.path(),
            "first\n",
            "third\nmore\n",
            Cmd::Fixup,
            Flag::None,
            &id(),
        )
        .unwrap();
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
    fn fixup_dash_c_skips_the_kept_messages_and_keeps_its_own() {
        let dir = tempfile::tempdir().unwrap();
        add(
            dir.path(),
            "first\n\nbody\n",
            "second\n",
            Cmd::Fixup,
            Flag::None,
            &id(),
        )
        .unwrap();
        let text = add(
            dir.path(),
            "first\n",
            "amend! first\n\nnew subject\n",
            Cmd::Fixup,
            Flag::Replace,
            &id(),
        )
        .unwrap();
        assert_eq!(
            text,
            "# This is a combination of 3 commits.\n# The 1st commit message will be skipped:\n\n# first\n#\n# body\n\n# The commit message #2 will be skipped:\n\n# second\n\n# This is the commit message #3:\n\n# amend! first\n\nnew subject\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FIXUP)).unwrap(),
            "new subject\n"
        );
        add(dir.path(), "first\n", "z\n", Cmd::Fixup, Flag::Edit, &id()).unwrap();
        assert!(!dir.path().join(FIXUP).exists());
    }

    #[test]
    fn a_leading_fixup_keeps_the_first_message_for_reuse() {
        let dir = tempfile::tempdir().unwrap();
        add(dir.path(), "first\n", "x\n", Cmd::Fixup, Flag::None, &id()).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FIXUP)).unwrap(),
            "first\n"
        );
    }
}

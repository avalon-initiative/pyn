//! What a rebase records about the commits it rewrote: `rewritten-pending` and `rewritten-list` in `rebase-merge/`,
//! and the notes copied from the old commits to the new ones when it ends (`notes.rewriteRef`).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use pyn_git::Expect;
use pyn_object::{FileMode, Kind, ObjectId, TreeEntry, encode_tree};

use super::GitHome;
use super::pick::read_text;
use super::stage::identity;

const PENDING: &str = "rewritten-pending";
const LIST: &str = "rewritten-list";
const FLAT_LIMIT: usize = 256;

fn append(path: &Path, text: &str) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

/// Notes that `old` became the commit `HEAD` is on once the commands that melt into it are done.
pub(super) fn record(dir: &Path, old: &ObjectId, head: &ObjectId, more_melds: bool) -> Result<()> {
    append(&dir.join(PENDING), &format!("{old}\n"))?;
    if !more_melds {
        flush(dir, head)?;
    }
    Ok(())
}

pub(super) fn flush(dir: &Path, head: &ObjectId) -> Result<()> {
    let Some(pending) = read_text(&dir.join(PENDING)) else {
        return Ok(());
    };
    let lines: String = pending
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| format!("{l} {head}\n"))
        .collect();
    append(&dir.join(LIST), &lines)?;
    std::fs::remove_file(dir.join(PENDING))?;
    Ok(())
}

fn pairs(git: &GitHome, dir: &Path) -> Vec<(ObjectId, ObjectId)> {
    let Some(text) = read_text(&dir.join(LIST)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let (old, new) = l.split_once(' ')?;
            let parse = |h: &str| ObjectId::from_hex(git.algorithm(), h.trim()).ok();
            Some((parse(old)?, parse(new)?))
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Concatenate,
    Overwrite,
    Ignore,
    CatSortUniq,
}

fn mode_of(git: &GitHome) -> Mode {
    match git
        .repo
        .config()
        .get("notes.rewritemode")
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("overwrite") => Mode::Overwrite,
        Some("ignore") => Mode::Ignore,
        Some("cat_sort_uniq") => Mode::CatSortUniq,
        _ => Mode::Concatenate,
    }
}

/// The notes refs to rewrite: `GIT_NOTES_REWRITE_REF` or `notes.rewriteRef` (globs allowed), under `refs/notes/`.
fn notes_refs(git: &GitHome) -> Result<Vec<String>> {
    let given: Vec<String> = match std::env::var("GIT_NOTES_REWRITE_REF") {
        Ok(v) if !v.is_empty() => v.split(':').map(str::to_string).collect(),
        _ => git
            .repo
            .config()
            .get_all("notes.rewriteref")
            .into_iter()
            .map(str::to_string)
            .collect(),
    };
    let existing = git.repo.refs().list("refs/notes/")?;
    let mut out: Vec<String> = Vec::new();
    for pattern in given {
        if !pattern.starts_with("refs/notes/") {
            continue;
        }
        for (name, _) in &existing {
            if glob(pattern.as_bytes(), name.as_bytes()) && !out.contains(name) {
                out.push(name.clone());
            }
        }
        if !pattern.contains(['*', '?', '[']) && !out.contains(&pattern) {
            out.push(pattern);
        }
    }
    Ok(out)
}

/// `*` matches any run of characters, `?` one; everything else is literal.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
    match pattern.split_first() {
        None => text.is_empty(),
        Some((b'*', rest)) => (0..=text.len()).any(|i| glob(rest, &text[i..])),
        Some((b'?', rest)) => !text.is_empty() && glob(rest, &text[1..]),
        Some((c, rest)) => text.first() == Some(c) && glob(rest, &text[1..]),
    }
}

type Notes = BTreeMap<String, ObjectId>;

fn read_notes(git: &GitHome, tree: &ObjectId, prefix: &str, out: &mut Notes) -> Result<()> {
    for entry in git.repo.tree(tree)? {
        let name = String::from_utf8_lossy(&entry.name).into_owned();
        if entry.mode.is_tree() {
            read_notes(git, &entry.id, &format!("{prefix}{name}"), out)?;
        } else {
            out.insert(format!("{prefix}{name}"), entry.id);
        }
    }
    Ok(())
}

fn write_level(
    git: &GitHome,
    notes: &[(&String, &ObjectId)],
    depth: usize,
    fanout: usize,
) -> Result<ObjectId> {
    let mut entries: Vec<TreeEntry> = Vec::new();
    if depth == fanout {
        for (hex, blob) in notes {
            entries.push(TreeEntry {
                mode: FileMode::Regular,
                name: hex.as_bytes()[depth * 2..].to_vec(),
                id: **blob,
            });
        }
    } else {
        let mut groups: BTreeMap<&str, Vec<(&String, &ObjectId)>> = BTreeMap::new();
        for n in notes {
            groups
                .entry(&n.0[depth * 2..depth * 2 + 2])
                .or_default()
                .push(*n);
        }
        for (dir, group) in groups {
            entries.push(TreeEntry {
                mode: FileMode::Tree,
                name: dir.as_bytes().to_vec(),
                id: write_level(git, &group, depth + 1, fanout)?,
            });
        }
    }
    let body = encode_tree(git.algorithm(), &entries)?;
    Ok(git.repo.objects().write(Kind::Tree, &body)?)
}

fn combine(git: &GitHome, mode: Mode, current: &ObjectId, incoming: &ObjectId) -> Result<ObjectId> {
    let (cur, new) = (git.repo.blob(current)?, git.repo.blob(incoming)?);
    let merged = match mode {
        Mode::Overwrite => return Ok(*incoming),
        Mode::Ignore => return Ok(*current),
        Mode::Concatenate => {
            let mut text = cur.strip_suffix(b"\n").unwrap_or(&cur).to_vec();
            text.extend_from_slice(b"\n\n");
            text.extend_from_slice(&new);
            text
        }
        Mode::CatSortUniq => {
            let mut lines: Vec<&[u8]> = cur
                .split(|b| *b == b'\n')
                .chain(new.split(|b| *b == b'\n'))
                .filter(|l| !l.is_empty())
                .collect();
            lines.sort();
            lines.dedup();
            let mut text = lines.join(&b'\n');
            text.push(b'\n');
            text
        }
    };
    Ok(git.repo.objects().write(Kind::Blob, &merged)?)
}

fn copy_into(git: &GitHome, name: &str, list: &[(ObjectId, ObjectId)], mode: Mode) -> Result<()> {
    let tip = git.repo.refs().resolve(name)?;
    let mut notes = Notes::new();
    if let Some(tip) = &tip {
        read_notes(git, &git.repo.commit(tip)?.tree, "", &mut notes)?;
    }
    let mut changed = false;
    for (old, new) in list {
        let (old_hex, new_hex) = (old.to_hex(), new.to_hex());
        let Some(from) = notes.get(&old_hex).copied() else {
            continue;
        };
        if old == new {
            continue;
        }
        let merged = match notes.get(&new_hex) {
            Some(current) => combine(git, mode, current, &from)?,
            None => from,
        };
        changed |= notes.insert(new_hex, merged) != Some(merged);
    }
    if !changed {
        return Ok(());
    }
    let fanout = match notes.len() {
        0..=FLAT_LIMIT => 0,
        n if n <= FLAT_LIMIT * 256 => 1,
        _ => 2,
    };
    let sorted: Vec<(&String, &ObjectId)> = notes.iter().collect();
    let tree = write_level(git, &sorted, 0, fanout)?;
    let record = pyn_object::Commit {
        tree,
        parents: tip.into_iter().collect(),
        author: identity(git, "author")?,
        committer: identity(git, "committer")?,
        extra_headers: Vec::new(),
        message: b"Notes added by 'git notes copy'\n".to_vec(),
    };
    let id = git
        .repo
        .objects()
        .write(Kind::Commit, &record.encode(git.algorithm())?)?;
    git.repo
        .refs()
        .update_checked(name, tip.map_or(Expect::Absent, Expect::Id), &id)?;
    git.log_ref(
        name,
        tip.as_ref(),
        &id,
        "notes: Notes added by 'git notes copy'",
        false,
    )
}

/// Copies the notes of every rewritten commit to its replacement, as `notes.rewrite.rebase` allows.
pub(super) fn copy_notes(git: &GitHome, dir: &Path) -> Result<()> {
    if !git.repo.bool_setting("notes.rewrite.rebase", true) {
        return Ok(());
    }
    let list = pairs(git, dir);
    if list.is_empty() {
        return Ok(());
    }
    let mode = mode_of(git);
    for name in notes_refs(git)? {
        copy_into(git, &name, &list, mode)?;
    }
    Ok(())
}

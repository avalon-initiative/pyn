//! `pyn diff` and `pyn show`: comparing two trees of files and printing the result as git does.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pyn_proto as api;

use crate::backend::Backend;
use crate::localfile;
use crate::time;
use crate::workspace::{FileState, Workspace, hash_of};

/// Bytes read to decide whether a file is binary before fetching the rest.
const PROBE: usize = 8000;
const STAT_GRAPH_WIDTH: usize = 40;

#[derive(Clone)]
pub enum Source {
    /// A path's revision on a pyn server.
    Remote {
        revision: u64,
    },
    /// A stored object, addressed by its id.
    Object {
        id: String,
    },
    Local {
        file: PathBuf,
    },
}

#[derive(Clone)]
pub struct Blob {
    pub hash: String,
    pub mode: api::FileMode,
    pub source: Source,
}

pub type Tree = BTreeMap<String, Blob>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Patch,
    Stat,
    NameOnly,
    NameStatus,
}

pub struct Format {
    pub context: usize,
    pub kind: Kind,
}

/// Revision 0 marks an entry with no per-path revision, as in a git home; it is read by its content id.
pub fn tree_of_snapshot(entries: &[api::SnapshotEntry]) -> Tree {
    entries
        .iter()
        .map(|e| {
            let blob = Blob {
                hash: e.content.clone(),
                mode: e.file_mode,
                source: if e.revision == 0 {
                    Source::Object {
                        id: e.content.clone(),
                    }
                } else {
                    Source::Remote {
                        revision: e.revision,
                    }
                },
            };
            (e.path.clone(), blob)
        })
        .collect()
}

/// The workspace's recorded revision of every file it tracks.
pub fn tree_of_state(state: &BTreeMap<String, FileState>) -> Tree {
    state
        .iter()
        .map(|(p, k)| {
            let blob = Blob {
                hash: k.hash.clone(),
                mode: k.file_mode,
                source: Source::Remote {
                    revision: k.revision,
                },
            };
            (p.clone(), blob)
        })
        .collect()
}

/// The files on disk for `paths`; those that do not exist are left out.
pub fn tree_of_disk<'a>(ws: &Workspace, paths: impl Iterator<Item = &'a String>) -> Result<Tree> {
    let mut tree = Tree::new();
    for path in paths {
        let file = ws.abs(path);
        if !localfile::exists(&file) {
            continue;
        }
        let (mode, bytes) = localfile::read(&file)?;
        tree.insert(
            path.clone(),
            Blob {
                hash: hash_of(&bytes),
                mode,
                source: Source::Local { file },
            },
        );
    }
    Ok(tree)
}

fn bytes(backend: &dyn Backend, path: &str, blob: &Blob, probe: bool) -> Result<Vec<u8>> {
    match &blob.source {
        Source::Local { file } if localfile::is_symlink(file) => Ok(localfile::read(file)?.1),
        Source::Local { file } => {
            let mut f = std::fs::File::open(file).with_context(|| format!("reading {path}"))?;
            let mut buf = Vec::new();
            if probe {
                f.take(PROBE as u64).read_to_end(&mut buf)?;
            } else {
                f.read_to_end(&mut buf)?;
            }
            Ok(buf)
        }
        Source::Remote { revision } => backend.content(path, *revision, probe.then_some(PROBE)),
        Source::Object { id } => backend.blob(id, probe.then_some(PROBE)),
    }
}

struct Contents {
    old: Vec<u8>,
    new: Vec<u8>,
    binary: bool,
}

fn contents(
    backend: &dyn Backend,
    path: &str,
    old: Option<&Blob>,
    new: Option<&Blob>,
) -> Result<Contents> {
    let old_probe = old
        .map(|b| bytes(backend, path, b, true))
        .transpose()?
        .unwrap_or_default();
    let new_probe = new
        .map(|b| bytes(backend, path, b, true))
        .transpose()?
        .unwrap_or_default();
    if pyn_diff::is_binary(&old_probe) || pyn_diff::is_binary(&new_probe) {
        return Ok(Contents {
            old: Vec::new(),
            new: Vec::new(),
            binary: true,
        });
    }
    let full = |blob: Option<&Blob>, probe: Vec<u8>| -> Result<Vec<u8>> {
        match blob {
            Some(b) if probe.len() >= PROBE => bytes(backend, path, b, false),
            _ => Ok(probe),
        }
    };
    let (old, new) = (full(old, old_probe)?, full(new, new_probe)?);
    let binary = pyn_diff::is_binary(&old) || pyn_diff::is_binary(&new);
    Ok(Contents { old, new, binary })
}

fn abbrev(blob: Option<&Blob>) -> String {
    blob.map_or("0000000".to_string(), |b| b.hash[..7].to_string())
}

fn patch(
    path: &str,
    old: Option<&Blob>,
    new: Option<&Blob>,
    c: &Contents,
    context: usize,
) -> String {
    let mut out = format!("diff --git a/{path} b/{path}\n");
    let mut tail = String::new();
    match (old, new) {
        (None, Some(b)) => {
            let _ = writeln!(out, "new file mode {}", b.mode.as_str());
        }
        (Some(a), None) => {
            let _ = writeln!(out, "deleted file mode {}", a.mode.as_str());
        }
        (Some(a), Some(b)) if a.mode != b.mode => {
            let _ = writeln!(
                out,
                "old mode {}\nnew mode {}",
                a.mode.as_str(),
                b.mode.as_str()
            );
        }
        (Some(a), Some(_)) => tail = format!(" {}", a.mode.as_str()),
        (None, None) => {}
    }
    if old.zip(new).is_none_or(|(a, b)| a.hash != b.hash) {
        let _ = writeln!(out, "index {}..{}{tail}", abbrev(old), abbrev(new));
    }
    let (from, to) = (
        old.map_or("/dev/null".to_string(), |_| format!("a/{path}")),
        new.map_or("/dev/null".to_string(), |_| format!("b/{path}")),
    );
    if c.binary {
        let _ = writeln!(out, "Binary files {from} and {to} differ");
        return out;
    }
    if let Some(hunks) = pyn_diff::unified_hunks(&c.old, &c.new, context) {
        let _ = writeln!(out, "--- {from}\n+++ {to}");
        out.push_str(&hunks);
    }
    out
}

fn stat_graph(added: usize, deleted: usize, widest: usize) -> String {
    let scale = |n: usize| {
        if widest > STAT_GRAPH_WIDTH && n > 0 {
            (n * STAT_GRAPH_WIDTH / widest).max(1)
        } else {
            n
        }
    };
    format!("{}{}", "+".repeat(scale(added)), "-".repeat(scale(deleted)))
}

fn stat_summary(files: usize, added: usize, deleted: usize) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut out = format!(" {} changed", plural(files, "file", "files"));
    if added > 0 {
        let _ = write!(out, ", {}(+)", plural(added, "insertion", "insertions"));
    }
    if deleted > 0 {
        let _ = write!(out, ", {}(-)", plural(deleted, "deletion", "deletions"));
    }
    out.push('\n');
    out
}

fn selected(path: &str, only: &[String]) -> bool {
    only.is_empty()
        || only.iter().any(|p| {
            path == p
                || path
                    .strip_prefix(p.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
}

/// Appends the difference between `old` and `new` (limited to `only` when given) to `out`; true if any file differs.
pub fn compare(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    only: &[String],
    format: &Format,
    out: &mut String,
) -> Result<bool> {
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let mut stat_rows: Vec<(String, Option<(usize, usize)>)> = Vec::new();
    let mut differs = false;
    for path in paths {
        let (a, b) = (old.get(path), new.get(path));
        if !selected(path, only)
            || a.zip(b)
                .is_some_and(|(a, b)| a.hash == b.hash && a.mode == b.mode)
        {
            continue;
        }
        differs = true;
        match format.kind {
            Kind::NameOnly => {
                let _ = writeln!(out, "{path}");
            }
            Kind::NameStatus => {
                let letter = match (a, b) {
                    (None, _) => 'A',
                    (_, None) => 'D',
                    _ => 'M',
                };
                let _ = writeln!(out, "{letter}\t{path}");
            }
            Kind::Patch => {
                let c = contents(backend, path, a, b)?;
                out.push_str(&patch(path, a, b, &c, format.context));
            }
            Kind::Stat => {
                let c = contents(backend, path, a, b)?;
                let counts = (!c.binary).then(|| pyn_diff::line_counts(&c.old, &c.new));
                stat_rows.push((path.clone(), counts));
            }
        }
    }
    if format.kind == Kind::Stat && !stat_rows.is_empty() {
        let name_w = stat_rows
            .iter()
            .map(|(p, _)| p.chars().count())
            .max()
            .unwrap_or(0);
        let total = |(a, d): (usize, usize)| a + d;
        let widest = stat_rows
            .iter()
            .filter_map(|(_, c)| c.map(total))
            .max()
            .unwrap_or(0);
        let binary = stat_rows.iter().any(|(_, c)| c.is_none());
        let num_w = widest
            .to_string()
            .len()
            .max(if binary { "Bin".len() } else { 0 });
        let (mut added, mut deleted) = (0, 0);
        for (path, counts) in &stat_rows {
            match counts {
                Some((a, d)) => {
                    added += a;
                    deleted += d;
                    let graph = stat_graph(*a, *d, widest);
                    let _ = writeln!(
                        out,
                        " {path:<name_w$} | {:>num_w$}{}{graph}",
                        a + d,
                        if graph.is_empty() { "" } else { " " },
                    );
                }
                None => {
                    let _ = writeln!(out, " {path:<name_w$} | {:>num_w$}", "Bin");
                }
            }
        }
        out.push_str(&stat_summary(stat_rows.len(), added, deleted));
    }
    Ok(differs)
}

/// The `commit`, `Author:` and `Date:` lines and the indented message, as `git show` prints them.
pub fn commit_header(c: &api::Commit) -> String {
    let mut out = format!(
        "commit {}\nAuthor: {} <{}>\nDate:   {}\n\n",
        c.id,
        c.author.name,
        c.author.email,
        time::local(c.created_at)
    );
    for line in c.message.lines() {
        let _ = writeln!(out, "    {line}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_filters_match_files_and_folders_only() {
        let only = vec!["Source".to_string(), "a.txt".to_string()];
        assert!(selected("Source/x.cpp", &only));
        assert!(selected("a.txt", &only));
        assert!(!selected("Sourcery/x.cpp", &only));
        assert!(!selected("a.txt.bak", &only));
        assert!(selected("anything", &[]));
    }

    #[test]
    fn stat_summary_pluralises_and_omits_zero_parts() {
        assert_eq!(stat_summary(1, 1, 0), " 1 file changed, 1 insertion(+)\n");
        assert_eq!(
            stat_summary(2, 3, 1),
            " 2 files changed, 3 insertions(+), 1 deletion(-)\n"
        );
    }
}

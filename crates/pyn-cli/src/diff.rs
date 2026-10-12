//! `pyn diff` and `pyn show`: comparing two trees of files and printing the result as git does.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pyn_proto as api;

use crate::client::Api;
use crate::snapshot;
use crate::time;
use crate::workspace::{FileState, Workspace, hash_of};

/// Bytes read to decide whether a file is binary before fetching the rest.
const PROBE: usize = 8000;
const STAT_GRAPH_WIDTH: usize = 40;

#[derive(Clone)]
pub enum Source {
    Remote { revision: u64 },
    Local { file: PathBuf },
}

#[derive(Clone)]
pub struct Blob {
    pub hash: String,
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

pub fn tree_of_snapshot(entries: &[api::SnapshotEntry]) -> Tree {
    entries
        .iter()
        .map(|e| {
            let blob = Blob {
                hash: e.content.clone(),
                source: Source::Remote {
                    revision: e.revision,
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
        if !file.is_file() {
            continue;
        }
        let hash = hash_of(&std::fs::read(&file).with_context(|| format!("reading {path}"))?);
        tree.insert(
            path.clone(),
            Blob {
                hash,
                source: Source::Local { file },
            },
        );
    }
    Ok(tree)
}

fn bytes(api: &Api, path: &str, blob: &Blob, probe: bool) -> Result<Vec<u8>> {
    match &blob.source {
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
        Source::Remote { revision } => {
            snapshot::content(api, path, *revision, probe.then_some(PROBE))
        }
    }
}

struct Contents {
    old: Vec<u8>,
    new: Vec<u8>,
    binary: bool,
}

fn contents(api: &Api, path: &str, old: Option<&Blob>, new: Option<&Blob>) -> Result<Contents> {
    let old_probe = old
        .map(|b| bytes(api, path, b, true))
        .transpose()?
        .unwrap_or_default();
    let new_probe = new
        .map(|b| bytes(api, path, b, true))
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
            Some(b) if probe.len() >= PROBE => bytes(api, path, b, false),
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
    match (old, new) {
        (None, _) => out.push_str("new file mode 100644\n"),
        (_, None) => out.push_str("deleted file mode 100644\n"),
        _ => {}
    }
    let tail = if old.is_some() && new.is_some() {
        " 100644"
    } else {
        ""
    };
    let _ = writeln!(out, "index {}..{}{tail}", abbrev(old), abbrev(new));
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
    api: &Api,
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
        if !selected(path, only) || a.zip(b).is_some_and(|(a, b)| a.hash == b.hash) {
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
                let c = contents(api, path, a, b)?;
                out.push_str(&patch(path, a, b, &c, format.context));
            }
            Kind::Stat => {
                let c = contents(api, path, a, b)?;
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
        let num_w = widest.to_string().len();
        let (mut added, mut deleted) = (0, 0);
        for (path, counts) in &stat_rows {
            match counts {
                Some((a, d)) => {
                    added += a;
                    deleted += d;
                    let _ = writeln!(
                        out,
                        " {path:<name_w$} | {:>num_w$} {}",
                        a + d,
                        stat_graph(*a, *d, widest)
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
        "commit c{}\nAuthor: {}\nDate:   {}\n\n",
        c.id,
        c.author,
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

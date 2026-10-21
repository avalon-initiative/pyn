//! `pyn diff` and `pyn show`: comparing two trees of files and printing the result as git does.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pyn_proto as api;

use crate::backend::Backend;
use crate::diffstat::{self, Counts};
use crate::localfile;
use crate::time;
use crate::workspace::{FileState, Workspace, hash_of};

/// Bytes read to decide whether a file is binary before fetching the rest.
const PROBE: usize = 8000;

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
    ShortStat,
    NameOnly,
    NameStatus,
    /// `--check`: only the whitespace problems in added lines.
    Check,
}

pub struct Format {
    pub context: usize,
    pub kind: Kind,
    pub ignore_ws: bool,
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

/// The staged content of every staged path, read from where the index keeps it.
pub fn tree_of_staged(ws: &Workspace, index: &crate::staging::Index) -> Tree {
    index
        .entries
        .iter()
        .filter(|(_, e)| !e.deleted)
        .map(|(p, e)| {
            let blob = Blob {
                hash: e.hash.clone(),
                mode: e.file_mode,
                source: Source::Local {
                    file: crate::staging::content_path(ws, &e.hash),
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
    format: &Format,
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
    if let Some(hunks) = pyn_diff::unified_hunks(&c.old, &c.new, format.context, format.ignore_ws) {
        let _ = writeln!(out, "--- {from}\n+++ {to}");
        out.push_str(&hunks);
    }
    out
}

fn blob_size(backend: &dyn Backend, path: &str, blob: Option<&Blob>) -> Result<usize> {
    blob.map_or(Ok(0), |b| Ok(bytes(backend, path, b, false)?.len()))
}

fn totals(rows: &[(String, Counts)]) -> (usize, usize) {
    rows.iter().fold((0, 0), |(a, d), (_, c)| match c {
        Counts::Lines(x, y) => (a + x, d + y),
        Counts::Binary(..) => (a, d),
    })
}

struct Pair<'a> {
    old_path: &'a str,
    new_path: &'a str,
    old: Option<&'a Blob>,
    new: Option<&'a Blob>,
    /// Similarity percent when git would show this pair as a rename.
    renamed: Option<u64>,
}

fn is_link(blob: &Blob) -> bool {
    blob.mode == api::FileMode::Symlink
}

const MAX_SCORE: u64 = 60_000;
const MIN_SCORE: u64 = 30_000;

/// Git's chunk hashes of a file: lines (up to 64 bytes a piece) with how many bytes carry each hash.
fn chunk_counts(data: &[u8]) -> BTreeMap<u64, u64> {
    use std::hash::{DefaultHasher, Hasher};
    let text = !pyn_diff::is_binary(data);
    let mut counts = BTreeMap::new();
    let (mut hasher, mut n) = (DefaultHasher::new(), 0u64);
    let mut flush = |hasher: &mut DefaultHasher, n: &mut u64| {
        if *n > 0 {
            *counts.entry(hasher.finish()).or_insert(0) += *n;
        }
        *hasher = DefaultHasher::new();
        *n = 0;
    };
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        i += 1;
        if text && c == b'\r' && data.get(i) == Some(&b'\n') {
            continue;
        }
        hasher.write_u8(c);
        n += 1;
        if (text && c == b'\n') || n >= 64 {
            flush(&mut hasher, &mut n);
        }
    }
    flush(&mut hasher, &mut n);
    counts
}

/// Git's similarity of two files out of `MAX_SCORE`, or 0 when they differ too much in size to count.
fn similarity(src: &[u8], dst: &[u8], a: &BTreeMap<u64, u64>, b: &BTreeMap<u64, u64>) -> u64 {
    let (small, large) = (
        src.len().min(dst.len()) as u64,
        src.len().max(dst.len()) as u64,
    );
    if large == 0 || large * (MAX_SCORE - MIN_SCORE) < (large - small) * MAX_SCORE {
        return 0;
    }
    let copied: u64 = b
        .iter()
        .map(|(h, n)| a.get(h).map_or(0, |m| *m.min(n)))
        .sum();
    copied * MAX_SCORE / large
}

/// Renames git detects: identical content first, then at least half-similar content, as `dst -> (src, percent)`.
fn detect_renames<'a>(
    backend: &dyn Backend,
    old: &'a Tree,
    new: &'a Tree,
) -> Result<BTreeMap<&'a str, (&'a str, u64)>> {
    let mut taken: BTreeSet<&str> = BTreeSet::new();
    let mut pairs = BTreeMap::new();
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let adds: Vec<(&String, &Blob)> = new.iter().filter(|(p, _)| !old.contains_key(*p)).collect();
    let dels: Vec<(&String, &Blob)> = old.iter().filter(|(p, _)| !new.contains_key(*p)).collect();
    for (dst, to) in &adds {
        let mut same: Vec<&String> = dels
            .iter()
            .filter(|(p, b)| {
                !taken.contains(p.as_str()) && b.hash == to.hash && is_link(b) == is_link(to)
            })
            .map(|(p, _)| *p)
            .collect();
        same.sort_by_key(|p| base(p) != base(dst));
        if let Some(src) = same.first() {
            taken.insert(src.as_str());
            pairs.insert(dst.as_str(), (src.as_str(), 100));
        }
    }
    let rest_adds: Vec<_> = adds
        .iter()
        .filter(|(p, _)| !pairs.contains_key(p.as_str()))
        .collect();
    let rest_dels: Vec<_> = dels
        .iter()
        .filter(|(p, _)| !taken.contains(p.as_str()))
        .collect();
    if rest_adds.is_empty() || rest_dels.is_empty() {
        return Ok(pairs);
    }
    let mut sources = Vec::new();
    for (p, b) in &rest_dels {
        let data = bytes(backend, p, b, false)?;
        let counts = chunk_counts(&data);
        sources.push((p.as_str(), is_link(b), data, counts));
    }
    let mut scored: Vec<(u64, bool, &str, &str)> = Vec::new();
    for (dst, to) in &rest_adds {
        let data = bytes(backend, dst, to, false)?;
        let counts = chunk_counts(&data);
        for (src, link, sdata, scounts) in &sources {
            if *link != is_link(to) {
                continue;
            }
            let score = similarity(sdata, &data, scounts, &counts);
            if score >= MIN_SCORE {
                scored.push((score, base(src) != base(dst), src, dst.as_str()));
            }
        }
    }
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.3.cmp(b.3))
            .then(a.2.cmp(b.2))
    });
    for (score, _, src, dst) in scored {
        if taken.contains(src) || pairs.contains_key(dst) {
            continue;
        }
        taken.insert(src);
        pairs.insert(dst, (src, score * 100 / MAX_SCORE));
    }
    Ok(pairs)
}

/// The diffstat and the create, delete, mode-change and rename lines git prints after a merge or fast-forward.
pub fn merge_stat(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    layout: (usize, Option<usize>),
) -> Result<String> {
    let moved = detect_renames(backend, old, new)?;
    let sources: BTreeSet<&str> = moved.values().map(|(src, _)| *src).collect();
    let mut pairs: Vec<Pair> = Vec::new();
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for path in paths {
        let (a, b) = (old.get(path), new.get(path));
        if a.zip(b)
            .is_some_and(|(a, b)| a.hash == b.hash && a.mode == b.mode)
            || sources.contains(path.as_str())
        {
            continue;
        }
        match moved.get(path.as_str()) {
            Some((src, percent)) => pairs.push(Pair {
                old_path: src,
                new_path: path,
                old: old.get(*src),
                new: b,
                renamed: Some(*percent),
            }),
            None => pairs.push(Pair {
                old_path: path,
                new_path: path,
                old: a,
                new: b,
                renamed: None,
            }),
        }
    }
    let mut rows = Vec::new();
    let mut summary = String::new();
    for p in &pairs {
        let same = p.old.zip(p.new).is_some_and(|(a, b)| a.hash == b.hash);
        let counts = {
            let c = contents(backend, p.new_path, p.old, p.new)?;
            if c.binary && p.renamed.is_some() && same {
                Counts::Binary(0, 0)
            } else if c.binary {
                Counts::Binary(
                    blob_size(backend, p.old_path, p.old)?,
                    blob_size(backend, p.new_path, p.new)?,
                )
            } else {
                let (x, y) = pyn_diff::line_counts(&c.old, &c.new, false);
                Counts::Lines(x, y)
            }
        };
        let name = if p.renamed.is_some() {
            diffstat::rename_name(p.old_path, p.new_path)
        } else {
            p.new_path.to_string()
        };
        match (p.old, p.new) {
            (None, Some(b)) => {
                let _ = writeln!(summary, " create mode {} {}", b.mode.as_str(), p.new_path);
            }
            (Some(a), None) => {
                let _ = writeln!(summary, " delete mode {} {}", a.mode.as_str(), p.old_path);
            }
            (Some(a), Some(b)) => {
                if let Some(percent) = p.renamed {
                    let _ = writeln!(summary, " rename {name} ({percent}%)");
                }
                if a.mode != b.mode {
                    let tail = if p.renamed.is_some() {
                        String::new()
                    } else {
                        format!(" {}", p.new_path)
                    };
                    let _ = writeln!(
                        summary,
                        " mode change {} => {}{tail}",
                        a.mode.as_str(),
                        b.mode.as_str()
                    );
                }
            }
            (None, None) => {}
        }
        rows.push(diffstat::Row { name, counts });
    }
    let mut out = diffstat::render(&rows, layout.0, layout.1);
    out.push_str(&summary);
    Ok(out)
}

/// A text file whose content differs from the other side only in whitespace.
fn whitespace_only(old: Option<&Blob>, new: Option<&Blob>, c: &Contents) -> bool {
    old.zip(new)
        .is_some_and(|(a, b)| a.mode == b.mode && !c.binary)
        && pyn_diff::line_counts(&c.old, &c.new, true) == (0, 0)
}

/// Whether `show` separates the diff from the message: git does for `--check` on any change, even a clean one.
pub fn has_body(format: &Format, differs: bool, body: &str) -> bool {
    !body.is_empty() || (format.kind == Kind::Check && differs)
}

/// Exits 2 after `--check` printed problems, as git does.
pub fn exit_if_check_failed(format: &Format, out: &str) {
    if format.kind == Kind::Check && !out.is_empty() {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::process::exit(2);
    }
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
    let mut stat_rows: Vec<(String, Counts)> = Vec::new();
    let mut differs = false;
    for path in paths {
        let (a, b) = (old.get(path), new.get(path));
        if !selected(path, only)
            || a.zip(b)
                .is_some_and(|(a, b)| a.hash == b.hash && a.mode == b.mode)
        {
            continue;
        }
        match format.kind {
            Kind::NameOnly => {
                differs = true;
                let _ = writeln!(out, "{path}");
            }
            Kind::NameStatus => {
                differs = true;
                let letter = match (a, b) {
                    (None, _) => 'A',
                    (_, None) => 'D',
                    _ => 'M',
                };
                let _ = writeln!(out, "{letter}\t{path}");
            }
            Kind::Patch | Kind::Check | Kind::Stat | Kind::ShortStat => {
                let c = contents(backend, path, a, b)?;
                if format.ignore_ws && format.kind != Kind::Check && whitespace_only(a, b, &c) {
                    continue;
                }
                differs = true;
                if format.kind == Kind::Patch {
                    out.push_str(&patch(path, a, b, &c, format));
                    continue;
                }
                if format.kind == Kind::Check {
                    if !c.binary {
                        out.push_str(&pyn_diff::whitespace_report(path, &c.old, &c.new));
                    }
                    continue;
                }
                let counts = if c.binary {
                    Counts::Binary(blob_size(backend, path, a)?, blob_size(backend, path, b)?)
                } else {
                    let (x, y) = pyn_diff::line_counts(&c.old, &c.new, format.ignore_ws);
                    Counts::Lines(x, y)
                };
                stat_rows.push((path.clone(), counts));
            }
        }
    }
    if format.kind == Kind::ShortStat && !stat_rows.is_empty() {
        let (added, deleted) = totals(&stat_rows);
        out.push_str(&diffstat::summary(stat_rows.len(), added, deleted));
    }
    if format.kind == Kind::Stat {
        let rows: Vec<diffstat::Row> = stat_rows
            .into_iter()
            .map(|(name, counts)| diffstat::Row { name, counts })
            .collect();
        out.push_str(&diffstat::render(&rows, diffstat::DEFAULT_WIDTH, None));
    }
    Ok(differs)
}

/// The diff sections `log` prints after each commit.
pub struct DiffParts {
    pub names: Option<Kind>,
    pub stat: bool,
    pub shortstat: bool,
    pub patch: bool,
    pub context: usize,
}

impl DiffParts {
    pub fn any(&self) -> bool {
        self.names.is_some() || self.stat || self.shortstat || self.patch
    }
}

/// The sections of `parts` for `old` against `new`, and whether `---` follows the message; `None` when nothing differs.
pub fn sections(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    only: &[String],
    parts: &DiffParts,
) -> Result<Option<(String, bool)>> {
    let mut differs = false;
    let mut section = |kind: Kind| -> Result<String> {
        let mut out = String::new();
        let format = Format {
            context: parts.context,
            kind,
            ignore_ws: false,
        };
        differs |= compare(backend, old, new, only, &format, &mut out)?;
        Ok(out)
    };
    let mut body = String::new();
    if let Some(kind) = parts.names {
        body.push_str(&section(kind)?);
    }
    if parts.stat {
        body.push_str(&section(Kind::Stat)?);
    }
    if parts.shortstat {
        body.push_str(&section(Kind::ShortStat)?);
    }
    if parts.patch {
        let patch = section(Kind::Patch)?;
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&patch);
    }
    if !differs {
        return Ok(None);
    }
    Ok(Some((body, parts.stat && parts.patch)))
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
}

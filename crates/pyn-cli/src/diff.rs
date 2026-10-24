//! `pyn diff` and `pyn show`: comparing two trees of files and printing the result as git does.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pyn_object::FileMode;
use pyn_proto as api;

use crate::backend::Backend;
use crate::diffstat::{self, Counts};
use crate::local_store::file_mode;
use crate::localfile;
use crate::rename::{self, Moved};
use crate::time;
use crate::workspace::{FileState, Workspace, hash_of};

/// Bytes read to decide whether a file is binary before fetching the rest.
const PROBE: usize = 8000;

#[derive(Clone)]
pub enum Source {
    /// A gitlink's synthetic `Subproject commit` line, never a blob in the superproject.
    Gitlink {
        dirty: bool,
    },
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
    pub mode: FileMode,
    pub source: Source,
}

impl Blob {
    pub fn gitlink(hash: String, dirty: bool) -> Self {
        Self {
            hash,
            mode: FileMode::Gitlink,
            source: Source::Gitlink { dirty },
        }
    }

    pub fn is_gitlink(&self) -> bool {
        self.mode == FileMode::Gitlink
    }

    pub(crate) fn is_regular(&self) -> bool {
        matches!(self.mode, FileMode::Regular | FileMode::Executable)
    }

    pub(crate) fn mode(&self) -> &str {
        self.mode.as_str()
    }

    fn same_content(&self, other: &Self) -> bool {
        self.hash == other.hash && self.dirty() == other.dirty()
    }

    fn dirty(&self) -> bool {
        matches!(self.source, Source::Gitlink { dirty: true })
    }
}

pub type Tree = BTreeMap<String, Blob>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Patch,
    Stat,
    ShortStat,
    NumStat,
    NameOnly,
    NameStatus,
    /// `--check`: only the whitespace problems in added lines.
    Check,
    /// `--summary` alone.
    Summary,
}

pub struct Format {
    pub context: usize,
    pub kind: Kind,
    pub ignore_ws: bool,
    pub renames: rename::Request,
    /// `--summary` next to another output kind.
    pub summary: bool,
    /// Only the pairs whose new path is listed, applied after rename detection (the pickaxe's choice).
    pub kept: Vec<String>,
}

/// Revision 0 marks an entry with no per-path revision, as in a git home; it is read by its content id.
pub fn tree_of_snapshot(entries: &[api::SnapshotEntry]) -> Tree {
    entries
        .iter()
        .map(|e| {
            let blob = Blob {
                hash: e.content.clone(),
                mode: file_mode(e.file_mode),
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
                mode: file_mode(k.file_mode),
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
                mode: file_mode(e.file_mode),
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
                mode: file_mode(mode),
                source: Source::Local { file },
            },
        );
    }
    Ok(tree)
}

fn bytes(backend: &dyn Backend, path: &str, blob: &Blob, probe: bool) -> Result<Vec<u8>> {
    match &blob.source {
        Source::Gitlink { dirty } => Ok(format!(
            "Subproject commit {}{}\n",
            blob.hash,
            if *dirty { "-dirty" } else { "" }
        )
        .into_bytes()),
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

fn patch(p: &Pair, c: &Contents, format: &Format) -> String {
    let (old, new) = (p.old.as_ref(), p.new.as_ref());
    let mut out = format!("diff --git a/{} b/{}\n", p.old_path, p.new_path);
    let mut tail = String::new();
    match (old, new) {
        (None, Some(b)) => {
            let _ = writeln!(out, "new file mode {}", b.mode());
        }
        (Some(a), None) => {
            let _ = writeln!(out, "deleted file mode {}", a.mode());
        }
        (Some(a), Some(b)) if a.mode() != b.mode() => {
            let _ = writeln!(out, "old mode {}\nnew mode {}", a.mode(), b.mode());
        }
        (Some(a), Some(_)) => tail = format!(" {}", a.mode()),
        (None, None) => {}
    }
    if let Some(m) = &p.moved {
        let word = if m.copy { "copy" } else { "rename" };
        let _ = writeln!(
            out,
            "similarity index {}%\n{word} from {}\n{word} to {}",
            m.percent(),
            p.old_path,
            p.new_path
        );
    }
    let changed = old.zip(new).is_none_or(|(a, b)| a.hash != b.hash);
    if changed {
        let _ = writeln!(out, "index {}..{}{tail}", abbrev(old), abbrev(new));
    }
    let (from, to) = (
        old.map_or("/dev/null".to_string(), |_| format!("a/{}", p.old_path)),
        new.map_or("/dev/null".to_string(), |_| format!("b/{}", p.new_path)),
    );
    if c.binary {
        if changed {
            let _ = writeln!(out, "Binary files {from} and {to} differ");
        }
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

#[derive(Clone)]
struct Pair {
    old_path: String,
    new_path: String,
    old: Option<Blob>,
    new: Option<Blob>,
    moved: Option<Moved>,
}

/// The content of a blob.
pub fn blob_bytes(backend: &dyn Backend, path: &str, blob: &Blob) -> Result<Vec<u8>> {
    bytes(backend, path, blob, false)
}

/// The path `target` was renamed from in the change `old` to `new`, as `git log --follow` finds it.
pub fn renamed_from(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    target: &str,
) -> Result<Option<String>> {
    let new: Tree = new
        .iter()
        .filter(|(p, _)| p.as_str() == target || old.contains_key(*p))
        .map(|(p, b)| (p.clone(), b.clone()))
        .collect();
    let found = rename::detect(backend, old, &new, &rename::Options::renames())?;
    Ok(found.moved.get(target).map(|m| m.src.clone()))
}

/// Renames git detects between `old` and `new`, as `dst -> (src, percent)`.
pub fn detect_renames<'a>(
    backend: &dyn Backend,
    old: &'a Tree,
    new: &'a Tree,
) -> Result<BTreeMap<&'a str, (&'a str, u64)>> {
    let found = rename::detect(backend, old, new, &rename::Options::renames())?;
    Ok(found
        .moved
        .iter()
        .filter_map(|(dst, m)| {
            let (dst, _) = new.get_key_value(dst)?;
            let (src, _) = old.get_key_value(&m.src)?;
            Some((dst.as_str(), (src.as_str(), m.percent())))
        })
        .collect())
}

/// The changed pairs of `old` against `new` limited to `only`, with the renames and copies `opts` finds folded in.
fn pairs(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    only: &[String],
    opts: Option<&rename::Options>,
) -> Result<Vec<Pair>> {
    let keep = |t: &Tree| -> Tree {
        t.iter()
            .filter(|(p, _)| selected(p, only))
            .map(|(p, b)| (p.clone(), b.clone()))
            .collect()
    };
    let limited;
    let (old, new) = if only.is_empty() {
        (old, new)
    } else {
        limited = (keep(old), keep(new));
        (&limited.0, &limited.1)
    };
    let found = match opts {
        Some(o) => Some(rename::detect(backend, old, new, o)?),
        None => None,
    };
    let mut out = Vec::new();
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for path in paths {
        let (a, b) = (old.get(path), new.get(path));
        if a.zip(b)
            .is_some_and(|(a, b)| a.same_content(b) && a.mode() == b.mode())
            || found.as_ref().is_some_and(|f| f.consumed.contains(path))
        {
            continue;
        }
        let moved = found.as_ref().and_then(|f| f.moved.get(path));
        out.push(match moved {
            Some(m) => Pair {
                old_path: m.src.clone(),
                new_path: path.clone(),
                old: old.get(&m.src).cloned(),
                new: b.cloned(),
                moved: Some(m.clone()),
            },
            None => Pair {
                old_path: path.clone(),
                new_path: path.clone(),
                old: a.cloned(),
                new: b.cloned(),
                moved: None,
            },
        });
    }
    Ok(out)
}

/// The create, delete, rename, copy and mode-change lines of `--summary` for a pair.
fn summary_line(p: &Pair) -> String {
    let mut out = String::new();
    match (&p.old, &p.new) {
        (None, Some(b)) => {
            let _ = writeln!(out, " create mode {} {}", b.mode(), p.new_path);
        }
        (Some(a), None) => {
            let _ = writeln!(out, " delete mode {} {}", a.mode(), p.old_path);
        }
        (Some(a), Some(b)) if a.is_gitlink() != b.is_gitlink() => {
            let _ = writeln!(out, " delete mode {} {}", a.mode(), p.old_path);
            let _ = writeln!(out, " create mode {} {}", b.mode(), p.new_path);
        }
        (Some(a), Some(b)) => {
            if let Some(m) = &p.moved {
                let word = if m.copy { "copy" } else { "rename" };
                let name = diffstat::rename_name(&p.old_path, &p.new_path);
                let _ = writeln!(out, " {word} {name} ({}%)", m.percent());
            }
            if a.mode() != b.mode() {
                let tail = if p.moved.is_some() {
                    String::new()
                } else {
                    format!(" {}", p.new_path)
                };
                let _ = writeln!(out, " mode change {} => {}{tail}", a.mode(), b.mode());
            }
        }
        (None, None) => {}
    }
    out
}

/// The diffstat and the create, delete, mode-change and rename lines git prints after a merge or fast-forward.
pub fn merge_stat(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    layout: (usize, Option<usize>),
) -> Result<String> {
    let opts = rename::Options {
        limit: backend.rename_defaults().limit,
        ..rename::Options::renames()
    };
    let pairs = pairs(backend, old, new, &[], Some(&opts))?;
    let mut rows = Vec::new();
    let mut summary = String::new();
    for p in &pairs {
        let same = p
            .old
            .as_ref()
            .zip(p.new.as_ref())
            .is_some_and(|(a, b)| a.hash == b.hash);
        let counts = {
            let c = contents(backend, &p.new_path, p.old.as_ref(), p.new.as_ref())?;
            if c.binary && p.moved.is_some() && same {
                Counts::Binary(0, 0)
            } else if c.binary {
                Counts::Binary(
                    blob_size(backend, &p.old_path, p.old.as_ref())?,
                    blob_size(backend, &p.new_path, p.new.as_ref())?,
                )
            } else {
                let (x, y) = pyn_diff::line_counts(&c.old, &c.new, false);
                Counts::Lines(x, y)
            }
        };
        let name = if p.moved.is_some() {
            diffstat::rename_name(&p.old_path, &p.new_path)
        } else {
            p.new_path.clone()
        };
        summary.push_str(&summary_line(p));
        rows.push(diffstat::Row { name, counts });
    }
    let mut out = diffstat::render(&rows, layout.0, layout.1);
    out.push_str(&summary);
    Ok(out)
}

/// A text file whose content differs from the other side only in whitespace.
fn whitespace_only(old: Option<&Blob>, new: Option<&Blob>, c: &Contents) -> bool {
    old.zip(new)
        .is_some_and(|(a, b)| a.mode() == b.mode() && !c.binary)
        && pyn_diff::line_counts(&c.old, &c.new, true) == (0, 0)
}

/// Whether `show` separates the diff from the message: git does for `--check` and `--summary` on any change, even a clean one.
pub fn has_body(format: &Format, differs: bool, body: &str) -> bool {
    !body.is_empty() || (matches!(format.kind, Kind::Check | Kind::Summary) && differs)
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
    let opts = format
        .renames
        .resolve(backend.rename_defaults())
        .unwrap_or_else(|message| crate::usage_error(&message));
    let mut list = pairs(backend, old, new, only, opts.as_ref())?;
    if !format.kept.is_empty() {
        list.retain(|p| format.kept.contains(&p.new_path));
    }
    let summary: String = if format.summary || format.kind == Kind::Summary {
        list.iter().map(summary_line).collect()
    } else {
        String::new()
    };
    if format.kind == Kind::Summary {
        out.push_str(&summary);
        return Ok(!list.is_empty());
    }
    let mut body = String::new();
    let differs = render(backend, &list, format, &mut body)?;
    if format.kind == Kind::Patch {
        out.push_str(&summary);
        if !summary.is_empty() && !body.is_empty() {
            out.push('\n');
        }
        out.push_str(&body);
    } else {
        out.push_str(&body);
        out.push_str(&summary);
    }
    Ok(differs)
}

fn render(backend: &dyn Backend, list: &[Pair], format: &Format, out: &mut String) -> Result<bool> {
    let mut stat_rows: Vec<(String, Counts)> = Vec::new();
    let mut differs = false;
    for p in list {
        let (a, b) = (p.old.as_ref(), p.new.as_ref());
        let path = &p.new_path;
        match format.kind {
            Kind::NameOnly => {
                differs = true;
                let _ = writeln!(out, "{path}");
            }
            Kind::NameStatus => {
                differs = true;
                if let Some(m) = &p.moved {
                    let letter = if m.copy { 'C' } else { 'R' };
                    let _ = writeln!(out, "{letter}{:03}\t{}\t{path}", m.percent(), p.old_path);
                    continue;
                }
                let letter = match (a, b) {
                    (None, _) => 'A',
                    (_, None) => 'D',
                    (Some(a), Some(b)) if a.is_gitlink() != b.is_gitlink() => 'T',
                    _ => 'M',
                };
                let _ = writeln!(out, "{letter}\t{path}");
            }
            Kind::Summary => {}
            Kind::Patch | Kind::Check | Kind::Stat | Kind::ShortStat | Kind::NumStat => {
                if format.kind == Kind::Patch
                    && a.zip(b)
                        .is_some_and(|(a, b)| a.is_gitlink() != b.is_gitlink())
                {
                    differs = true;
                    for (old, new) in [(a, None), (None, b)] {
                        let split = Pair {
                            old: old.cloned(),
                            new: new.cloned(),
                            moved: None,
                            ..(*p).clone()
                        };
                        let c = contents(backend, path, old, new)?;
                        out.push_str(&patch(&split, &c, format));
                    }
                    continue;
                }
                let c = contents(backend, path, a, b)?;
                if format.ignore_ws
                    && format.kind != Kind::Check
                    && p.moved.is_none()
                    && whitespace_only(a, b, &c)
                {
                    continue;
                }
                differs = true;
                if format.kind == Kind::Patch {
                    out.push_str(&patch(p, &c, format));
                    continue;
                }
                if format.kind == Kind::Check {
                    if !c.binary {
                        out.push_str(&pyn_diff::whitespace_report(path, &c.old, &c.new));
                    }
                    continue;
                }
                let same = a.zip(b).is_some_and(|(a, b)| a.hash == b.hash);
                let counts = if c.binary && p.moved.is_some() && same {
                    Counts::Binary(0, 0)
                } else if c.binary {
                    Counts::Binary(
                        blob_size(backend, &p.old_path, a)?,
                        blob_size(backend, path, b)?,
                    )
                } else if a
                    .zip(b)
                    .is_some_and(|(a, b)| a.is_gitlink() && b.is_gitlink() && a.hash == b.hash)
                {
                    Counts::Lines(0, 0)
                } else {
                    let (x, y) = pyn_diff::line_counts(&c.old, &c.new, format.ignore_ws);
                    Counts::Lines(x, y)
                };
                let name = if p.moved.is_some() {
                    diffstat::rename_name(&p.old_path, path)
                } else {
                    path.clone()
                };
                stat_rows.push((name, counts));
            }
        }
    }
    if format.kind == Kind::NumStat {
        for (path, counts) in &stat_rows {
            let path = backend.quote_diff_path(path.as_bytes());
            match counts {
                Counts::Lines(a, d) => {
                    let _ = writeln!(out, "{a}\t{d}\t{path}");
                }
                Counts::Binary(..) => {
                    let _ = writeln!(out, "-\t-\t{path}");
                }
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
    pub renames: rename::Request,
    pub summary: bool,
    pub names: Option<Kind>,
    pub stat: bool,
    pub shortstat: bool,
    pub patch: bool,
    pub context: usize,
}

impl DiffParts {
    pub fn any(&self) -> bool {
        self.names.is_some() || self.stat || self.shortstat || self.patch || self.summary
    }
}

/// The sections of `parts` for `old` against `new`, and whether `---` follows the message; `None` when nothing differs.
pub fn sections(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    (only, kept): (&[String], &[String]),
    parts: &DiffParts,
) -> Result<Option<(String, bool)>> {
    let mut differs = false;
    let mut section = |kind: Kind| -> Result<String> {
        let mut out = String::new();
        let format = Format {
            context: parts.context,
            kind,
            ignore_ws: false,
            renames: parts.renames,
            summary: false,
            kept: kept.to_vec(),
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
    if parts.summary {
        body.push_str(&section(Kind::Summary)?);
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

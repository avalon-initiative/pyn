//! `pyn diff` and `pyn show`: comparing two trees of files and printing the result as git does.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pyn_object::FileMode;
use pyn_proto as api;

use crate::backend::Backend;
use crate::diff_color::{self, Style};
use crate::diff_opts::Opts;
use crate::diff_raw;
use crate::diffstat::{self, Counts};
use crate::dirstat;
use crate::local_store::file_mode;
use crate::localfile;
use crate::rename::{self, Moved};
use crate::time;
use crate::word_diff;
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
    /// A work tree file whose id git does not record: the raw format shows it as zeros.
    Worktree {
        file: PathBuf,
        /// The repository form of the file when line endings or `ident` convert it.
        cleaned: Option<std::sync::Arc<Vec<u8>>>,
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
    /// `--raw`: a line of modes, ids and a status letter per file.
    Raw,
    /// `--dirstat`: the share of the change in each folder.
    Dirstat,
    /// `-s`: no output, only the exit status.
    Nothing,
}

#[derive(Clone)]
pub struct Format {
    pub context: usize,
    pub kind: Kind,
    pub opts: Opts,
    pub renames: rename::Request,
    /// `--summary` next to another output kind.
    pub summary: bool,
    /// The patch after the output of a statistics or raw kind, as `-p --stat` prints.
    pub with_patch: bool,
    /// Only the pairs whose new path is listed, applied after rename detection (the pickaxe's choice).
    pub kept: Vec<String>,
}

impl Format {
    /// This format, or one that prints nothing when `silent`.
    pub fn silenced(self, silent: bool) -> Self {
        if !silent {
            return self;
        }
        if self.kind != Kind::Patch || self.with_patch {
            crate::usage_error("`-s` together with another output format is not built");
        }
        Self {
            kind: Kind::Nothing,
            summary: false,
            with_patch: false,
            ..self
        }
    }
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
        Source::Worktree {
            cleaned: Some(data),
            ..
        } => Ok(match probe {
            true => data[..data.len().min(PROBE)].to_vec(),
            false => data.to_vec(),
        }),
        Source::Local { file } | Source::Worktree { file, .. } if localfile::is_symlink(file) => {
            Ok(localfile::read(file)?.1)
        }
        Source::Local { file } | Source::Worktree { file, .. } => {
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
    text: bool,
) -> Result<Contents> {
    let old_probe = old
        .map(|b| bytes(backend, path, b, true))
        .transpose()?
        .unwrap_or_default();
    let new_probe = new
        .map(|b| bytes(backend, path, b, true))
        .transpose()?
        .unwrap_or_default();
    let forced = backend.diff_binary(path);
    let binary_of = |data: &[u8]| forced.unwrap_or_else(|| pyn_diff::is_binary(data));
    if !text && (binary_of(&old_probe) || binary_of(&new_probe)) {
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
    let binary = !text && (binary_of(&old) || binary_of(&new));
    Ok(Contents { old, new, binary })
}

/// A blob's id as `shorten` writes it; the null id of a missing side is as long as the other side's.
fn id_of(blob: Option<&Blob>, other: Option<&Blob>, shorten: impl Fn(&str) -> String) -> String {
    match blob {
        Some(b) => shorten(&b.hash),
        None => shorten(&"0".repeat(other.map_or(40, |o| o.hash.len()))),
    }
}

fn patch(
    backend: &dyn Backend,
    p: &Pair,
    c: &Contents,
    format: &Format,
    (style, words): (Option<&Style>, Option<&word_diff::Setup>),
) -> String {
    let (old, new) = (p.old.as_ref(), p.new.as_ref());
    let o = &format.opts;
    // Outside a repository an absolute path loses its leading slash in the headers.
    let bare = |path: &str| -> String {
        match path.strip_prefix('/') {
            Some(rest) if p.detached => rest.to_string(),
            _ => path.to_string(),
        }
    };
    let labelled = |prefix: &str, path: &str| {
        backend.quote_diff_path(format!("{prefix}{}", bare(path)).as_bytes())
    };
    let (old_label, new_label) = (
        labelled(&o.src_prefix, &p.old_path),
        labelled(&o.dst_prefix, &p.new_path),
    );
    let mut head = format!("diff --git {old_label} {new_label}\n");
    let mut tail = String::new();
    match (old, new) {
        (None, Some(b)) => {
            let _ = writeln!(head, "new file mode {}", b.mode());
        }
        (Some(a), None) => {
            let _ = writeln!(head, "deleted file mode {}", a.mode());
        }
        (Some(a), Some(b)) if a.mode() != b.mode() => {
            let _ = writeln!(head, "old mode {}\nnew mode {}", a.mode(), b.mode());
        }
        (Some(a), Some(_)) => tail = format!(" {}", a.mode()),
        (None, None) => {}
    }
    if let Some(m) = &p.moved {
        let word = if m.copy { "copy" } else { "rename" };
        let _ = writeln!(
            head,
            "similarity index {}%\n{word} from {}\n{word} to {}",
            m.percent(),
            backend.quote_diff_path(p.old_path.as_bytes()),
            backend.quote_diff_path(p.new_path.as_bytes())
        );
    }
    let changed = old.zip(new).is_none_or(|(a, b)| a.hash != b.hash);
    if changed {
        let (x, y) = (
            id_of(old, new, |h| o.index_id(h)),
            id_of(new, old, |h| o.index_id(h)),
        );
        let _ = writeln!(head, "index {x}..{y}{tail}");
    }
    let (from, to) = (
        old.map_or("/dev/null".to_string(), |_| old_label.clone()),
        new.map_or("/dev/null".to_string(), |_| new_label.clone()),
    );
    // A name with a space is followed by a tab so that patch(1) finds where it ends.
    let spaced = |label: &str, shown: &str| {
        if shown != "/dev/null" && label.contains(' ') {
            format!("{shown}\t")
        } else {
            shown.to_string()
        }
    };
    let finish = |mut head: String, body: String| {
        paint_header(&mut head, style);
        head + &body
    };
    if c.binary {
        let body = if changed {
            format!("Binary files {from} and {to} differ\n")
        } else {
            String::new()
        };
        return finish(head, body);
    }
    let layout = pyn_diff::Layout {
        context: format.context,
        inter_hunk: o.inter_hunk,
        function: o.function,
    };
    let hunks = crate::xdiff::unified(&c.old, &c.new, &layout, &o.ws);
    if hunks.is_empty() {
        let ignored =
            changed && old.zip(new).is_some_and(|(a, b)| a.mode() == b.mode()) && p.moved.is_none();
        // Every change was one the options ignore, so git prints no header.
        return if ignored {
            String::new()
        } else {
            finish(head, String::new())
        };
    }
    let _ = writeln!(
        head,
        "--- {}\n+++ {}",
        spaced(&old_label, &from),
        spaced(&new_label, &to)
    );
    let body = if let Some(setup) = words {
        String::from_utf8_lossy(&word_diff::render(&hunks, setup, style)).into_owned()
    } else if let Some(st) = style {
        let eof = diff_color::blank_at_eof(&c.old, &c.new, &st.rule);
        String::from_utf8_lossy(&st.hunks(&hunks, eof)).into_owned()
    } else {
        hunks.iter().map(pyn_diff::Hunk::render).collect()
    };
    finish(head, body)
}

/// The color of the `commit` line when output is colored; colored decorations and graphs are not built.
pub fn commit_color(
    backend: &dyn Backend,
    opts: &Opts,
    decorating: bool,
    graph: bool,
) -> Option<String> {
    let style = Style::resolve(opts, &|k| backend.diff_setting(k))?;
    if decorating || graph {
        crate::usage_error("colored output with decorations or a graph is not built");
    }
    Some(style.palette.commit)
}

/// `--check` for one file: the problems in its added lines, with the colors of `style` when there is one.
fn check_file(backend: &dyn Backend, path: &str, c: &Contents, style: Option<&Style>) -> String {
    let rule = diff_color::whitespace_rule(&|k| backend.diff_setting(k));
    let edits = crate::xdiff::edits(&c.old, &c.new, &pyn_diff::Ws::default());
    let paint = style.map(|s| pyn_diff::whitespace::Paint {
        set: &s.palette.new,
        reset: diff_color::RESET,
        ws: &s.palette.whitespace,
    });
    pyn_diff::check_report(path, (&c.old, &c.new), &edits, &rule, paint.as_ref())
}

/// Recolors the header lines written so far.
fn paint_header(out: &mut String, style: Option<&Style>) {
    if let Some(st) = style {
        *out = out.lines().map(|l| st.meta(l)).collect();
    }
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
pub(crate) struct Pair {
    pub(crate) old_path: String,
    pub(crate) new_path: String,
    pub(crate) old: Option<Blob>,
    pub(crate) new: Option<Blob>,
    pub(crate) moved: Option<Moved>,
    /// Compared outside a repository: stat and name-status show both names.
    pub(crate) detached: bool,
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

/// The changed pairs of `old` against `new` limited to `only`, with the renames and copies `opts` finds folded in.
pub(crate) fn pairs(
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
                detached: false,
            },
            None => Pair {
                old_path: path.clone(),
                new_path: path.clone(),
                old: a.cloned(),
                new: b.cloned(),
                moved: None,
                detached: false,
            },
        });
    }
    Ok(out)
}

/// The detection `request` asks for; `-C -C` is refused where callers pass only the changed paths.
pub(crate) fn resolved(
    backend: &dyn Backend,
    request: &rename::Request,
) -> Option<rename::Options> {
    let opts = request
        .resolve(backend.rename_defaults())
        .unwrap_or_else(|message| crate::usage_error(&message));
    if opts.as_ref().is_some_and(|o| o.harder) && !backend.whole_trees() {
        crate::usage_error(
            "`-C -C` (copies from unmodified files) is not built for this repository",
        );
    }
    opts
}

/// A path of `new` that differs from `old`: the blob it came from (following a rename), the blob it is, and git's status letter.
pub struct Origin {
    pub path: String,
    pub from: Option<Blob>,
    pub to: Option<Blob>,
    pub status: char,
}

/// The changed paths of `old` against `new` as `Origin`s, in path order.
pub fn origins(
    backend: &dyn Backend,
    old: &Tree,
    new: &Tree,
    only: &[String],
    renames: rename::Request,
) -> Result<Vec<Origin>> {
    let opts = resolved(backend, &renames);
    let class = |b: &Blob| match b.mode {
        FileMode::Symlink => 1,
        FileMode::Gitlink => 2,
        _ => 0,
    };
    Ok(pairs(backend, old, new, only, opts.as_ref())?
        .into_iter()
        .map(|p| {
            let status = match (&p.old, &p.new, &p.moved) {
                (_, _, Some(m)) if m.copy => 'C',
                (_, _, Some(_)) => 'R',
                (None, _, _) => 'A',
                (_, None, _) => 'D',
                (Some(a), Some(b), _) if class(a) != class(b) => 'T',
                _ => 'M',
            };
            Origin {
                path: p.new_path,
                from: p.old,
                to: p.new,
                status,
            }
        })
        .collect())
}

/// The create, delete, rename, copy and mode-change lines of `--summary` for a pair.
fn summary_line(backend: &dyn Backend, p: &Pair) -> String {
    let mut out = String::new();
    let quote = |s: &str| backend.quote_diff_path(s.as_bytes());
    let (old_path, new_path) = (&quote(&p.old_path), &quote(&p.new_path));
    match (&p.old, &p.new) {
        (None, Some(b)) => {
            let _ = writeln!(out, " create mode {} {}", b.mode(), new_path);
        }
        (Some(a), None) => {
            let _ = writeln!(out, " delete mode {} {}", a.mode(), old_path);
        }
        (Some(a), Some(b)) if a.is_gitlink() != b.is_gitlink() => {
            let _ = writeln!(out, " delete mode {} {}", a.mode(), old_path);
            let _ = writeln!(out, " create mode {} {}", b.mode(), new_path);
        }
        (Some(a), Some(b)) => {
            if let Some(m) = &p.moved {
                let word = if m.copy { "copy" } else { "rename" };
                let name = diffstat::rename_name(old_path, new_path);
                let _ = writeln!(out, " {word} {name} ({}%)", m.percent());
            }
            if a.mode() != b.mode() {
                let tail = if p.moved.is_some() {
                    String::new()
                } else {
                    format!(" {new_path}")
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
            let c = contents(backend, &p.new_path, p.old.as_ref(), p.new.as_ref(), false)?;
            if c.binary && p.moved.is_some() && same {
                Counts::Binary(0, 0)
            } else if c.binary {
                Counts::Binary(
                    blob_size(backend, &p.old_path, p.old.as_ref())?,
                    blob_size(backend, &p.new_path, p.new.as_ref())?,
                )
            } else {
                let (x, y) = crate::xdiff::line_counts(&c.old, &c.new, &pyn_diff::Ws::default());
                Counts::Lines(x, y)
            }
        };
        let quote = |s: &str| backend.quote_diff_path(s.as_bytes());
        let name = if p.moved.is_some() {
            diffstat::rename_name(&quote(&p.old_path), &quote(&p.new_path))
        } else {
            quote(&p.new_path)
        };
        summary.push_str(&summary_line(backend, p));
        rows.push(diffstat::Row { name, counts });
    }
    let mut out = diffstat::render(&rows, layout.0, layout.1);
    out.push_str(&summary);
    Ok(out)
}

/// A text file whose content differs from the other side only in what `ws` ignores.
fn whitespace_only(
    old: Option<&Blob>,
    new: Option<&Blob>,
    c: &Contents,
    ws: &pyn_diff::Ws,
) -> bool {
    ws.compares_loosely() && ignored_only(old, new, c, ws)
}

fn ignored_only(old: Option<&Blob>, new: Option<&Blob>, c: &Contents, ws: &pyn_diff::Ws) -> bool {
    old.zip(new)
        .is_some_and(|(a, b)| a.mode() == b.mode() && !c.binary)
        && crate::xdiff::line_counts(&c.old, &c.new, ws) == (0, 0)
}

/// Whether `show` separates the diff from the message: git does for `--check`, `--summary` and `--dirstat` on any change, even a clean one.
pub fn has_body(format: &Format, differs: bool, body: &str) -> bool {
    !body.is_empty()
        || (matches!(format.kind, Kind::Check | Kind::Summary | Kind::Dirstat) && differs)
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
    let opts = resolved(backend, &format.renames);
    let (old, new) = if format.opts.reverse {
        (new, old)
    } else {
        (old, new)
    };
    let mut list = pairs(backend, old, new, only, opts.as_ref())?;
    if !format.kept.is_empty() {
        list.retain(|p| format.kept.contains(&p.new_path));
    }
    if let Some((keep, skip)) = &format.opts.filter {
        if keep.contains('*') {
            crate::usage_error("`--diff-filter=*` is not built");
        }
        list.retain(|p| {
            let letter = status_of(p).chars().next().unwrap_or('M');
            !skip.contains(letter) && (keep.is_empty() || keep.contains(letter))
        });
    }
    emit(backend, &list, format, out)
}

/// Appends the rendering of the changed `list` to `out`; true if any file differs.
pub(crate) fn emit(
    backend: &dyn Backend,
    list: &[Pair],
    format: &Format,
    out: &mut String,
) -> Result<bool> {
    let format = &Format {
        opts: format.opts.with_config(&|k| backend.diff_setting(k)),
        ..format.clone()
    };
    let summary: String = if format.summary || format.kind == Kind::Summary {
        list.iter().map(|p| summary_line(backend, p)).collect()
    } else {
        String::new()
    };
    if format.kind == Kind::Summary {
        out.push_str(&summary);
        return Ok(!list.is_empty());
    }
    let mut body = String::new();
    let differs = render(backend, list, format, &mut body)?;
    let start = out.len();
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
    if format.with_patch {
        let patch_format = Format {
            kind: Kind::Patch,
            summary: false,
            with_patch: false,
            ..format.clone()
        };
        let mut patch = String::new();
        render(backend, list, &patch_format, &mut patch)?;
        // The blank line before the patch is left out after a dirstat by changes or files.
        let by_line = format
            .opts
            .dirstat
            .is_some_and(|d| d.measure == dirstat::Measure::Lines);
        let before =
            ((format.kind != Kind::Dirstat || by_line) && out.len() > start) || !summary.is_empty();
        if before && !patch.is_empty() {
            out.push('\n');
        }
        out.push_str(&patch);
    }
    Ok(differs)
}

/// What `--compact-summary` says about a pair next to its name.
fn compact_note(p: &Pair) -> Option<String> {
    let exec = |b: &Blob| b.mode == FileMode::Executable;
    let link = |b: &Blob| b.mode == FileMode::Symlink;
    match (&p.old, &p.new) {
        (None, Some(b)) => Some(format!(
            "new{}",
            if link(b) {
                " +l"
            } else if exec(b) {
                " +x"
            } else {
                ""
            }
        )),
        (Some(_), None) => Some("gone".into()),
        (Some(a), Some(b)) if a.mode != b.mode => Some(format!(
            "mode{}",
            if link(a) != link(b) {
                if link(b) { " +l" } else { " -l" }
            } else if exec(b) {
                " +x"
            } else {
                " -x"
            }
        )),
        _ => None,
    }
}

/// What `--dirstat` counts for a pair: bytes changed, lines changed or one per file.
fn dirstat_damage(
    backend: &dyn Backend,
    p: &Pair,
    measure: dirstat::Measure,
    opts: &Opts,
) -> Result<u64> {
    let ws = &opts.ws;
    let changed = p.old.as_ref().zip(p.new.as_ref()).is_none_or(|(a, b)| {
        p.detached || a.hash != b.hash || !diff_raw::id_known(a) || !diff_raw::id_known(b)
    });
    if !changed {
        return Ok(0);
    }
    let read = |path: &str, blob: &Option<Blob>| -> Result<Vec<u8>> {
        blob.as_ref()
            .map_or(Ok(Vec::new()), |b| blob_bytes(backend, path, b))
    };
    let (old, new) = (read(&p.old_path, &p.old)?, read(&p.new_path, &p.new)?);
    Ok(match measure {
        dirstat::Measure::Files => 1,
        dirstat::Measure::Changes => (if p.old.is_none() {
            new.len() as u64
        } else if p.new.is_none() {
            old.len() as u64
        } else {
            dirstat::byte_damage(&old, &new)
        })
        .max(1),
        dirstat::Measure::Lines => {
            if pyn_diff::is_binary(&old) || pyn_diff::is_binary(&new) {
                ((old.len() + new.len()) as u64).div_ceil(64)
            } else {
                let (a, d) = crate::xdiff::line_counts(&old, &new, ws);
                (a + d) as u64
            }
        }
    })
}

/// Whether the two sides are not the same kind of thing: a file, a symbolic link or a submodule.
fn type_changed(a: &Blob, b: &Blob) -> bool {
    let kind = |m: FileMode| match m {
        FileMode::Regular | FileMode::Executable => 0,
        FileMode::Symlink => 1,
        FileMode::Gitlink => 2,
        FileMode::Tree => 3,
    };
    kind(a.mode) != kind(b.mode)
}

/// The status letter of a pair, with the similarity of a rename or copy.
pub(crate) fn status_of(p: &Pair) -> String {
    if let Some(m) = &p.moved {
        return format!("{}{:03}", if m.copy { 'C' } else { 'R' }, m.percent());
    }
    match (&p.old, &p.new) {
        (None, _) => "A",
        (_, None) => "D",
        (Some(a), Some(b)) if type_changed(a, b) => "T",
        _ => "M",
    }
    .into()
}

fn render(backend: &dyn Backend, list: &[Pair], format: &Format, out: &mut String) -> Result<bool> {
    let style = Style::resolve(&format.opts, &|k| backend.diff_setting(k));
    let style = style.as_ref();
    let words = word_diff::Setup::of(&format.opts, &|k| backend.diff_setting(k));
    let mut stat_rows: Vec<(String, Counts)> = Vec::new();
    let mut moved_from: Vec<Option<(String, String)>> = Vec::new();
    let mut dirstat_rows: Vec<(String, u64)> = Vec::new();
    let mut differs = false;
    let (sep, end) = if format.opts.nul {
        ('\0', '\0')
    } else {
        ('\t', '\n')
    };
    let quote = |s: &str| {
        if format.opts.nul {
            s.to_string()
        } else {
            backend.quote_diff_path(s.as_bytes())
        }
    };
    for p in list {
        let (a, b) = (p.old.as_ref(), p.new.as_ref());
        let path = &p.new_path;
        match format.kind {
            Kind::NameOnly => {
                differs = true;
                let shown = if p.detached && b.is_none() {
                    "/dev/null".to_string()
                } else {
                    quote(path)
                };
                out.push_str(&format!("{shown}{end}"));
            }
            Kind::NameStatus => {
                differs = true;
                let names = match &p.moved {
                    Some(_) => format!("{sep}{}{sep}{}", quote(&p.old_path), quote(path)),
                    None if p.detached && p.old.is_some() => format!("{sep}{}", quote(&p.old_path)),
                    None => format!("{sep}{}", quote(path)),
                };
                out.push_str(&format!("{}{names}{end}", status_of(p)));
            }
            Kind::Raw => {
                differs = true;
                out.push_str(&diff_raw::record(p, &format.opts, &quote));
            }
            Kind::Dirstat => {
                differs = true;
                let spec = format.opts.dirstat.unwrap_or_default();
                let damage = dirstat_damage(backend, p, spec.measure, &format.opts)?;
                let name = if p.detached && b.is_none() {
                    "/dev/null"
                } else {
                    path
                };
                dirstat_rows.push((name.to_string(), damage));
            }
            Kind::Summary => {}
            Kind::Nothing => differs = true,
            Kind::Patch | Kind::Check | Kind::Stat | Kind::ShortStat | Kind::NumStat => {
                if format.kind == Kind::Patch && a.zip(b).is_some_and(|(a, b)| type_changed(a, b)) {
                    differs = true;
                    for (old, new) in [(a, None), (None, b)] {
                        let split = Pair {
                            old: old.cloned(),
                            new: new.cloned(),
                            moved: None,
                            ..(*p).clone()
                        };
                        let c = contents(backend, path, old, new, format.opts.text)?;
                        out.push_str(&patch(backend, &split, &c, format, (style, words.as_ref())));
                    }
                    continue;
                }
                let c = contents(backend, path, a, b, format.opts.text)?;
                if format.opts.ws.any()
                    && format.kind != Kind::Check
                    && p.moved.is_none()
                    && whitespace_only(a, b, &c, &format.opts.ws)
                {
                    continue;
                }
                differs = true;
                let stat = matches!(format.kind, Kind::Stat | Kind::ShortStat | Kind::NumStat);
                if stat
                    && format.opts.ws.blank_lines
                    && p.moved.is_none()
                    && ignored_only(a, b, &c, &format.opts.ws)
                {
                    continue;
                }
                if format.kind == Kind::Patch {
                    out.push_str(&patch(backend, p, &c, format, (style, words.as_ref())));
                    continue;
                }
                if format.kind == Kind::Check {
                    if !c.binary {
                        out.push_str(&check_file(backend, path, &c, style));
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
                    let (x, y) = crate::xdiff::line_counts(&c.old, &c.new, &format.opts.ws);
                    Counts::Lines(x, y)
                };
                let mut name = match (&p.moved, p.detached) {
                    (Some(_), _) => diffstat::rename_name(&quote(&p.old_path), &quote(path)),
                    (None, true) => match (a, b) {
                        (Some(_), Some(_)) if p.old_path != *path => {
                            diffstat::rename_name(&quote(&p.old_path), &quote(path))
                        }
                        (Some(_), None) => format!("{} => /dev/null", quote(&p.old_path)),
                        (None, Some(_)) => format!("/dev/null => {}", quote(path)),
                        _ => quote(path),
                    },
                    (None, false) => quote(path),
                };
                if let Some(note) = compact_note(p).filter(|_| format.opts.compact_summary) {
                    let _ = write!(name, " ({note})");
                }
                stat_rows.push((name, counts));
                moved_from.push(
                    p.moved
                        .as_ref()
                        .map(|_| (p.old_path.clone(), p.new_path.clone())),
                );
            }
        }
    }
    if format.kind == Kind::Dirstat {
        let spec = format.opts.dirstat.unwrap_or_default();
        out.push_str(&dirstat::render(dirstat_rows, spec));
    }
    if format.kind == Kind::NumStat {
        for ((path, counts), from) in stat_rows.iter().zip(&moved_from) {
            let (x, y) = match counts {
                Counts::Lines(a, d) => (a.to_string(), d.to_string()),
                Counts::Binary(..) => ("-".into(), "-".into()),
            };
            match (from, format.opts.nul) {
                (Some((old, new)), true) => {
                    let _ = write!(out, "{x}\t{y}\t\0{old}\0{new}\0");
                }
                (_, true) => {
                    let _ = write!(out, "{x}\t{y}\t{path}\0");
                }
                _ => {
                    let _ = writeln!(out, "{x}\t{y}\t{path}");
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
        let spec = &format.opts.stat;
        let layout = diffstat::Layout {
            width: spec
                .width
                .filter(|w| *w > 0)
                .unwrap_or_else(diffstat::terminal_width),
            name_width: spec.name_width,
            graph_width: spec.graph_width.or_else(|| {
                backend
                    .diff_setting("diff.statgraphwidth")
                    .and_then(|v| v.parse().ok())
            }),
            count: spec.count,
            colors: style.map(|s| (s.palette.new.clone(), s.palette.old.clone())),
        };
        out.push_str(&diffstat::render_with(&rows, &layout));
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
    pub dirstat: bool,
    pub patch: bool,
    pub context: usize,
    pub opts: Opts,
}

impl DiffParts {
    pub fn any(&self) -> bool {
        self.names.is_some()
            || self.stat
            || self.shortstat
            || self.dirstat
            || self.patch
            || self.summary
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
            opts: parts.opts.clone(),
            renames: parts.renames,
            summary: false,
            with_patch: false,
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
    let mut separated = !body.is_empty();
    if parts.dirstat {
        body.push_str(&section(Kind::Dirstat)?);
        separated |= parts
            .opts
            .dirstat
            .is_some_and(|d| d.measure == dirstat::Measure::Lines && !body.is_empty());
    }
    if parts.summary {
        body.push_str(&section(Kind::Summary)?);
    }
    if parts.patch {
        let patch = section(Kind::Patch)?;
        if separated || parts.summary {
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

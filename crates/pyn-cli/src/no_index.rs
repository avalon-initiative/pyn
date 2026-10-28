//! `diff --no-index`: two files, or two directories compared recursively, with no repository involved.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use pyn_object::{Algorithm, Kind as ObjectKind};
use pyn_proto as api;

use crate::backend::Backend;
use crate::diff::{self, Blob, Format, Pair, Source, Tree};
use crate::git_home::GitHome;
use crate::local_store::file_mode;
use crate::localfile;
use crate::rename;

/// A stand-in home for a diff that reads nothing from a repository.
struct Detached;

macro_rules! none {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> Result<$ret> {
            $(let _ = $arg;)*
            anyhow::bail!("not in a repository")
        })*
    };
}

impl Backend for Detached {
    fn rename_defaults(&self) -> rename::Defaults {
        rename::Defaults {
            mode: Some(rename::Mode::Renames),
            ..rename::Defaults::default()
        }
    }

    none! {
        whoami() -> String;
        list_files() -> Vec<api::FileEntry>;
        tree(path: Option<&str>) -> api::TreeListing;
        head_commit() -> Option<String>;
        commit_page(before: Option<&str>, limit: usize) -> api::CommitPage;
        commit(id: &str) -> api::Commit;
        snapshot(commit: Option<&str>) -> (Option<String>, Vec<api::SnapshotEntry>);
        content(path: &str, revision: u64, probe: Option<usize>) -> Vec<u8>;
        read(path: &str, revision: Option<u64>, out: &mut dyn std::io::Write) -> ();
        download(path: &str, revision: Option<u64>, dest: &Path) -> (u64, String);
        upload(file: &Path) -> String;
        upload_bytes(bytes: Vec<u8>) -> String;
        commit_file(request: &api::CheckinRequest) -> api::Revision;
        commit_changeset(request: &api::ChangesetRequest) -> api::ChangesetResponse;
        path_history(path: &str) -> Vec<api::Revision>;
        repo_history(filter: Option<&str>, limit: usize) -> Vec<api::Revision>;
        restore(request: &api::RestoreRequest) -> api::Revision;
    }
}

/// What the walk found, in git's order.
enum Entry {
    Changed(Pair),
    Deleted(String, Blob),
    Added(String, Blob),
}

fn blob_at(path: &Path, algorithm: Algorithm) -> Result<Blob> {
    let (mode, bytes) = localfile::read(path)?;
    Ok(Blob {
        hash: pyn_object::hash_object(algorithm, ObjectKind::Blob, &bytes)?.to_hex(),
        mode: file_mode(mode),
        source: Source::Local {
            file: path.to_path_buf(),
        },
    })
}

fn names(dir: &Path) -> Result<Vec<std::ffi::OsString>> {
    let mut found: Vec<_> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<std::io::Result<_>>()?;
    found.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
    Ok(found)
}

fn is_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

fn is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() || m.file_type().is_symlink())
}

/// Every file under `dir` as deleted or added.
fn sweep(
    dir: &Path,
    name: &str,
    algorithm: Algorithm,
    added: bool,
    out: &mut Vec<Entry>,
) -> Result<()> {
    for file in names(dir)? {
        let (path, shown) = (
            dir.join(&file),
            format!("{name}/{}", file.to_string_lossy()),
        );
        if is_dir(&path) {
            sweep(&path, &shown, algorithm, added, out)?;
        } else if is_file(&path) {
            let blob = blob_at(&path, algorithm)?;
            out.push(if added {
                Entry::Added(shown, blob)
            } else {
                Entry::Deleted(shown, blob)
            });
        }
    }
    Ok(())
}

fn walk(
    a: (&Path, &str),
    b: (&Path, &str),
    algorithm: Algorithm,
    out: &mut Vec<Entry>,
) -> Result<()> {
    let mut all = names(a.0)?;
    all.extend(names(b.0)?);
    all.sort_by(|x, y| x.as_encoded_bytes().cmp(y.as_encoded_bytes()));
    all.dedup();
    for file in all {
        let label = file.to_string_lossy();
        let (pa, pb) = (a.0.join(&file), b.0.join(&file));
        let (na, nb) = (format!("{}/{label}", a.1), format!("{}/{label}", b.1));
        match (
            std::fs::symlink_metadata(&pa).ok(),
            std::fs::symlink_metadata(&pb).ok(),
        ) {
            (Some(x), Some(y)) if x.is_dir() && y.is_dir() => {
                walk((&pa, &na), (&pb, &nb), algorithm, out)?
            }
            (Some(x), Some(y)) if x.is_dir() != y.is_dir() => {
                crate::usage_error("a file and a directory of the same name are not compared here");
            }
            (Some(_), Some(_)) => {
                if is_file(&pa) && is_file(&pb) {
                    out.push(changed(&pa, &na, &pb, &nb, algorithm)?);
                }
            }
            (Some(x), None) if x.is_dir() => sweep(&pa, &na, algorithm, false, out)?,
            (None, Some(y)) if y.is_dir() => sweep(&pb, &nb, algorithm, true, out)?,
            (Some(_), None) if is_file(&pa) => {
                out.push(Entry::Deleted(na, blob_at(&pa, algorithm)?))
            }
            (None, Some(_)) if is_file(&pb) => out.push(Entry::Added(nb, blob_at(&pb, algorithm)?)),
            _ => {}
        }
    }
    Ok(())
}

fn changed(pa: &Path, na: &str, pb: &Path, nb: &str, algorithm: Algorithm) -> Result<Entry> {
    let (old, new) = (blob_at(pa, algorithm)?, blob_at(pb, algorithm)?);
    Ok(Entry::Changed(Pair {
        old_path: na.to_string(),
        new_path: nb.to_string(),
        old: Some(old),
        new: Some(new),
        moved: None,
        rewrite: None,
        detached: true,
    }))
}

/// The entry as seen from the other side, for `-R`.
fn reversed(entry: Entry) -> Entry {
    match entry {
        Entry::Changed(p) => Entry::Changed(Pair {
            old_path: p.new_path,
            new_path: p.old_path,
            old: p.new,
            new: p.old,
            ..p
        }),
        Entry::Deleted(n, b) => Entry::Added(n, b),
        Entry::Added(n, b) => Entry::Deleted(n, b),
    }
}

fn access_error(path: &str) -> ! {
    eprintln!("error: Could not access '{path}'");
    std::process::exit(1);
}

/// A path argument: where it is and the name it is shown as (a directory loses trailing slashes).
fn operand(arg: &str) -> Option<(PathBuf, String)> {
    if arg == "/dev/null" {
        return None;
    }
    if std::fs::symlink_metadata(arg).is_err() {
        access_error(arg);
    }
    let shown = if is_dir(Path::new(arg)) {
        arg.strip_suffix('/').unwrap_or(arg).to_string()
    } else {
        arg.to_string()
    };
    Some((
        PathBuf::from(arg),
        if shown.is_empty() { "/".into() } else { shown },
    ))
}

/// Whether the arguments are exactly two paths that exist, which outside a repository `diff` compares as files.
pub fn are_paths<'a>(operands: impl Iterator<Item = &'a String>) -> bool {
    let all: Vec<&String> = operands.collect();
    all.len() == 2
        && all
            .iter()
            .all(|p| p.as_str() == "/dev/null" || std::fs::symlink_metadata(p.as_str()).is_ok())
}

/// Joins the file name of `file` onto `dir`, as git does to compare a file with a directory.
fn inside(dir: &(PathBuf, String), file: &(PathBuf, String)) -> (PathBuf, String) {
    let base = Path::new(&file.1)
        .file_name()
        .map(|b| b.to_string_lossy().into_owned())
        .unwrap_or_default();
    let shown = format!("{}/{base}", dir.1);
    if std::fs::symlink_metadata(dir.0.join(&base)).is_err() {
        access_error(&shown);
    }
    (dir.0.join(&base), shown)
}

fn entries(
    left: Option<(PathBuf, String)>,
    right: Option<(PathBuf, String)>,
    algorithm: Algorithm,
) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    match (left, right) {
        (None, None) => {}
        (Some(a), None) if !is_dir(&a.0) => {
            out.push(Entry::Deleted(a.1, blob_at(&a.0, algorithm)?));
        }
        (None, Some(a)) if !is_dir(&a.0) => {
            out.push(Entry::Added(a.1, blob_at(&a.0, algorithm)?));
        }
        (Some(a), Some(b)) => match (is_dir(&a.0), is_dir(&b.0)) {
            (true, true) => walk((&a.0, &a.1), (&b.0, &b.1), algorithm, &mut out)?,
            (false, false) => out.push(changed(&a.0, &a.1, &b.0, &b.1, algorithm)?),
            (false, true) => {
                let (p, n) = inside(&b, &a);
                out.push(changed(&a.0, &a.1, &p, &n, algorithm)?);
            }
            (true, false) => {
                let (p, n) = inside(&a, &b);
                out.push(changed(&p, &n, &b.0, &b.1, algorithm)?);
            }
        },
        _ => crate::usage_error("a directory against /dev/null is not compared here"),
    }
    Ok(out)
}

/// Compares the two paths named, printing what differs; true if anything does.
pub fn run(
    operands: &[String],
    format: &Format,
    quiet: bool,
    git: Option<&GitHome>,
) -> Result<bool> {
    let [left, right] = operands else {
        crate::usage_error("`--no-index` takes exactly two paths");
    };
    if left == "-" || right == "-" {
        crate::usage_error("`--no-index` reading standard input is not built");
    }
    let algorithm = git.map_or(Algorithm::Sha1, |g| g.algorithm());
    let mut found = entries(operand(left), operand(right), algorithm)?;
    if format.opts.reverse {
        found = found.into_iter().map(reversed).collect();
    }
    let detached = Detached;
    let backend: &dyn Backend = match git {
        Some(g) => g,
        None => &detached,
    };
    let opts = format
        .renames
        .resolve(backend.rename_defaults())
        .unwrap_or_else(|message| crate::usage_error(&message));
    let (mut gone, mut came): (Tree, Tree) = (BTreeMap::new(), BTreeMap::new());
    for e in &found {
        match e {
            Entry::Deleted(n, b) => {
                gone.insert(n.clone(), b.clone());
            }
            Entry::Added(n, b) => {
                came.insert(n.clone(), b.clone());
            }
            Entry::Changed(_) => {}
        }
    }
    if opts.as_ref().is_some_and(|o| o.brk.is_some()) {
        crate::usage_error("`-B` with `--no-index` is not built");
    }
    let moves = match &opts {
        Some(o) if o.mode == rename::Mode::Copies => {
            crate::usage_error("`-C` with `--no-index` is not built")
        }
        Some(o) => rename::detect(backend, &gone, &came, o)?,
        None => rename::Detected::default(),
    };
    let mut list = Vec::new();
    for e in found {
        match e {
            Entry::Changed(p) => {
                let same = p
                    .old
                    .as_ref()
                    .zip(p.new.as_ref())
                    .is_some_and(|(a, b)| a.hash == b.hash && a.mode == b.mode);
                if !same {
                    list.push(p);
                }
            }
            Entry::Deleted(n, b) if !moves.consumed.contains(&n) => list.push(Pair {
                old_path: n.clone(),
                new_path: n,
                old: Some(b),
                new: None,
                moved: None,
                rewrite: None,
                detached: true,
            }),
            Entry::Added(n, b) => {
                let moved = moves.moved.get(&n).cloned();
                list.push(match moved {
                    Some(m) => Pair {
                        old_path: m.src.clone(),
                        new_path: n,
                        old: gone.get(&m.src).cloned(),
                        new: Some(b),
                        moved: Some(m),
                        rewrite: None,
                        detached: true,
                    },
                    None => Pair {
                        old_path: n.clone(),
                        new_path: n,
                        old: None,
                        new: Some(b),
                        moved: None,
                        rewrite: None,
                        detached: true,
                    },
                });
            }
            Entry::Deleted(..) => {}
        }
    }
    let mut out = String::new();
    let differs = diff::emit(backend, &list, format, &mut out)?;
    if !quiet {
        print!("{out}");
        diff::exit_if_check_failed(format, &out);
    }
    Ok(differs)
}

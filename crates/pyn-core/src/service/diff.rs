//! Line diffs between two revisions, and the lines of a file for the context controls (docs/concepts/diff.md).

use std::collections::{BTreeMap, HashMap};

use pyn_diff::{Layout, Op, Ws};
use pyn_object::{FileMode, ObjectId};

use super::RepoService;
use super::compare::{ChangeKind, Changed};
use crate::error::{PynError, Result};
use crate::rules::Mode;
use crate::types::{Lock, RepoPath, Revision};

/// Largest side, in bytes, whose lines are diffed; a bigger file is listed with `too_large` and no hunks.
pub const DIFF_MAX_FILE_BYTES: u64 = 1 << 20;
/// Bytes of content one diff reads in all; files after the budget is spent are `too_large`.
pub const DIFF_MAX_TOTAL_BYTES: u64 = 16 << 20;
/// Most unchanged lines around a change a diff may ask for.
pub const DIFF_MAX_CONTEXT: usize = 100;
/// Context lines when a request names none.
pub const DIFF_DEFAULT_CONTEXT: usize = 3;
/// Most lines one `file_lines` call returns.
pub const LINES_MAX: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Add,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    /// The line without its newline, invalid UTF-8 replaced.
    pub text: String,
    /// The side's last line has no newline.
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    /// The `@@ -a,b +c,d @@ heading` line.
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
    /// Unchanged lines between the previous hunk (or the file's start) and this one.
    pub hidden_before: u32,
    /// Unchanged lines after the hunk to the file's end; set on the last hunk only.
    pub hidden_after: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: RepoPath,
    pub old_path: Option<RepoPath>,
    pub status: DiffStatus,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    pub too_large: bool,
    pub mode: Mode,
    pub file_mode: FileMode,
    /// The previous git file mode, when it changed.
    pub old_file_mode: Option<FileMode>,
    /// Git blob ids of the two sides; absent on the side the path does not exist.
    pub old_content: Option<ObjectId>,
    pub new_content: Option<ObjectId>,
    /// The live lock on the path, whoever holds it.
    pub lock: Option<Lock>,
    pub hunks: Vec<DiffHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub base: String,
    pub base_commit: ObjectId,
    pub head: String,
    pub head_commit: ObjectId,
    /// The commit the changes are measured from: the merge base, or the base commit for a direct diff.
    pub from_commit: Option<ObjectId>,
    pub files: Vec<FileDiff>,
    /// More paths changed than listed.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct DiffOptions {
    /// Measure from the merge base (what a pull request shows) instead of from `base` itself.
    pub merge_base: bool,
    pub context: usize,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            merge_base: true,
            context: DIFF_DEFAULT_CONTEXT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLines {
    pub path: RepoPath,
    pub commit: ObjectId,
    pub revision: u64,
    /// Lines in the whole file.
    pub total: u32,
    /// The first returned line, counting from 1.
    pub start: u32,
    pub lines: Vec<String>,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn text_of(line: &[u8]) -> (String, bool) {
    let no_newline = !line.ends_with(b"\n");
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    (String::from_utf8_lossy(line).into_owned(), no_newline)
}

/// The hunks of `old` against `new`, with the hidden-line counts the context controls need.
fn hunks_of(old: &[u8], new: &[u8], context: usize) -> Vec<DiffHunk> {
    let layout = Layout {
        context,
        ..Layout::default()
    };
    let raw = pyn_diff::hunks_with(old, new, &layout, &Ws::default());
    let old_total = pyn_diff::split_lines(old).len();
    let mut cursor = 1usize;
    let mut out = Vec::with_capacity(raw.len());
    for h in &raw {
        let (mut o, mut n) = (h.old_start, h.new_start);
        let lines = h
            .lines
            .iter()
            .map(|(op, bytes)| {
                let (text, no_newline) = text_of(bytes);
                let (kind, old_no, new_no) = match op {
                    Op::Equal => (DiffLineKind::Context, Some(o), Some(n)),
                    Op::Delete => (DiffLineKind::Delete, Some(o), None),
                    Op::Insert => (DiffLineKind::Add, None, Some(n)),
                };
                if *op != Op::Insert {
                    o += 1;
                }
                if *op != Op::Delete {
                    n += 1;
                }
                DiffLine {
                    kind,
                    old_no: old_no.map(count),
                    new_no: new_no.map(count),
                    text,
                    no_newline,
                }
            })
            .collect();
        out.push(DiffHunk {
            header: h.header().trim_end().to_string(),
            old_start: count(h.old_start),
            old_lines: count(h.old_count),
            new_start: count(h.new_start),
            new_lines: count(h.new_count),
            lines,
            hidden_before: count(h.old_start.saturating_sub(cursor)),
            hidden_after: 0,
        });
        cursor = h.old_start + h.old_count;
    }
    if let Some(last) = out.last_mut() {
        last.hidden_after = count((old_total + 1).saturating_sub(cursor));
    }
    out
}

impl RepoService {
    /// The line diff of `head` against `base` (the default branch when `None`), per changed path. With
    /// `merge_base` the base side is the two commits' merge base, as for a pull request.
    pub async fn diff(&self, base: Option<&str>, head: &str, options: DiffOptions) -> Result<Diff> {
        if options.context > DIFF_MAX_CONTEXT {
            return Err(PynError::InvalidRequest(format!(
                "context is at most {DIFF_MAX_CONTEXT} lines"
            )));
        }
        let (base_commit, base_name) = match base {
            Some(spec) => self.resolve_ref(spec).await?,
            None => self.resolve_ref(crate::branch::DEFAULT_BRANCH).await?,
        };
        let (head_commit, head_name) = self.resolve_ref(head).await?;
        let ahead = self.commits_between(head_commit, base_commit).await?;
        let (from, commits) = if options.merge_base {
            let from = self
                .meta
                .merge_bases(&self.repo, &base_commit, &head_commit)
                .await?
                .into_iter()
                .next();
            (from, ahead)
        } else {
            let mut commits = ahead;
            commits.extend(self.commits_between(base_commit, head_commit).await?);
            (Some(base_commit), commits)
        };
        let (changed, truncated) = self
            .changed_paths(&commits, &head_commit, from.as_ref())
            .await?;
        let locks: BTreeMap<RepoPath, Lock> = self
            .locks()
            .await?
            .into_iter()
            .map(|l| (l.path.clone(), l))
            .collect();
        let mut budget = DIFF_MAX_TOTAL_BYTES;
        let mut files = Vec::new();
        for (path, old_path, c) in pair_renames(changed) {
            let lock = locks.get(&path).cloned();
            files.push(
                self.file_diff(path, old_path, c, lock, options.context, &mut budget)
                    .await?,
            );
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Diff {
            base: base_name,
            base_commit,
            head: head_name,
            head_commit,
            from_commit: from,
            files,
            truncated,
        })
    }

    async fn file_diff(
        &self,
        path: RepoPath,
        old_path: Option<RepoPath>,
        c: Changed,
        lock: Option<Lock>,
        context: usize,
        budget: &mut u64,
    ) -> Result<FileDiff> {
        let status = match (c.kind, &old_path) {
            (ChangeKind::Added, Some(_)) => DiffStatus::Renamed,
            (ChangeKind::Added, None) => DiffStatus::Added,
            (ChangeKind::Deleted, _) => DiffStatus::Deleted,
            (ChangeKind::Modified, _) => DiffStatus::Modified,
        };
        let (old, new) = (c.before.as_ref(), c.now.as_ref());
        let mut file = FileDiff {
            mode: self.mode_for(&path),
            file_mode: new.or(old).map_or(FileMode::Regular, |r| r.file_mode),
            old_file_mode: match (old, new) {
                (Some(o), Some(n)) if o.file_mode != n.file_mode => Some(o.file_mode),
                _ => None,
            },
            old_content: old.map(|r| r.content),
            new_content: new.map(|r| r.content),
            path,
            old_path,
            status,
            additions: 0,
            deletions: 0,
            binary: false,
            too_large: false,
            lock,
            hunks: Vec::new(),
        };
        if old.zip(new).is_some_and(|(o, n)| o.content == n.content) {
            return Ok(file);
        }
        let (old_bytes, new_bytes) =
            match (self.side(old, budget).await?, self.side(new, budget).await?) {
                (Some(o), Some(n)) => (o, n),
                _ => {
                    file.too_large = true;
                    return Ok(file);
                }
            };
        if pyn_diff::is_binary(&old_bytes) || pyn_diff::is_binary(&new_bytes) {
            file.binary = true;
            return Ok(file);
        }
        let permit = self
            .merge_slots
            .acquire()
            .await
            .map_err(|e| PynError::Storage(e.to_string()))?;
        let hunks = tokio::task::spawn_blocking(move || hunks_of(&old_bytes, &new_bytes, context))
            .await
            .map_err(|e| PynError::Storage(e.to_string()));
        drop(permit);
        file.hunks = hunks?;
        for line in file.hunks.iter().flat_map(|h| &h.lines) {
            match line.kind {
                DiffLineKind::Add => file.additions += 1,
                DiffLineKind::Delete => file.deletions += 1,
                DiffLineKind::Context => {}
            }
        }
        Ok(file)
    }

    /// A side's bytes, empty for a missing side; `None` when it is over the per-file limit or the diff's budget.
    async fn side(&self, rev: Option<&Revision>, budget: &mut u64) -> Result<Option<Vec<u8>>> {
        let Some(rev) = rev else {
            return Ok(Some(Vec::new()));
        };
        let size = self
            .objects
            .size(&rev.content)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(rev.content.to_string()))?;
        if size > DIFF_MAX_FILE_BYTES || size > *budget {
            return Ok(None);
        }
        *budget -= size;
        let bytes = self
            .objects
            .get_limited(&rev.content, DIFF_MAX_FILE_BYTES)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(rev.content.to_string()))?;
        Ok(Some(bytes))
    }

    /// Lines `start` to `end` (1-based, inclusive; at most `LINES_MAX`) of `path` as of `rev`, a branch or commit.
    pub async fn file_lines(
        &self,
        rev: &str,
        path: &RepoPath,
        start: u32,
        end: Option<u32>,
    ) -> Result<FileLines> {
        if start == 0 || end.is_some_and(|e| e < start) {
            return Err(PynError::InvalidRequest(
                "lines are counted from 1 and end must not be before start".into(),
            ));
        }
        let (commit, _) = self.resolve_ref(rev).await?;
        let found = self
            .meta
            .revision_at(&self.repo, &commit, path)
            .await?
            .filter(|r| !r.deleted)
            .ok_or_else(|| PynError::RevisionNotFound {
                path: path.clone(),
                revision: commit.to_hex(),
            })?;
        found.verify()?;
        let size = self
            .objects
            .size(&found.content)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(found.content.to_string()))?;
        if size > DIFF_MAX_FILE_BYTES {
            return Err(PynError::InvalidRequest(format!(
                "{path} is larger than {DIFF_MAX_FILE_BYTES} bytes"
            )));
        }
        let bytes = self
            .objects
            .get_limited(&found.content, DIFF_MAX_FILE_BYTES)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(found.content.to_string()))?;
        if pyn_diff::is_binary(&bytes) {
            return Err(PynError::InvalidRequest(format!("{path} is binary")));
        }
        let all = pyn_diff::split_lines(&bytes);
        let first = (start as usize - 1).min(all.len());
        let last = end
            .map_or(usize::MAX, |e| e as usize)
            .min(first.saturating_add(LINES_MAX))
            .min(all.len());
        Ok(FileLines {
            path: path.clone(),
            commit,
            revision: found.id.0,
            total: count(all.len()),
            start,
            lines: all[first..last].iter().map(|l| text_of(l).0).collect(),
        })
    }
}

/// Pairs each deleted path with an added one of identical content (an exact rename, when the match is unique) and
/// drops the deletion.
fn pair_renames(changed: Vec<Changed>) -> Vec<(RepoPath, Option<RepoPath>, Changed)> {
    let mut deleted: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    let mut added: HashMap<ObjectId, Vec<usize>> = HashMap::new();
    for (i, c) in changed.iter().enumerate() {
        let (map, rev) = match c.kind {
            ChangeKind::Deleted => (&mut deleted, &c.before),
            ChangeKind::Added => (&mut added, &c.now),
            ChangeKind::Modified => continue,
        };
        if let Some(r) = rev {
            map.entry(r.content).or_default().push(i);
        }
    }
    let mut from_of: BTreeMap<usize, usize> = BTreeMap::new();
    for (content, gone) in &deleted {
        if let (&[d], Some(&[a])) = (gone.as_slice(), added.get(content).map(Vec::as_slice)) {
            from_of.insert(a, d);
        }
    }
    let dropped: Vec<usize> = from_of.values().copied().collect();
    let olds: Vec<(RepoPath, Option<Revision>)> = changed
        .iter()
        .map(|c| (c.path.clone(), c.before.clone()))
        .collect();
    changed
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !dropped.contains(i))
        .map(|(i, mut c)| {
            let old = from_of.get(&i).map(|d| {
                c.before = olds[*d].1.clone();
                olds[*d].0.clone()
            });
            (c.path.clone(), old, c)
        })
        .collect()
}

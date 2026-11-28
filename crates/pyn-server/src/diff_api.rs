//! Line diffs between two revisions, and the lines of a file for the context controls.

use crate::json::Json;
use axum::extract::{Query, State};
use pyn_core::{
    DIFF_DEFAULT_CONTEXT, Diff, DiffLineKind, DiffOptions, DiffStatus, FileDiff, FileLines,
    PynError, RepoPath,
};
use pyn_proto as api;
use serde::Deserialize;

use crate::caller::RepoReader;
use crate::{ApiResult, AppState, RepoAddress, file_mode_dto, lock_dto, mode_dto};

fn file_dto(f: FileDiff) -> api::DiffFile {
    api::DiffFile {
        path: f.path.to_string(),
        old_path: f.old_path.map(|p| p.to_string()),
        status: match f.status {
            DiffStatus::Added => api::DiffFileStatus::Added,
            DiffStatus::Modified => api::DiffFileStatus::Modified,
            DiffStatus::Deleted => api::DiffFileStatus::Deleted,
            DiffStatus::Renamed => api::DiffFileStatus::Renamed,
        },
        additions: f.additions,
        deletions: f.deletions,
        binary: f.binary,
        too_large: f.too_large,
        mode: mode_dto(f.mode),
        file_mode: file_mode_dto(f.file_mode),
        old_file_mode: f.old_file_mode.map(file_mode_dto),
        old_content: f.old_content.map(|c| c.to_hex()),
        new_content: f.new_content.map(|c| c.to_hex()),
        lock: f.lock.map(lock_dto),
        hunks: f
            .hunks
            .into_iter()
            .map(|h| api::DiffHunk {
                header: h.header,
                old_start: h.old_start,
                old_lines: h.old_lines,
                new_start: h.new_start,
                new_lines: h.new_lines,
                hidden_before: h.hidden_before,
                hidden_after: h.hidden_after,
                lines: h
                    .lines
                    .into_iter()
                    .map(|l| api::DiffLine {
                        kind: match l.kind {
                            DiffLineKind::Context => api::DiffLineKind::Context,
                            DiffLineKind::Add => api::DiffLineKind::Add,
                            DiffLineKind::Delete => api::DiffLineKind::Delete,
                        },
                        old_no: l.old_no,
                        new_no: l.new_no,
                        text: l.text,
                        no_newline: l.no_newline,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// The wire form of a diff, with lock holders' identities filled in.
pub(crate) async fn diff_dto(s: &AppState, d: Diff) -> ApiResult<api::RepoDiff> {
    let mut files: Vec<api::DiffFile> = d.files.into_iter().map(file_dto).collect();
    crate::self_service_api::annotate_held(s, files.iter_mut().filter_map(|f| f.lock.as_mut()))
        .await?;
    Ok(api::RepoDiff {
        base: d.base,
        base_commit: d.base_commit.to_hex(),
        head: d.head,
        head_commit: d.head_commit.to_hex(),
        from_commit: d.from_commit.map(|c| c.to_hex()),
        files,
        truncated: d.truncated,
    })
}

#[derive(Deserialize)]
pub(crate) struct DiffQuery {
    base: Option<String>,
    head: String,
    merge_base: Option<bool>,
    context: Option<usize>,
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/diff",
    params(RepoAddress,
           ("base" = Option<String>, Query, description = "a branch, or a commit id or abbreviation; the default branch if omitted"),
           ("head" = String, Query, description = "a branch, or a commit id or abbreviation"),
           ("merge_base" = Option<bool>, Query, description = "measure from the merge base of base and head (the default, as a pull request shows it); false diffs the two commits directly"),
           ("context" = Option<usize>, Query, description = "unchanged lines around each change, 0 to 100, default 3")),
    responses((status = 200, body = api::RepoDiff),
              (status = 400, body = api::ErrorBody, description = "invalid_request for a branch with no commits, a context over 100 or a history too long to walk, ambiguous_commit"),
              (status = 404, body = api::ErrorBody, description = "commit_not_found: neither a branch nor a commit")))]
/// The changes of `head` against `base` with line hunks, per path, with each path's mode and live lock.
pub(crate) async fn diff(
    State(s): State<AppState>,
    RepoReader { repo, .. }: RepoReader,
    Query(q): Query<DiffQuery>,
) -> ApiResult<Json<api::RepoDiff>> {
    let options = DiffOptions {
        merge_base: q.merge_base.unwrap_or(true),
        context: q.context.unwrap_or(DIFF_DEFAULT_CONTEXT),
    };
    let d = repo
        .service
        .diff(q.base.as_deref(), &q.head, options)
        .await?;
    Ok(Json(diff_dto(&s, d).await?))
}

#[derive(Deserialize)]
pub(crate) struct LinesQuery {
    path: String,
    rev: Option<String>,
    start: Option<u32>,
    end: Option<u32>,
}

fn lines_dto(l: FileLines) -> api::FileLines {
    api::FileLines {
        path: l.path.to_string(),
        commit: l.commit.to_hex(),
        revision: l.revision,
        total: l.total,
        start: l.start,
        lines: l.lines,
    }
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/lines",
    params(RepoAddress,
           ("path" = String, Query, description = "the file"),
           ("rev" = Option<String>, Query, description = "a branch, or a commit id or abbreviation; the default branch if omitted"),
           ("start" = Option<u32>, Query, description = "first line, counting from 1; default 1"),
           ("end" = Option<u32>, Query, description = "last line, inclusive; at most 2000 lines are returned")),
    responses((status = 200, body = api::FileLines),
              (status = 400, body = api::ErrorBody, description = "invalid_request for a binary file, one over 1 MiB, or a bad range"),
              (status = 404, body = api::ErrorBody, description = "commit_not_found, or revision_not_found when the path does not exist at that commit")))]
/// A range of the lines of a text file as of a commit, for expanding the context around a diff hunk.
pub(crate) async fn file_lines(
    RepoReader { repo, .. }: RepoReader,
    Query(q): Query<LinesQuery>,
) -> ApiResult<Json<api::FileLines>> {
    let path = RepoPath::new(q.path).map_err(|e: PynError| e)?;
    let rev = q.rev.as_deref().unwrap_or(pyn_core::branch::DEFAULT_BRANCH);
    let l = repo
        .service
        .file_lines(rev, &path, q.start.unwrap_or(1), q.end)
        .await?;
    Ok(Json(lines_dto(l)))
}

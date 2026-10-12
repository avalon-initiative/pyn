//! Merging a branch or commit into a branch: a preview, and the write.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pyn_core::merge::{
    Change, Conflict, ConflictKind, FastForward, Inputs, MergeOutcome, MergeReport, MergeRequest,
    Resolution, Side,
};
use pyn_core::{ObjectId, Permission, PynError, REPO_ALGORITHM, RepoPath};
use pyn_proto as api;

use crate::{
    ApiResult, AppState, RepoAddress, authorize_repo, file_mode_dto, file_mode_from, mode_dto,
};

fn object_id(hex: &str, what: &str) -> Result<ObjectId, PynError> {
    ObjectId::from_hex(REPO_ALGORITHM, hex)
        .map_err(|_| PynError::InvalidRequest(format!("{what} {hex:?} is not an object id")))
}

fn request_from(req: api::MergeRequest) -> Result<MergeRequest, PynError> {
    let resolutions = req
        .resolutions
        .into_iter()
        .map(|r| {
            Ok(Resolution {
                path: RepoPath::new(r.path)?,
                content: object_id(&r.content, "content")?,
                file_mode: file_mode_from(r.file_mode),
            })
        })
        .collect::<Result<_, PynError>>()?;
    Ok(MergeRequest {
        source: req.source,
        target: req.target,
        fast_forward: match req.fast_forward.unwrap_or_default() {
            api::MergeFastForward::Allow => FastForward::Allow,
            api::MergeFastForward::Never => FastForward::Never,
            api::MergeFastForward::Only => FastForward::Only,
        },
        message: req.message,
        expect_target: req
            .expect_target
            .as_deref()
            .map(|h| object_id(h, "expect_target"))
            .transpose()?,
        resolutions,
    })
}

fn side_dto(s: Side) -> api::MergeSide {
    api::MergeSide {
        revision: s.revision.map(|r| r.0),
        content: s.content.to_hex(),
        file_mode: file_mode_dto(s.file_mode),
    }
}

fn inputs_dto(i: Inputs) -> api::MergeInputs {
    api::MergeInputs {
        base: i.base.map(side_dto),
        ours: i.ours.map(side_dto),
        theirs: i.theirs.map(side_dto),
        base_inline: i.base_inline.map(|b| STANDARD.encode(b)),
    }
}

fn change_dto(c: Change) -> api::MergeChange {
    api::MergeChange {
        path: c.path.to_string(),
        mode: mode_dto(c.mode),
        content: c.content.to_hex(),
        file_mode: file_mode_dto(c.file_mode),
        revision: c.revision.map(|r| r.0),
        inputs: c.inputs.map(inputs_dto),
    }
}

fn conflict_dto(c: Conflict) -> api::MergeConflict {
    api::MergeConflict {
        path: c.path.to_string(),
        mode: mode_dto(c.mode),
        kind: match c.kind {
            ConflictKind::Content => api::MergeConflictKind::Content,
            ConflictKind::BothAdded => api::MergeConflictKind::BothAdded,
            ConflictKind::Binary => api::MergeConflictKind::Binary,
            ConflictKind::DeleteModify => api::MergeConflictKind::DeleteModify,
            ConflictKind::FileMode => api::MergeConflictKind::FileMode,
            ConflictKind::ExclusiveConversion => api::MergeConflictKind::ExclusiveConversion,
        },
        inputs: inputs_dto(c.inputs),
    }
}

fn report_dto(r: MergeReport) -> api::MergeReport {
    api::MergeReport {
        target: r.target,
        target_commit: r.target_head.map(|h| h.to_hex()),
        source: r.source.to_hex(),
        bases: r.bases.iter().map(ObjectId::to_hex).collect(),
        virtual_base: r.virtual_base,
        outcome: match r.outcome {
            MergeOutcome::UpToDate => api::MergeOutcome::UpToDate,
            MergeOutcome::FastForward => api::MergeOutcome::FastForward,
            MergeOutcome::Merge => api::MergeOutcome::Merge,
            MergeOutcome::Conflicted => api::MergeOutcome::Conflicted,
        },
        changes: r.changes.into_iter().map(change_dto).collect(),
        conflicts: r.conflicts.into_iter().map(conflict_dto).collect(),
        commit: r.commit.map(|c| c.to_hex()),
    }
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/merge/preview", params(RepoAddress),
    request_body = api::MergeRequest,
    responses((status = 200, body = api::MergeReport, description = "what the merge would do and what would conflict; nothing is written, and `message`, `expect_target` and `resolutions` are ignored"),
              (status = 404, body = api::ErrorBody, description = "branch_not_found, or commit_not_found for the source"),
              (status = 400, body = api::ErrorBody, description = "ambiguous_commit for the source, or invalid_request"),
              (status = 409, body = api::ErrorBody, description = "not_fast_forward when only a fast-forward was asked for")))]
/// Preview a merge.
pub(crate) async fn preview_merge(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::MergeRequest>,
) -> ApiResult<Json<api::MergeReport>> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Read)).await?;
    let report = repo.service.merge_preview(&request_from(req)?).await?;
    Ok(Json(report_dto(report)))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/merge", params(RepoAddress),
    request_body = api::MergeRequest,
    responses((status = 200, body = api::MergeReport, description = "the outcome and, unless the source was already in the target, the commit the target now points at"),
              (status = 400, body = api::ErrorBody, description = "invalid_request for a resolution the merge does not need, object_missing for resolved content that was not uploaded, ambiguous_commit for the source"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission, and the edit_policy permission when the merge brings a different policy file into the default branch"),
              (status = 404, body = api::ErrorBody, description = "branch_not_found, or commit_not_found for the source"),
              (status = 409, body = api::ErrorBody, description = "merge_conflict when a conflict has no resolution (nothing is written), merge_stale when the target moved, not_fast_forward, lock_required, lock_held or stale_base for a resolution on an exclusive path")))]
/// Merge a branch or commit into a branch.
pub(crate) async fn merge(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::MergeRequest>,
) -> ApiResult<Json<api::MergeReport>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    let report = repo.service.merge(&who, request_from(req)?).await?;
    Ok(Json(report_dto(report)))
}

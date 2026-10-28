//! Per-owner limits and usage. Limits are opt-in: with none set nothing is enforced.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use pyn_core::{AccountKind, LimitsChange, UserId};
use pyn_proto as api;

use crate::{
    ApiResult, AppState, RepoAddress, authorize_read, identify, identify_admin, open_visible,
};

fn limits_dto(l: pyn_core::Limits) -> api::Limits {
    api::Limits {
        repositories: l.repositories,
        members: l.members,
        storage_bytes: l.storage_bytes,
    }
}

fn kind_dto(kind: AccountKind) -> api::OwnerKind {
    match kind {
        AccountKind::User => api::OwnerKind::User,
        AccountKind::Org => api::OwnerKind::Org,
    }
}

fn owner_limits_dto(l: pyn_core::OwnerLimits) -> api::OwnerLimits {
    api::OwnerLimits {
        owner_id: l.id.to_string(),
        kind: kind_dto(l.kind),
        effective: limits_dto(l.effective),
        own: limits_dto(l.own),
    }
}

#[utoipa::path(get, path = "/v1/admin/limits", operation_id = "list_limits",
    responses((status = 200, body = api::LimitsListing, description = "the server default and every owner with limits of its own"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_limits")))]
pub(crate) async fn list_limits(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<api::LimitsListing>> {
    let who = identify_admin(&s, &headers).await?;
    let owners = s.access.list_owner_limits(&who).await?;
    Ok(Json(api::LimitsListing {
        defaults: limits_dto(s.access.default_limits()),
        owners: owners
            .into_iter()
            .map(|(owner, limits)| api::OwnerLimitsEntry {
                owner: owner.to_string(),
                limits: owner_limits_dto(limits),
            })
            .collect(),
    }))
}

#[utoipa::path(get, path = "/v1/owners/{owner}/limits", operation_id = "owner_limits",
    params(("owner" = String, Path, description = "a user or organization name")),
    responses((status = 200, body = api::OwnerLimits, description = "all limits null means unlimited"),
              (status = 403, body = api::ErrorBody, description = "not_namespace_owner (also for a name that does not exist) or not_org_owner (a member who does not own it): only the owner, an administrator or a service credential with manage_limits may look"),
              (status = 404, body = api::ErrorBody, description = "user_not_found, for an administrator or a service credential only")))]
pub(crate) async fn owner_limits(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(owner): Path<String>,
) -> ApiResult<Json<api::OwnerLimits>> {
    let who = identify_admin(&s, &headers).await?;
    let limits = s.access.owner_limits(&who, &UserId::new(owner)).await?;
    Ok(Json(owner_limits_dto(limits)))
}

#[utoipa::path(patch, path = "/v1/admin/owners/{owner}/limits", operation_id = "set_owner_limits",
    params(("owner" = String, Path, description = "a user or organization name")),
    request_body = api::SetLimitsRequest, responses(
    (status = 200, body = api::OwnerLimits, description = "lowering a limit below current use deletes nothing; it only stops growth"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: a value out of range, or a member limit on a user"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_limits"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn set_owner_limits(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(owner): Path<String>,
    Json(req): Json<api::SetLimitsRequest>,
) -> ApiResult<Json<api::OwnerLimits>> {
    let who = identify_admin(&s, &headers).await?;
    let change = LimitsChange {
        repositories: req.repositories,
        members: req.members,
        storage_bytes: req.storage_bytes,
    };
    let limits = s
        .access
        .set_owner_limits(&who, &UserId::new(owner), change)
        .await?;
    Ok(Json(owner_limits_dto(limits)))
}

#[utoipa::path(get, path = "/v1/owners/{owner}/usage", operation_id = "owner_usage",
    params(("owner" = String, Path, description = "a user or organization name")),
    responses((status = 200, body = api::OwnerUsage, description = "usage next to the limits in force, with every repository the owner has"),
              (status = 403, body = api::ErrorBody, description = "not_namespace_owner (also for a name that does not exist) or not_org_owner (a member who does not own it): only the owner, an administrator or a service credential with manage_limits may look"),
              (status = 404, body = api::ErrorBody, description = "user_not_found, for an administrator or a service credential only")))]
pub(crate) async fn owner_usage(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(owner): Path<String>,
) -> ApiResult<Json<api::OwnerUsage>> {
    let who = identify_admin(&s, &headers).await?;
    let report = s
        .repos
        .owner_report(&who, &UserId::new(owner.clone()))
        .await?;
    Ok(Json(api::OwnerUsage {
        owner,
        owner_id: report.limits.id.to_string(),
        limits: owner_limits_dto(report.limits),
        usage: api::Usage {
            repositories: report.usage.repositories,
            members: report.usage.members,
            stored_bytes: report.usage.stored_bytes,
        },
        repositories: report
            .repositories
            .into_iter()
            .map(|(record, used)| repo_usage_dto(&record, used))
            .collect(),
    }))
}

fn repo_usage_dto(record: &pyn_core::RepoRecord, used: pyn_core::RepoUsage) -> api::RepoUsage {
    api::RepoUsage {
        repository: record.address(),
        repo_id: record.id.to_string(),
        stored_bytes: used.stored_bytes,
        files: used.files,
        revisions: used.revisions,
        storage_cap: record.storage_cap,
    }
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/usage", operation_id = "repo_usage", params(RepoAddress),
    responses((status = 200, body = api::RepoUsage, description = "readable by anyone who can read the repository")))]
pub(crate) async fn repo_usage(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<api::RepoUsage>> {
    let open = authorize_read(&s, &headers, &owner, &name).await?;
    let used = s.repos.repo_usage(&open.record).await?;
    Ok(Json(repo_usage_dto(&open.record, used)))
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/usage/breakdown", operation_id = "repo_usage_breakdown", params(RepoAddress),
    responses((status = 200, body = api::RepoUsageBreakdown, description = "distinct stored bytes by top-level folder and the largest paths"),
              (status = 403, body = api::ErrorBody, description = "forbidden: needs the manage_roles permission (repository admin)"),
              (status = 404, body = api::ErrorBody, description = "repo_not_found")))]
pub(crate) async fn repo_usage_breakdown(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<api::RepoUsageBreakdown>> {
    let who = identify(&s, &headers).await?;
    let (open, _) = open_visible(&s, Some(&who), &owner, &name).await?;
    let used = s.repos.repo_usage(&open.record).await?;
    let breakdown = s.repos.repo_breakdown(&who, &open.record).await?;
    Ok(Json(api::RepoUsageBreakdown {
        repository: open.record.address(),
        repo_id: open.record.id.to_string(),
        stored_bytes: used.stored_bytes,
        storage_cap: open.record.storage_cap,
        folders: breakdown
            .folders
            .into_iter()
            .map(|f| api::FolderUsage {
                folder: f.folder,
                stored_bytes: f.stored_bytes,
                files: f.files,
            })
            .collect(),
        largest_paths: breakdown
            .largest_paths
            .into_iter()
            .map(|p| api::PathUsage {
                path: p.path,
                stored_bytes: p.stored_bytes,
                revisions: p.revisions,
            })
            .collect(),
    }))
}

#[utoipa::path(put, path = "/v1/repos/{owner}/{name}/storage-cap", operation_id = "set_repo_storage_cap", params(RepoAddress),
    request_body = api::SetRepoStorageCapRequest, responses(
    (status = 200, body = api::RepoStorageCap, description = "a cap below current use deletes nothing; writes that add stored bytes are refused with repo_storage_limit_reached until use falls under it"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: a value out of range"),
    (status = 403, body = api::ErrorBody, description = "not_namespace_owner or not_org_owner: only the owner, a server administrator or a service credential with manage_limits; service_scope_required for a service credential without manage_limits; owner_frozen for the owner's own request while it is frozen"),
    (status = 404, body = api::ErrorBody, description = "repo_not_found"),
))]
pub(crate) async fn set_repo_storage_cap(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::SetRepoStorageCapRequest>,
) -> ApiResult<Json<api::RepoStorageCap>> {
    let who = identify_admin(&s, &headers).await?;
    let record = s
        .repos
        .set_storage_cap(&who, &UserId::new(owner), &name, req.storage_bytes)
        .await?;
    Ok(Json(api::RepoStorageCap {
        repository: record.address(),
        repo_id: record.id.to_string(),
        storage_bytes: record.storage_cap,
    }))
}

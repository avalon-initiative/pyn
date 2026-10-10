//! Per-owner limits and usage. Limits are opt-in: with none set nothing is enforced.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use pyn_core::{AccountKind, LimitsChange, UserId};
use pyn_proto as api;

use crate::{ApiResult, AppState, RepoAddress, authorize_read, identify_admin};

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
              (status = 403, body = api::ErrorBody, description = "not_namespace_owner or not_org_owner: only the owner, an administrator or a service credential with manage_limits may look"),
              (status = 404, body = api::ErrorBody, description = "user_not_found")))]
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
              (status = 403, body = api::ErrorBody, description = "not_namespace_owner or not_org_owner: only the owner, an administrator or a service credential with manage_limits may look"),
              (status = 404, body = api::ErrorBody, description = "user_not_found")))]
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
        limits: owner_limits_dto(report.limits),
        usage: api::Usage {
            repositories: report.usage.repositories,
            members: report.usage.members,
            stored_bytes: report.usage.stored_bytes,
        },
        repositories: report
            .repositories
            .into_iter()
            .map(|(record, used)| repo_usage_dto(record.address(), used))
            .collect(),
    }))
}

fn repo_usage_dto(repository: String, used: pyn_core::RepoUsage) -> api::RepoUsage {
    api::RepoUsage {
        repository,
        stored_bytes: used.stored_bytes,
        files: used.files,
        revisions: used.revisions,
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
    Ok(Json(repo_usage_dto(open.record.address(), used)))
}

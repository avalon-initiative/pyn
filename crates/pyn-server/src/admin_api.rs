//! Server administration: accounts, organizations, service credentials and the server audit log.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{
    AccountRecord, AccountStatus, AuditQuery, OrgRole, ServiceCredentialRecord, ServiceScope,
    UserId,
};
use pyn_proto as api;
use serde::Deserialize;

use crate::{ApiResult, AppState, identify_admin};

fn info(a: AccountRecord) -> api::AccountInfo {
    api::AccountInfo {
        status: a.status().to_string(),
        user: a.user.to_string(),
        email: a.email,
        email_verified: a.email_verified_at.is_some(),
        disabled_at: a.disabled_at,
        disabled_reason: a.disabled_reason,
        admin: a.is_admin,
        created_at: a.created_at,
    }
}

#[derive(Deserialize)]
pub(crate) struct AccountsQuery {
    status: Option<String>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/admin/users",
    params(("status" = Option<String>, Query, description = "only accounts in this state: pending_verification, pending_approval, active or disabled"),
           ("limit" = Option<usize>, Query, description = "default 100, max 500")),
    responses((status = 200, body = Vec<api::AccountInfo>, description = "oldest first"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_accounts")))]
pub(crate) async fn list_accounts(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AccountsQuery>,
) -> ApiResult<Json<Vec<api::AccountInfo>>> {
    let who = identify_admin(&s, &headers).await?;
    let status = q
        .status
        .as_deref()
        .map(AccountStatus::from_str)
        .transpose()?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let accounts = s.access.list_accounts(&who, status, limit).await?;
    Ok(Json(accounts.into_iter().map(info).collect()))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/approve",
    params(("user" = String, Path, description = "the account's user name")),
    responses(
    (status = 200, body = api::AccountInfo),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is not waiting for approval"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_accounts"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn approve(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify_admin(&s, &headers).await?;
    let account = s.access.approve_account(&who, &UserId::new(user)).await?;
    Ok(Json(info(account)))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/disable",
    params(("user" = String, Path, description = "the account's user name")),
    request_body = api::DisableAccountRequest, responses(
    (status = 200, body = api::AccountInfo, description = "sessions end at once; tokens and keys stop working"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: one's own account"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_accounts"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn disable(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
    Json(req): Json<api::DisableAccountRequest>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify_admin(&s, &headers).await?;
    let account = s
        .access
        .disable_account(&who, &UserId::new(user), req.reason.as_deref())
        .await?;
    Ok(Json(info(account)))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/enable",
    params(("user" = String, Path, description = "the account's user name")),
    responses(
    (status = 200, body = api::AccountInfo),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is not disabled"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_accounts"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn enable(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify_admin(&s, &headers).await?;
    let account = s.access.enable_account(&who, &UserId::new(user)).await?;
    Ok(Json(info(account)))
}

#[utoipa::path(put, path = "/v1/admin/users/{user}/admin",
    params(("user" = String, Path, description = "the account's user name")),
    responses(
    (status = 200, body = api::AccountInfo, description = "the account is now an administrator; granting again changes nothing"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is not active"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required: also for any service credential"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn grant_admin(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify_admin(&s, &headers).await?;
    let account = s.access.grant_admin(&who, &UserId::new(user)).await?;
    Ok(Json(info(account)))
}

#[utoipa::path(delete, path = "/v1/admin/users/{user}/admin",
    params(("user" = String, Path, description = "the account's user name")),
    responses(
    (status = 200, body = api::AccountInfo, description = "the account is no longer an administrator; revoking from a non-administrator changes nothing"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required: also for any service credential"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
    (status = 409, body = api::ErrorBody, description = "last_server_admin: no other active administrator would remain"),
))]
pub(crate) async fn revoke_admin(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify_admin(&s, &headers).await?;
    let account = s.access.revoke_admin(&who, &UserId::new(user)).await?;
    Ok(Json(info(account)))
}

#[derive(Deserialize)]
pub(crate) struct AuditParams {
    before: Option<i64>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/admin/audit",
    params(("before" = Option<i64>, Query, description = "events older than this id"),
           ("limit" = Option<usize>, Query, description = "page size, default 50, max 500")),
    responses((status = 200, body = api::AuditPage, description = "account, administrator and setup events"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required: service credentials cannot read the log")))]
pub(crate) async fn audit(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditParams>,
) -> ApiResult<Json<api::AuditPage>> {
    let who = identify_admin(&s, &headers).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let query = AuditQuery {
        before: q.before,
        limit: limit + 1,
        ..Default::default()
    };
    let mut events = s.access.server_audit(&who, &query).await?;
    let next_before = (events.len() > limit).then(|| {
        events.truncate(limit);
        events[limit - 1].id
    });
    let entries = events
        .into_iter()
        .map(|e| api::AuditEntry {
            id: e.id,
            at: e.at,
            actor: e.actor.to_string(),
            action: e.action.to_string(),
            path: None,
            detail: e.detail,
        })
        .collect();
    Ok(Json(api::AuditPage {
        entries,
        next_before,
    }))
}

#[utoipa::path(post, path = "/v1/admin/orgs", request_body = api::AdminCreateOrgRequest, responses(
    (status = 201, body = api::OrgInfo, description = "the named user becomes its first owner"),
    (status = 400, body = api::ErrorBody, description = "invalid_request (bad name) or reserved_name"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_organizations"),
    (status = 404, body = api::ErrorBody, description = "user_not_found: the owner"),
    (status = 409, body = api::ErrorBody, description = "user_exists: the name is taken by a user or organization"),
))]
pub(crate) async fn create_org(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::AdminCreateOrgRequest>,
) -> ApiResult<(StatusCode, Json<api::OrgInfo>)> {
    let who = identify_admin(&s, &headers).await?;
    let org = s
        .access
        .admin_create_org(&who, &req.name, &UserId::new(req.owner))
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::OrgInfo {
            name: org.user.to_string(),
            created_at: org.created_at,
            role: Some(OrgRole::Owner.to_string()),
        }),
    ))
}

#[utoipa::path(delete, path = "/v1/admin/orgs/{org}",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 204, description = "its audit log is kept"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_organizations"),
              (status = 404, body = api::ErrorBody, description = "org_not_found"),
              (status = 409, body = api::ErrorBody, description = "org_not_empty or org_deleting")))]
pub(crate) async fn delete_org(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify_admin(&s, &headers).await?;
    s.access.admin_delete_org(&who, &UserId::new(name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn credential_info(c: ServiceCredentialRecord) -> api::ServiceCredentialInfo {
    api::ServiceCredentialInfo {
        name: c.name,
        scopes: c.scopes.iter().map(|s| s.to_string()).collect(),
        created_by: c.created_by.to_string(),
        created_at: c.created_at,
        revoked_at: c.revoked_at,
        last_used_at: c.last_used_at,
    }
}

#[utoipa::path(post, path = "/v1/admin/service-credentials", request_body = api::CreateServiceCredentialRequest, responses(
    (status = 201, body = api::CreatedServiceCredential, description = "the secret is shown once"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: bad name, no scope or an unknown scope"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required: a service credential cannot create credentials"),
    (status = 409, body = api::ErrorBody, description = "service_credential_exists: names are never reused"),
))]
pub(crate) async fn create_service_credential(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CreateServiceCredentialRequest>,
) -> ApiResult<(StatusCode, Json<api::CreatedServiceCredential>)> {
    let who = identify_admin(&s, &headers).await?;
    let scopes = req
        .scopes
        .iter()
        .map(|n| n.parse::<ServiceScope>())
        .collect::<Result<_, _>>()?;
    let (record, secret) = s
        .access
        .create_service_credential(&who, &req.name, scopes)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::CreatedServiceCredential {
            secret,
            info: credential_info(record),
        }),
    ))
}

#[utoipa::path(get, path = "/v1/admin/service-credentials",
    responses((status = 200, body = Vec<api::ServiceCredentialInfo>, description = "oldest first, revoked ones included"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required")))]
pub(crate) async fn list_service_credentials(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::ServiceCredentialInfo>>> {
    let who = identify_admin(&s, &headers).await?;
    let all = s.access.list_service_credentials(&who).await?;
    Ok(Json(all.into_iter().map(credential_info).collect()))
}

#[utoipa::path(delete, path = "/v1/admin/service-credentials/{name}",
    params(("name" = String, Path, description = "the credential's name")),
    responses((status = 204, description = "revoked at once; revoking again is harmless"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required"),
              (status = 404, body = api::ErrorBody, description = "service_credential_not_found")))]
pub(crate) async fn revoke_service_credential(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify_admin(&s, &headers).await?;
    s.access.revoke_service_credential(&who, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

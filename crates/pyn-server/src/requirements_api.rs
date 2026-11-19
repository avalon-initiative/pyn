//! Required second factors (#1106): the server's and an organization's requirement, the administrator's reset and
//! the account's notices.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{NoticeRecord, RequiredBy, Requirement, TwoFactorPolicy, UserId};
use pyn_proto as api;

use crate::caller::{AdminCaller, FullScopeCaller, OrgCaller};
use crate::two_factor_api::require_fresh;
use crate::{ApiResult, AppState, session_api};

pub(crate) fn required_dto(r: &Requirement, enforced: bool) -> api::TwoFactorRequired {
    let (by, organization) = match &r.by {
        RequiredBy::Server => ("server", None),
        RequiredBy::Org(org) => ("organization", Some(org.to_string())),
    };
    api::TwoFactorRequired {
        by: by.to_string(),
        organization,
        deadline: r.deadline,
        enforced,
    }
}

fn setting(policy: Option<TwoFactorPolicy>) -> api::TwoFactorSetting {
    api::TwoFactorSetting {
        required: policy.is_some(),
        since: policy.map(|p| p.since),
        grace_days: policy.map(|p| p.grace_days),
    }
}

fn notice_dto(n: NoticeRecord) -> api::Notice {
    api::Notice {
        id: n.id,
        kind: n.kind,
        detail: n.detail,
        created_at: n.created_at,
    }
}

#[utoipa::path(get, path = "/v1/admin/two-factor", responses(
    (status = 200, body = api::TwoFactorSetting, description = "whether every account must hold a second factor"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required: also for any service credential"),
))]
pub(crate) async fn get_server_requirement(
    State(s): State<AppState>,
    AdminCaller(_): AdminCaller,
) -> Json<api::TwoFactorSetting> {
    Json(setting(s.access.server_two_factor()))
}

#[utoipa::path(put, path = "/v1/admin/two-factor", request_body = api::SetTwoFactorRequest, responses(
    (status = 200, body = api::TwoFactorSetting, description = "the requirement now in force; the same one again changes nothing"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: a grace period over 90 days"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or fresh_check_required"),
))]
pub(crate) async fn set_server_requirement(
    State(s): State<AppState>,
    headers: HeaderMap,
    AdminCaller(who): AdminCaller,
    Json(req): Json<api::SetTwoFactorRequest>,
) -> ApiResult<Json<api::TwoFactorSetting>> {
    require_fresh(&s, &who, &headers).await?;
    let policy = s
        .access
        .set_server_two_factor(&who, req.required, req.grace_days)
        .await?;
    Ok(Json(setting(policy)))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/two-factor/reset",
    params(("user" = String, Path, description = "the account's user name")), responses(
    (status = 204, description = "every second factor and recovery code removed, the reset audited and a `two_factor_reset` notice left for the account"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is an administrator, whose factors are reset on the host"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or fresh_check_required"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn reset_two_factor(
    State(s): State<AppState>,
    headers: HeaderMap,
    AdminCaller(who): AdminCaller,
    Path(user): Path<String>,
) -> ApiResult<StatusCode> {
    s.access
        .reset_two_factor(
            &who,
            session_api::session_cookie(&headers),
            &UserId::new(user),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/orgs/{org}/two-factor",
    params(("org" = String, Path, description = "the organization's name")), responses(
    (status = 200, body = api::TwoFactorSetting),
    (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without the organization grant"),
    (status = 404, body = api::ErrorBody, description = "org_not_found"),
))]
pub(crate) async fn get_org_requirement(
    State(s): State<AppState>,
    OrgCaller(who): OrgCaller,
    Path(org): Path<String>,
) -> ApiResult<Json<api::TwoFactorSetting>> {
    let policy = s
        .access
        .org_two_factor_policy(&who, &UserId::new(org))
        .await?;
    Ok(Json(setting(policy)))
}

#[utoipa::path(put, path = "/v1/orgs/{org}/two-factor",
    params(("org" = String, Path, description = "the organization's name")),
    request_body = api::SetTwoFactorRequest, responses(
    (status = 200, body = api::TwoFactorSetting, description = "the requirement now in force; the same one again changes nothing"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: a grace period over 90 days"),
    (status = 403, body = api::ErrorBody, description = "not_org_owner, a token without the organization grant, or fresh_check_required"),
    (status = 404, body = api::ErrorBody, description = "org_not_found"),
))]
pub(crate) async fn set_org_requirement(
    State(s): State<AppState>,
    headers: HeaderMap,
    OrgCaller(who): OrgCaller,
    Path(org): Path<String>,
    Json(req): Json<api::SetTwoFactorRequest>,
) -> ApiResult<Json<api::TwoFactorSetting>> {
    require_fresh(&s, &who, &headers).await?;
    let policy = s
        .access
        .set_org_two_factor(&who, &UserId::new(org), req.required, req.grace_days)
        .await?;
    Ok(Json(setting(policy)))
}

#[utoipa::path(get, path = "/v1/me/notices", responses(
    (status = 200, body = Vec<api::Notice>, description = "the account's notices, oldest first; also answers while a second factor is required and missing"),
))]
pub(crate) async fn notices(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<Vec<api::Notice>>> {
    let list = s.access.notices(&who).await?;
    Ok(Json(list.into_iter().map(notice_dto).collect()))
}

#[utoipa::path(delete, path = "/v1/me/notices/{id}",
    params(("id" = String, Path, description = "the notice")), responses(
    (status = 204, description = "dismissed"),
    (status = 404, body = api::ErrorBody, description = "notice_not_found"),
))]
pub(crate) async fn dismiss_notice(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    s.access.dismiss_notice(&who, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

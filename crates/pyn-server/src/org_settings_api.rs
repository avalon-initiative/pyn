//! An organization's repository defaults.

use axum::Json;
use axum::extract::{Path, State};
use pyn_core::{OrgSettings, UserId};
use pyn_proto as api;

use crate::caller::OrgCaller;
use crate::{ApiResult, AppState};

fn info(settings: OrgSettings) -> api::OrgSettingsInfo {
    api::OrgSettingsInfo {
        exclusive_read_only: settings.exclusive_read_only,
    }
}

#[utoipa::path(get, path = "/v1/orgs/{org}/settings",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = api::OrgSettingsInfo),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without the organization grant"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn get_org_settings(
    State(s): State<AppState>,
    OrgCaller(who): OrgCaller,
    Path(org): Path<String>,
) -> ApiResult<Json<api::OrgSettingsInfo>> {
    Ok(Json(info(
        s.access.org_settings(&who, &UserId::new(org)).await?,
    )))
}

#[utoipa::path(put, path = "/v1/orgs/{org}/settings",
    params(("org" = String, Path, description = "the organization's name")),
    request_body = api::SetOrgSettingsRequest,
    responses((status = 200, body = api::OrgSettingsInfo, description = "the settings after the change"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without the organization grant"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn set_org_settings(
    State(s): State<AppState>,
    OrgCaller(who): OrgCaller,
    Path(org): Path<String>,
    Json(req): Json<api::SetOrgSettingsRequest>,
) -> ApiResult<Json<api::OrgSettingsInfo>> {
    let settings = OrgSettings {
        exclusive_read_only: req.exclusive_read_only,
    };
    Ok(Json(info(
        s.access
            .set_org_settings(&who, &UserId::new(org), settings)
            .await?,
    )))
}

//! Owner freeze: every write under a frozen user or organization is refused, reads continue.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use pyn_core::{AccountKind, UserId};
use pyn_proto as api;

use crate::{ApiResult, AppState, identify_admin};

fn freeze_dto(owner: String, state: pyn_core::OwnerFreezeState) -> api::OwnerFreeze {
    api::OwnerFreeze {
        owner,
        owner_id: state.id.to_string(),
        kind: match state.kind {
            AccountKind::User => api::OwnerKind::User,
            AccountKind::Org => api::OwnerKind::Org,
        },
        frozen: state.frozen_at.is_some(),
        frozen_at: state.frozen_at,
        reason: state.reason,
    }
}

#[utoipa::path(get, path = "/v1/owners/{owner}/freeze", operation_id = "owner_freeze",
    params(("owner" = String, Path, description = "a user or organization name")),
    responses((status = 200, body = api::OwnerFreeze, description = "frozen is false, with a null time and reason, while the owner is not frozen"),
              (status = 403, body = api::ErrorBody, description = "not_namespace_owner (also for a name that does not exist): only the user themself, a member of the organization, an administrator or a service credential with manage_freeze may look; service_scope_required for a service credential without it"),
              (status = 404, body = api::ErrorBody, description = "user_not_found, for an administrator or a service credential only")))]
pub(crate) async fn owner_freeze(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(owner): Path<String>,
) -> ApiResult<Json<api::OwnerFreeze>> {
    let who = identify_admin(&s, &headers).await?;
    let state = s.access.owner_freeze(&who, &UserId::new(&owner)).await?;
    Ok(Json(freeze_dto(owner, state)))
}

#[utoipa::path(put, path = "/v1/admin/owners/{owner}/freeze", operation_id = "freeze_owner",
    params(("owner" = String, Path, description = "a user or organization name")),
    request_body = api::FreezeRequest, responses(
    (status = 200, body = api::OwnerFreeze, description = "every write under the owner is now refused with owner_frozen; freezing a frozen owner replaces the reason and time"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the reason is empty or over 512 bytes"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_freeze"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn freeze_owner(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(owner): Path<String>,
    Json(req): Json<api::FreezeRequest>,
) -> ApiResult<Json<api::OwnerFreeze>> {
    let who = identify_admin(&s, &headers).await?;
    let state = s
        .access
        .freeze_owner(&who, &UserId::new(&owner), &req.reason)
        .await?;
    Ok(Json(freeze_dto(owner, state)))
}

#[utoipa::path(delete, path = "/v1/admin/owners/{owner}/freeze", operation_id = "unfreeze_owner",
    params(("owner" = String, Path, description = "a user or organization name")),
    responses(
    (status = 200, body = api::OwnerFreeze, description = "writes work again; unfreezing an owner that is not frozen changes nothing"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_freeze"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn unfreeze_owner(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(owner): Path<String>,
) -> ApiResult<Json<api::OwnerFreeze>> {
    let who = identify_admin(&s, &headers).await?;
    let state = s.access.unfreeze_owner(&who, &UserId::new(&owner)).await?;
    Ok(Json(freeze_dto(owner, state)))
}

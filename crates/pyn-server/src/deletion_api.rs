//! Account deletion routes: the signed-in user's own request, restore and status, and an administrator's deletion.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use pyn_core::{AccountDeletion, Credential, Identity, PynError, UserId};
use pyn_proto as api;

use crate::{ApiResult, AppState, bearer_token, identify_admin, session_api};

fn status(user: &UserId, deletion: Option<AccountDeletion>) -> api::AccountDeletionStatus {
    api::AccountDeletionStatus {
        user: user.to_string(),
        pending: deletion.is_some(),
        scheduled_at: deletion.as_ref().map(|d| d.scheduled_at),
        purge_at: deletion.map(|d| d.purge_at),
    }
}

/// Like `identify`, but an account its owner scheduled for deletion is accepted: these routes are how it is restored.
async fn identify_pending_ok(s: &AppState, headers: &HeaderMap) -> ApiResult<Identity> {
    let who = match (bearer_token(headers), session_api::session_cookie(headers)) {
        (Some(token), _) => s.access.identify_allowing_pending(token).await?,
        (None, Some(cookie)) => {
            s.access
                .authenticate_session_allowing_pending(cookie)
                .await?
                .0
        }
        (None, None) => identify_admin(s, headers).await?,
    };
    if matches!(who.credential, Credential::Service(_)) {
        return Err(PynError::ServiceCredentialNotAllowed.into());
    }
    Ok(who)
}

#[utoipa::path(get, path = "/v1/account/deletion", responses(
    (status = 200, body = api::AccountDeletionStatus, description = "works while the account is scheduled for deletion"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated"),
))]
pub(crate) async fn account_deletion_status(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<api::AccountDeletionStatus>> {
    let who = identify_pending_ok(&s, &headers).await?;
    let deletion = s.access.deletion_status(&who).await?;
    Ok(Json(status(&who.user, deletion)))
}

#[utoipa::path(post, path = "/v1/account/deletion", request_body = api::DeleteOwnAccountRequest, responses(
    (status = 200, body = api::AccountDeletionStatus, description = "the account is disabled and will be deleted at purge_at unless restored; other credentials of the account now answer account_pending_deletion"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is already scheduled"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated: the password is wrong"),
    (status = 409, body = api::ErrorBody, description = "account_owns_resources (the message lists the repositories and organizations to transfer or delete), account_frozen or last_server_admin"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn schedule_account_deletion(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::DeleteOwnAccountRequest>,
) -> ApiResult<Json<api::AccountDeletionStatus>> {
    let who = crate::identify(&s, &headers).await?;
    let deletion = s.access.schedule_own_deletion(&who, &req.password).await?;
    Ok(Json(status(&who.user, Some(deletion))))
}

#[utoipa::path(post, path = "/v1/account/deletion/restore", responses(
    (status = 200, body = api::AccountDeletionStatus, description = "the account is enabled again"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: nothing is scheduled, or the recovery window has ended"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated"),
))]
pub(crate) async fn restore_account_deletion(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<api::AccountDeletionStatus>> {
    let who = identify_pending_ok(&s, &headers).await?;
    s.access.restore_account(&who).await?;
    Ok(Json(status(&who.user, None)))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/delete",
    params(("user" = String, Path, description = "the account's user name")),
    request_body = api::AdminDeleteAccountRequest, responses(
    (status = 200, body = api::AdminDeleteAccountResult),
    (status = 400, body = api::ErrorBody, description = "invalid_request: no reason, or one's own account"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_accounts"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
    (status = 409, body = api::ErrorBody, description = "account_not_disabled, account_owns_resources (the message lists the repositories and organizations), account_frozen or last_server_admin"),
))]
pub(crate) async fn admin_delete_account(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
    Json(req): Json<api::AdminDeleteAccountRequest>,
) -> ApiResult<Json<api::AdminDeleteAccountResult>> {
    let who = identify_admin(&s, &headers).await?;
    let user = UserId::new(user);
    let scheduled = s
        .access
        .admin_delete_account(&who, &user, &req.reason, req.immediately)
        .await?;
    Ok(Json(api::AdminDeleteAccountResult {
        user: user.to_string(),
        deleted: scheduled.is_none(),
        purge_at: scheduled.map(|d| d.purge_at),
    }))
}

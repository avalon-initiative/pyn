//! Account deletion routes: the signed-in user's own request, restore and status, and an administrator's deletion.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use pyn_core::{AccountDeletion, MailerKind, UserId};
use pyn_proto as api;

use crate::caller::{FullScopeCaller, ServiceCaller};
use crate::two_factor_api::require_fresh;
use crate::{ApiResult, AppState};

fn status(
    user: &UserId,
    deletion: Option<AccountDeletion>,
    window_days: i64,
) -> api::AccountDeletionStatus {
    api::AccountDeletionStatus {
        user: user.to_string(),
        pending: deletion.is_some(),
        scheduled_at: deletion.as_ref().map(|d| d.scheduled_at),
        purge_at: deletion.map(|d| d.purge_at),
        window_days,
    }
}

#[utoipa::path(get, path = "/v1/account/deletion", responses(
    (status = 200, body = api::AccountDeletionStatus, description = "works while the account is scheduled for deletion"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated"),
    (status = 403, body = api::ErrorBody, description = "a token that is not full-scope, or a service credential"),
))]
pub(crate) async fn account_deletion_status(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<api::AccountDeletionStatus>> {
    let deletion = s.access.deletion_status(&who).await?;
    Ok(Json(status(
        &who.user,
        deletion,
        s.access.deletion_window_days(),
    )))
}

#[utoipa::path(post, path = "/v1/account/deletion", request_body = api::DeleteOwnAccountRequest, responses(
    (status = 200, body = api::AccountDeletionStatus, description = "the account is disabled and will be deleted at purge_at unless restored; other credentials of the account now answer account_pending_deletion"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is already scheduled"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated: the password is wrong"),
    (status = 403, body = api::ErrorBody, description = "fresh_check_required: the account has a second factor"),
    (status = 409, body = api::ErrorBody, description = "account_owns_resources (the message lists the repositories and organizations to transfer or delete), account_frozen or last_server_admin"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn schedule_account_deletion(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Json(req): Json<api::DeleteOwnAccountRequest>,
) -> ApiResult<Json<api::AccountDeletionStatus>> {
    require_fresh(&s, &who, &headers).await?;
    let deletion = s.access.schedule_own_deletion(&who, &req.password).await?;
    Ok(Json(status(
        &who.user,
        Some(deletion),
        s.access.deletion_window_days(),
    )))
}

#[utoipa::path(post, path = "/v1/account/deletion/restore", responses(
    (status = 200, body = api::AccountDeletionStatus, description = "the account is enabled again"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: nothing is scheduled, or the recovery window has ended"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated"),
))]
pub(crate) async fn restore_account_deletion(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<api::AccountDeletionStatus>> {
    s.access.restore_account(&who).await?;
    Ok(Json(status(
        &who.user,
        None,
        s.access.deletion_window_days(),
    )))
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
    ServiceCaller(who): ServiceCaller,
    Path(user): Path<String>,
    Json(req): Json<api::AdminDeleteAccountRequest>,
) -> ApiResult<Json<api::AdminDeleteAccountResult>> {
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

#[utoipa::path(get, path = "/v1/admin/settings", responses(
    (status = 200, body = api::AdminSettings, description = "the effective server settings an administrator can read"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_accounts"),
))]
pub(crate) async fn admin_settings(
    State(s): State<AppState>,
    ServiceCaller(who): ServiceCaller,
) -> ApiResult<Json<api::AdminSettings>> {
    let (window, retention) = s.access.account_deletion_settings(&who).await?;
    let active = s.access.active_admin_count(&who).await?;
    Ok(Json(api::AdminSettings {
        administrators: api::AdministratorSummary {
            active: u32::try_from(active).unwrap_or(u32::MAX),
            single: active == 1,
        },
        account_deletion: api::AccountDeletionSettings {
            window_days: u32::try_from(window).unwrap_or(0),
            disabled_retention_days: u32::try_from(retention).unwrap_or(0),
        },
        storage: s.storage.settings(),
        certificate: s.certificate.settings(),
        email: {
            let (mailer, verification) = s.access.email_status();
            api::EmailSettings {
                mailer: match mailer {
                    MailerKind::None => api::MailerKind::None,
                    MailerKind::Log => api::MailerKind::Log,
                    MailerKind::Smtp => api::MailerKind::Smtp,
                },
                verification,
            }
        },
    }))
}

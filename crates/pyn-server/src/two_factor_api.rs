//! Second factors (#1104): enrolment, recovery codes and the fresh check behind sensitive actions.

use crate::json::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{FactorRecord, Identity};
use pyn_proto as api;

use crate::caller::FullScopeCaller;
use crate::{ApiResult, AppState, session_api};

/// Refuses unless the credential passed a second factor recently, when the account has one.
pub(crate) async fn require_fresh(
    s: &AppState,
    who: &Identity,
    headers: &HeaderMap,
) -> ApiResult<()> {
    Ok(s.access
        .require_fresh(who, session_api::session_cookie(headers))
        .await?)
}

fn factor_dto(f: &FactorRecord) -> api::TwoFactorFactor {
    api::TwoFactorFactor {
        id: f.id.clone(),
        kind: f.kind.as_str().to_string(),
        label: f.label.clone(),
        created_at: f.confirmed_at.unwrap_or(f.created_at),
    }
}

#[utoipa::path(get, path = "/v1/me/two-factor", responses(
    (status = 200, body = api::TwoFactorStatus, description = "the confirmed factors and the recovery codes left"),
))]
pub(crate) async fn two_factor_status(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<api::TwoFactorStatus>> {
    let status = s.access.two_factor_status(&who).await?;
    Ok(Json(api::TwoFactorStatus {
        enabled: !status.factors.is_empty() || !status.passkeys.is_empty(),
        factors: status.factors.iter().map(factor_dto).collect(),
        passkeys: status
            .passkeys
            .iter()
            .map(crate::passkeys_api::passkey_dto)
            .collect(),
        passkeys_available: s.access.passkeys_available(),
        recovery_codes_left: status.recovery_codes_left,
        required: s
            .access
            .unmet_two_factor_requirement(&who.user)
            .await?
            .map(|r| crate::requirements_api::required_dto(&r, s.access.two_factor_enforced(&r))),
    }))
}

#[utoipa::path(post, path = "/v1/me/two-factor/totp", request_body = api::BeginTotpRequest, responses(
    (status = 201, body = api::TotpEnrolment, description = "a pending authenticator-app factor; it counts after `confirm`, and starting again replaces it"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated: the password is wrong or missing"),
    (status = 403, body = api::ErrorBody, description = "fresh_check_required: the account already has a factor"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
))]
pub(crate) async fn begin_totp(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Json(req): Json<api::BeginTotpRequest>,
) -> ApiResult<(StatusCode, Json<api::TotpEnrolment>)> {
    let begun = s
        .access
        .begin_totp(
            &who,
            session_api::session_cookie(&headers),
            req.label.as_deref(),
            req.password.as_deref(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::TotpEnrolment {
            id: begun.factor.id,
            secret: begun.secret,
            uri: begun.uri,
        }),
    ))
}

#[utoipa::path(post, path = "/v1/me/two-factor/totp/{id}/confirm", request_body = api::CodeRequest,
    params(("id" = String, Path, description = "the pending factor")), responses(
    (status = 200, body = api::TotpConfirmed, description = "the factor is on; the account's first one also gets its recovery codes, shown only now"),
    (status = 401, body = api::ErrorBody, description = "second_factor_invalid"),
    (status = 404, body = api::ErrorBody, description = "factor_not_found: no such pending factor"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn confirm_totp(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Path(id): Path<String>,
    Json(req): Json<api::CodeRequest>,
) -> ApiResult<Json<api::TotpConfirmed>> {
    let done = s
        .access
        .confirm_totp(&who, session_api::session_cookie(&headers), &id, &req.code)
        .await?;
    Ok(Json(api::TotpConfirmed {
        factor: factor_dto(&done.factor),
        recovery_codes: done.recovery_codes,
        fresh_until: done.fresh_until,
    }))
}

#[utoipa::path(delete, path = "/v1/me/two-factor/{id}", params(("id" = String, Path, description = "a factor, or a pending enrolment")), responses(
    (status = 204, description = "removed; removing the last one turns two-factor authentication off and drops the recovery codes"),
    (status = 403, body = api::ErrorBody, description = "fresh_check_required"),
    (status = 404, body = api::ErrorBody, description = "factor_not_found"),
))]
pub(crate) async fn remove_factor(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    s.access
        .remove_factor(&who, session_api::session_cookie(&headers), &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/me/two-factor/recovery-codes", responses(
    (status = 200, body = api::RecoveryCodes, description = "ten new codes, shown once; the old ones stop working"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: two-factor authentication is off"),
    (status = 403, body = api::ErrorBody, description = "fresh_check_required"),
))]
pub(crate) async fn recovery_codes(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<api::RecoveryCodes>> {
    let codes = s
        .access
        .regenerate_recovery_codes(&who, session_api::session_cookie(&headers))
        .await?;
    Ok(Json(api::RecoveryCodes {
        recovery_codes: codes,
    }))
}

#[utoipa::path(post, path = "/v1/me/two-factor/verify", request_body = api::CodeRequest, responses(
    (status = 200, body = api::FreshCheck, description = "sensitive actions stay open on this credential until `fresh_until`"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: two-factor authentication is off"),
    (status = 401, body = api::ErrorBody, description = "second_factor_invalid"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn verify_second_factor(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Json(req): Json<api::CodeRequest>,
) -> ApiResult<Json<api::FreshCheck>> {
    let until = s
        .access
        .verify_second_factor(&who, session_api::session_cookie(&headers), &req.code)
        .await?;
    Ok(Json(api::FreshCheck { fresh_until: until }))
}

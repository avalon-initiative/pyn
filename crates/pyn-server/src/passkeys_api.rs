//! Passkeys and security keys (#1105): registration, passkey sign-in, the fresh check, and managing the account's own.

use crate::json::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pyn_core::{
    PasskeyAssertion, PasskeyAttestation, PasskeyCreation, PasskeyRecord, PasskeyRequest, PynError,
    two_factor::CHALLENGE_MINUTES,
};
use pyn_proto as api;

use crate::caller::FullScopeCaller;
use crate::client::Client;
use crate::{ApiResult, AppState, session_api};

const TIMEOUT_MS: u64 = CHALLENGE_MINUTES as u64 * 60_000;

pub(crate) fn passkey_dto(p: &PasskeyRecord) -> api::Passkey {
    api::Passkey {
        id: p.id.clone(),
        name: p.name.clone(),
        created_at: p.created_at,
        last_used_at: p.last_used_at,
        transports: p.transports.clone(),
        sign_count: p.sign_count,
        backup_eligible: p.backup_eligible,
        backup_state: p.backup_state,
    }
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(field: &str, text: &str) -> ApiResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(text.trim_end_matches('='))
        .map_err(|_| PynError::InvalidRequest(format!("{field} is not base64url")).into())
}

fn descriptors(list: &[(Vec<u8>, Vec<String>)]) -> Vec<api::PasskeyDescriptor> {
    list.iter()
        .map(|(id, transports)| api::PasskeyDescriptor {
            kind: "public-key".into(),
            id: b64(id),
            transports: transports.clone(),
        })
        .collect()
}

fn registration_dto(c: PasskeyCreation) -> api::PasskeyRegistrationBegun {
    api::PasskeyRegistrationBegun {
        ceremony: c.ceremony,
        expires_at: c.expires_at,
        public_key: api::PasskeyCreationOptions {
            rp: api::PasskeyRp {
                id: c.rp_id,
                name: "pyn".into(),
            },
            user: api::PasskeyUser {
                id: b64(&c.user_handle),
                display_name: c.user_name.clone(),
                name: c.user_name,
            },
            challenge: b64(&c.challenge),
            pub_key_cred_params: [-7, -8]
                .map(|alg| api::PasskeyParam {
                    kind: "public-key".into(),
                    alg,
                })
                .to_vec(),
            timeout: TIMEOUT_MS,
            exclude_credentials: descriptors(&c.exclude),
            authenticator_selection: api::PasskeySelection {
                resident_key: "required".into(),
                user_verification: "required".into(),
            },
            attestation: "none".into(),
        },
    }
}

fn authentication_dto(r: PasskeyRequest) -> api::PasskeyAuthenticationBegun {
    api::PasskeyAuthenticationBegun {
        ceremony: r.ceremony,
        expires_at: r.expires_at,
        public_key: api::PasskeyRequestOptions {
            challenge: b64(&r.challenge),
            timeout: TIMEOUT_MS,
            rp_id: r.rp_id,
            allow_credentials: descriptors(&r.allow),
            user_verification: "required".into(),
        },
    }
}

fn assertion_from(c: &api::PasskeyAssertionCredential) -> ApiResult<PasskeyAssertion> {
    let r = &c.response;
    Ok(PasskeyAssertion {
        credential_id: unb64("credential.id", &c.id)?,
        client_data_json: unb64("clientDataJSON", &r.client_data_json)?,
        authenticator_data: unb64("authenticatorData", &r.authenticator_data)?,
        signature: unb64("signature", &r.signature)?,
        user_handle: r
            .user_handle
            .as_deref()
            .map(|h| unb64("userHandle", h))
            .transpose()?,
    })
}

#[utoipa::path(get, path = "/v1/me/passkeys", responses(
    (status = 200, body = Vec<api::Passkey>, description = "the account's passkeys, oldest first"),
))]
pub(crate) async fn list_passkeys(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<Vec<api::Passkey>>> {
    let list = s.access.list_passkeys(&who).await?;
    Ok(Json(list.iter().map(passkey_dto).collect()))
}

#[utoipa::path(post, path = "/v1/me/passkeys/register/begin", request_body = api::BeginPasskeyRequest, responses(
    (status = 200, body = api::PasskeyRegistrationBegun, description = "run `navigator.credentials.create` with `public_key`, then post the result to `register/finish`; the ceremony is single use and expires in five minutes"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account holds the most passkeys it may"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated: the password is wrong or missing"),
    (status = 403, body = api::ErrorBody, description = "fresh_check_required: the account already has a second factor"),
    (status = 501, body = api::ErrorBody, description = "passkeys_unavailable: the server has no public address WebAuthn accepts"),
))]
pub(crate) async fn begin_registration(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Json(req): Json<api::BeginPasskeyRequest>,
) -> ApiResult<Json<api::PasskeyRegistrationBegun>> {
    let begun = s
        .access
        .begin_passkey_registration(
            &who,
            session_api::session_cookie(&headers),
            req.password.as_deref(),
        )
        .await?;
    Ok(Json(registration_dto(begun)))
}

#[utoipa::path(post, path = "/v1/me/passkeys/register/finish", request_body = api::RegisterPasskeyRequest, responses(
    (status = 201, body = api::PasskeyRegistered, description = "the passkey is on; the account's first second factor also gets its recovery codes, shown only now"),
    (status = 400, body = api::ErrorBody, description = "invalid_request"),
    (status = 401, body = api::ErrorBody, description = "a refusal code (passkey_ceremony_expired, passkey_origin_not_allowed, passkey_user_verification_required, passkey_already_registered, ...) or passkey_invalid for malformed data"),
    (status = 501, body = api::ErrorBody, description = "passkeys_unavailable"),
))]
pub(crate) async fn finish_registration(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Json(req): Json<api::RegisterPasskeyRequest>,
) -> ApiResult<(StatusCode, Json<api::PasskeyRegistered>)> {
    let r = &req.credential.response;
    let answer = PasskeyAttestation {
        client_data_json: unb64("clientDataJSON", &r.client_data_json)?,
        attestation_object: unb64("attestationObject", &r.attestation_object)?,
        transports: r.transports.clone(),
    };
    let done = s
        .access
        .finish_passkey_registration(
            &who,
            session_api::session_cookie(&headers),
            &req.ceremony,
            req.name.as_deref(),
            &answer,
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::PasskeyRegistered {
            passkey: passkey_dto(&done.passkey),
            recovery_codes: done.recovery_codes,
            fresh_until: done.fresh_until,
        }),
    ))
}

#[utoipa::path(patch, path = "/v1/me/passkeys/{id}", request_body = api::RenamePasskeyRequest,
    params(("id" = String, Path, description = "a passkey")), responses(
    (status = 200, body = api::Passkey),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the name is empty or too long"),
    (status = 404, body = api::ErrorBody, description = "passkey_not_found"),
))]
pub(crate) async fn rename_passkey(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
    Path(id): Path<String>,
    Json(req): Json<api::RenamePasskeyRequest>,
) -> ApiResult<Json<api::Passkey>> {
    let passkey = s.access.rename_passkey(&who, &id, &req.name).await?;
    Ok(Json(passkey_dto(&passkey)))
}

#[utoipa::path(delete, path = "/v1/me/passkeys/{id}", params(("id" = String, Path, description = "a passkey")), responses(
    (status = 204, description = "removed; removing the last second factor turns two-factor authentication off and drops the recovery codes"),
    (status = 403, body = api::ErrorBody, description = "fresh_check_required"),
    (status = 404, body = api::ErrorBody, description = "passkey_not_found"),
))]
pub(crate) async fn remove_passkey(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    s.access
        .remove_passkey(&who, session_api::session_cookie(&headers), &id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/me/passkeys/verify/begin", responses(
    (status = 200, body = api::PasskeyAuthenticationBegun, description = "run `navigator.credentials.get` with `public_key`, then post the result to `verify/finish`"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account has no passkey"),
    (status = 501, body = api::ErrorBody, description = "passkeys_unavailable"),
))]
pub(crate) async fn begin_verify(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<api::PasskeyAuthenticationBegun>> {
    Ok(Json(authentication_dto(
        s.access.begin_passkey_verify(&who).await?,
    )))
}

#[utoipa::path(post, path = "/v1/me/passkeys/verify/finish", request_body = api::PasskeyAssertionRequest, responses(
    (status = 200, body = api::FreshCheck, description = "sensitive actions stay open on this credential until `fresh_until`"),
    (status = 401, body = api::ErrorBody, description = "a refusal code: passkey_ceremony_expired, passkey_not_registered, passkey_user_handle_mismatch, passkey_signature_invalid, passkey_counter_regressed, ... or passkey_invalid for malformed data"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
    (status = 501, body = api::ErrorBody, description = "passkeys_unavailable"),
))]
pub(crate) async fn finish_verify(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Json(req): Json<api::PasskeyAssertionRequest>,
) -> ApiResult<Json<api::FreshCheck>> {
    let until = s
        .access
        .finish_passkey_verify(
            &who,
            session_api::session_cookie(&headers),
            &req.ceremony,
            &assertion_from(&req.credential)?,
        )
        .await?;
    Ok(Json(api::FreshCheck { fresh_until: until }))
}

#[utoipa::path(post, path = "/v1/session/passkey/begin", request_body = api::BeginPasskeySignInRequest, responses(
    (status = 200, body = api::PasskeyAuthenticationBegun, description = "run `navigator.credentials.get` with `public_key`, then post the result to `/v1/session/passkey/finish`"),
    (status = 403, body = api::ErrorBody, description = "origin_not_allowed"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
    (status = 501, body = api::ErrorBody, description = "passkeys_unavailable"),
))]
pub(crate) async fn begin_sign_in(
    State(s): State<AppState>,
    Client(client): Client,
    Json(req): Json<api::BeginPasskeySignInRequest>,
) -> ApiResult<Json<api::PasskeyAuthenticationBegun>> {
    let begun = s
        .access
        .begin_passkey_sign_in(req.username.as_deref(), client.as_deref())
        .await?;
    Ok(Json(authentication_dto(begun)))
}

#[utoipa::path(post, path = "/v1/session/passkey/finish", request_body = api::PasskeyAssertionRequest, responses(
    (status = 200, body = api::SessionInfo, description = "sets the HttpOnly session cookie; the passkey stood for the password and the second factor"),
    (status = 401, body = api::ErrorBody, description = "a refusal code: passkey_ceremony_expired, passkey_not_registered, passkey_user_handle_mismatch, passkey_signature_invalid, passkey_counter_regressed, ... or passkey_invalid for malformed data"),
    (status = 403, body = api::ErrorBody, description = "the account cannot sign in: email_not_verified, approval_pending or account_disabled; or origin_not_allowed"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
    (status = 501, body = api::ErrorBody, description = "passkeys_unavailable"),
))]
pub(crate) async fn finish_sign_in(
    State(s): State<AppState>,
    Client(client): Client,
    headers: HeaderMap,
    Json(req): Json<api::PasskeyAssertionRequest>,
) -> ApiResult<Response> {
    let (record, cookie) = s
        .access
        .finish_passkey_sign_in(
            &req.ceremony,
            &assertion_from(&req.credential)?,
            req.remember,
            client.as_deref(),
        )
        .await?;
    Ok(session_api::session_response(
        &s, &headers, &record, &cookie,
    ))
}

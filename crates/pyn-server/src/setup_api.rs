//! First-run setup: the only routes besides the health check that a fresh server serves.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use pyn_core::{PynError, RegistrationMode, SetupRequest};
use pyn_proto as api;

use crate::client::Client;
use crate::{ApiError, ApiResult, AppState};

#[utoipa::path(get, path = "/v1/setup", responses((status = 200, body = api::SetupStatus)))]
pub(crate) async fn setup_status(State(s): State<AppState>) -> ApiResult<Json<api::SetupStatus>> {
    let status = s.access.setup_status().await?;
    Ok(Json(api::SetupStatus {
        initialised: status.initialised,
        server_name: status.server_name,
        public_url: status.public_url,
        registration: status.registration.as_str().to_string(),
    }))
}

#[utoipa::path(post, path = "/v1/setup", request_body = api::SetupRequest, responses(
    (status = 201, body = api::SetupCompleted),
    (status = 400, body = api::ErrorBody, description = "invalid_request or reserved_name"),
    (status = 403, body = api::ErrorBody, description = "invalid_setup_token"),
    (status = 409, body = api::ErrorBody, description = "already_initialised"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn complete_setup(
    State(s): State<AppState>,
    Client(client): Client,
    Json(req): Json<api::SetupRequest>,
) -> ApiResult<(StatusCode, Json<api::SetupCompleted>)> {
    let user = s
        .access
        .complete_setup(SetupRequest {
            token: &req.token,
            username: &req.username,
            password: &req.password,
            email: req.email.as_deref(),
            server_name: req.server_name.as_deref(),
            public_url: req.public_url.as_deref(),
            registration: RegistrationMode::from_str(&req.registration)?,
            client: client.as_deref(),
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::SetupCompleted {
            user: user.to_string(),
        }),
    ))
}

/// Refuses every route but setup and the health check until the server is initialised.
pub(crate) async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if matches!(req.uri().path(), "/healthz" | "/v1/setup") {
        return next.run(req).await;
    }
    match s.access.is_initialised().await {
        Ok(true) => next.run(req).await,
        Ok(false) => ApiError::from(PynError::NotInitialised).into_response(),
        Err(e) => ApiError::from(e).into_response(),
    }
}

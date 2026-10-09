//! Web sessions: an HttpOnly cookie plus a CSRF header on state-changing requests.

use axum::Json;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use pyn_core::{PynError, SessionRecord};
use pyn_proto as api;

use crate::client::Client;
use crate::{ApiResult, AppState};

pub(crate) fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == api::SESSION_COOKIE && !value.is_empty()).then_some(value)
        })
}

fn is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"))
}

fn set_cookie(value: &str, max_age_secs: i64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{}={value}; Path=/; Max-Age={max_age_secs}; HttpOnly; SameSite=Lax{secure}",
        api::SESSION_COOKIE
    );
    HeaderValue::from_str(&cookie).expect("cookie is ASCII")
}

fn info(session: &SessionRecord) -> api::SessionInfo {
    api::SessionInfo {
        user: session.user.to_string(),
        csrf_token: session.csrf_token.clone(),
        expires_at: session.expires_at,
    }
}

fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Rejects a state-changing request authenticated only by a session cookie unless it carries the session's CSRF token.
pub(crate) async fn csrf_guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let headers = req.headers();
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    let other_credential =
        headers.contains_key(AUTHORIZATION) || headers.contains_key(api::DEV_USER_HEADER);
    let sign_in = req.method() == Method::POST && req.uri().path() == "/v1/session";
    if safe || other_credential || sign_in {
        return next.run(req).await;
    }
    if let Some(cookie) = session_cookie(headers) {
        let given = headers.get(api::CSRF_HEADER).and_then(|v| v.to_str().ok());
        match s.access.find_session(cookie).await {
            Ok(Some(session)) if !given.is_some_and(|g| same(g, &session.csrf_token)) => {
                return crate::ApiError::from(PynError::CsrfFailed).into_response();
            }
            Err(e) => return crate::ApiError::from(e).into_response(),
            _ => {}
        }
    }
    next.run(req).await
}

#[utoipa::path(post, path = "/v1/session", request_body = api::LoginRequest, responses(
    (status = 200, body = api::SessionInfo, description = "sets the HttpOnly session cookie"),
    (status = 401, body = api::ErrorBody, description = "unauthenticated"),
    (status = 403, body = api::ErrorBody, description = "right password, but the account cannot sign in: email_not_verified, approval_pending or account_disabled"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn sign_in(
    State(s): State<AppState>,
    Client(client): Client,
    headers: HeaderMap,
    Json(req): Json<api::LoginRequest>,
) -> ApiResult<Response> {
    let (record, cookie) = s
        .access
        .start_session(&req.username, &req.password, client.as_deref())
        .await?;
    let max_age = s.access.session_lifetime().num_seconds();
    let mut res = Json(info(&record)).into_response();
    res.headers_mut()
        .insert(SET_COOKIE, set_cookie(&cookie, max_age, is_https(&headers)));
    Ok(res)
}

#[utoipa::path(get, path = "/v1/session", responses(
    (status = 200, body = api::SessionInfo),
    (status = 401, body = api::ErrorBody, description = "not signed in"),
))]
pub(crate) async fn current(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<api::SessionInfo>> {
    let cookie = session_cookie(&headers)
        .ok_or_else(|| PynError::Unauthenticated("not signed in".into()))?;
    let (_, record) = s.access.authenticate_session(cookie).await?;
    Ok(Json(info(&record)))
}

#[utoipa::path(delete, path = "/v1/session", responses(
    (status = 204, description = "clears the cookie"),
    (status = 403, body = api::ErrorBody, description = "csrf_failed"),
))]
pub(crate) async fn sign_out(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    if let Some(cookie) = session_cookie(&headers) {
        s.access.end_session(cookie).await?;
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut()
        .insert(SET_COOKIE, set_cookie("", 0, is_https(&headers)));
    Ok(res)
}

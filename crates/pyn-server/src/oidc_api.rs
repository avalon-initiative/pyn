//! Sign-in with an external OpenID Connect provider. The browser is sent to the provider and comes back to
//! `/v1/oidc/callback`, which ends in the same session cookie as a password sign-in.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::header::{LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pyn_core::{ExternalIdentity, ExternalSignIn, OidcBegin, OidcFinish, OidcStart, PynError};
use pyn_proto as api;
use serde::Deserialize;

use crate::client::Client;
use crate::session_api::{cookie_value, is_https, set_cookie};
use crate::{ApiResult, AppState, identify};

const BINDING_PATH: &str = "/v1/oidc";
const BINDING_MAX_AGE: i64 = 600;

fn binding_cookie(value: &str, max_age_secs: i64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{}={value}; Path={BINDING_PATH}; Max-Age={max_age_secs}; HttpOnly; SameSite=Lax{secure}",
        api::OIDC_BINDING_COOKIE
    );
    HeaderValue::from_str(&cookie).expect("cookie is ASCII")
}

fn redirect(to: &str) -> Response {
    let mut res = StatusCode::FOUND.into_response();
    res.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(to).unwrap_or_else(|_| HeaderValue::from_static("/")),
    );
    res
}

fn identity_info(i: ExternalIdentity) -> api::ExternalIdentityInfo {
    api::ExternalIdentityInfo {
        id: i.id,
        issuer: i.issuer,
        subject: i.subject,
        email: i.email,
        created_at: i.created_at,
        last_sign_in_at: i.last_sign_in_at,
    }
}

#[utoipa::path(get, path = "/v1/sign-in-options", responses((status = 200, body = api::SignInOptions)))]
pub(crate) async fn sign_in_options(State(s): State<AppState>) -> Json<api::SignInOptions> {
    Json(api::SignInOptions {
        password: s.access.password_sign_in_enabled(),
        external: s.access.oidc_name().map(|name| api::ExternalProviderInfo {
            name: name.to_string(),
            sign_in_url: "/v1/oidc/authorize".into(),
        }),
    })
}

#[derive(Deserialize)]
pub(crate) struct AuthorizeQuery {
    return_to: Option<String>,
}

fn started(begin: OidcBegin, headers: &HeaderMap, body: Response) -> Response {
    let mut res = body;
    res.headers_mut().insert(
        SET_COOKIE,
        binding_cookie(&begin.binding, BINDING_MAX_AGE, is_https(headers)),
    );
    res
}

#[utoipa::path(get, path = "/v1/oidc/authorize",
    params(("return_to" = Option<String>, Query, description = "a path in the web app to open after signing in, such as /repos")),
    responses(
        (status = 302, description = "to the provider; sets a short-lived cookie that ties the sign-in to this browser"),
        (status = 400, body = api::ErrorBody, description = "invalid_request: return_to is not a path in the web app"),
        (status = 404, body = api::ErrorBody, description = "oidc_not_configured"),
        (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
        (status = 502, body = api::ErrorBody, description = "external_provider_unavailable"),
    ))]
pub(crate) async fn authorize(
    State(s): State<AppState>,
    Client(client): Client,
    headers: HeaderMap,
    Query(q): Query<AuthorizeQuery>,
) -> ApiResult<Response> {
    let begin = s
        .access
        .begin_oidc(OidcStart {
            link: None,
            return_to: q.return_to.as_deref(),
            client: client.as_deref(),
        })
        .await?;
    let to = begin.url.clone();
    Ok(started(begin, &headers, redirect(&to)))
}

#[utoipa::path(post, path = "/v1/oidc/link",
    params(("return_to" = Option<String>, Query, description = "a path in the web app to open after linking")),
    responses(
        (status = 200, body = api::OidcLink, description = "send the browser to `url`; the provider identity it signs in as is linked to the caller's account"),
        (status = 401, body = api::ErrorBody),
        (status = 404, body = api::ErrorBody, description = "oidc_not_configured"),
        (status = 502, body = api::ErrorBody, description = "external_provider_unavailable"),
    ))]
pub(crate) async fn link(
    State(s): State<AppState>,
    Client(client): Client,
    headers: HeaderMap,
    Query(q): Query<AuthorizeQuery>,
) -> ApiResult<Response> {
    let who = identify(&s, &headers).await?;
    let begin = s
        .access
        .begin_oidc(OidcStart {
            link: Some(&who),
            return_to: q.return_to.as_deref(),
            client: client.as_deref(),
        })
        .await?;
    let body = Json(api::OidcLink {
        url: begin.url.clone(),
    })
    .into_response();
    Ok(started(begin, &headers, body))
}

#[derive(Deserialize)]
pub(crate) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[utoipa::path(get, path = "/v1/oidc/callback",
    params(
        ("code" = Option<String>, Query, description = "the authorization code"),
        ("state" = Option<String>, Query, description = "the state sent with the authorization request"),
        ("error" = Option<String>, Query, description = "set by the provider when the person did not sign in"),
    ),
    responses((
        status = 302,
        description = "to the web app: the return_to path (or /) with the session cookie set, or /sign-in?error=<code> on failure",
    )))]
pub(crate) async fn callback(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let web = s.access.public_url();
    let web = web.trim_end_matches('/');
    let secure = is_https(&headers);
    let outcome = match (&q.code, &q.state, &q.error) {
        (Some(code), Some(state), None) => {
            s.access
                .finish_oidc(OidcFinish {
                    code,
                    state,
                    binding: cookie_value(&headers, api::OIDC_BINDING_COOKIE),
                })
                .await
        }
        (_, _, Some(_)) => Err(PynError::ExternalSignInFailed(
            "the provider did not sign you in".into(),
        )),
        _ => Err(PynError::ExternalSignInFailed(
            "the callback is missing its code or state".into(),
        )),
    };
    let mut res = match outcome {
        Ok(ExternalSignIn::SignedIn {
            session,
            cookie,
            return_to,
            ..
        }) => {
            let max_age = (session.expires_at - session.created_at).num_seconds();
            let mut res = redirect(&format!("{web}{}", return_to.as_deref().unwrap_or("/")));
            res.headers_mut()
                .append(SET_COOKIE, set_cookie(&cookie, max_age, secure));
            res
        }
        Ok(ExternalSignIn::Linked { return_to, .. }) => {
            redirect(&format!("{web}{}", return_to.as_deref().unwrap_or("/")))
        }
        Err(e) => redirect(&format!("{web}/sign-in?error={}", e.code())),
    };
    res.headers_mut()
        .append(SET_COOKIE, binding_cookie("", 0, secure));
    res
}

#[utoipa::path(get, path = "/v1/me/identities", responses(
    (status = 200, body = Vec<api::ExternalIdentityInfo>, description = "the provider identities linked to the caller's account, oldest first"),
    (status = 401, body = api::ErrorBody),
))]
pub(crate) async fn list_identities(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::ExternalIdentityInfo>>> {
    let who = identify(&s, &headers).await?;
    let identities = s.access.external_identities(&who).await?;
    Ok(Json(identities.into_iter().map(identity_info).collect()))
}

#[utoipa::path(delete, path = "/v1/me/identities/{id}",
    params(("id" = String, Path, description = "the link's id")),
    responses(
        (status = 204),
        (status = 404, body = api::ErrorBody, description = "external_identity_not_found"),
        (status = 409, body = api::ErrorBody, description = "last_sign_in_method: the account has no password and no other link"),
    ))]
pub(crate) async fn unlink_identity(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access.unlink_external_identity(&who, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

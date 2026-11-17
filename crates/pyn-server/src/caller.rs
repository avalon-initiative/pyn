//! Who is calling: the extractors a handler takes to learn the caller, each tied to the reach declared for its route
//! in `routes.rs`. Only this module reads credentials; a handler that takes no extractor never sees an identity.

use std::collections::HashMap;

use axum::extract::{FromRequestParts, Path};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use pyn_core::repositories::OpenRepo;
use pyn_core::{AccessMode, Credential, Identity, Permission, Principal, PynError, SessionRecord};
use pyn_proto as api;

use crate::routes::{Declared, Reach};
use crate::{ApiError, ApiResult, AppState, open_visible, session_api};

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

/// Whether the request is authenticated by a bearer token or the dev header rather than a cookie.
pub(crate) fn uses_non_cookie_credential(state: &AppState, headers: &HeaderMap) -> bool {
    bearer_token(headers).is_some()
        || (state.dev_auth.is_some()
            && headers
                .get(api::DEV_USER_HEADER)
                .is_some_and(|v| v.to_str().is_ok()))
}

/// Names the account behind a bearer token, the dev header or a session cookie. A service credential is refused.
async fn identify(state: &AppState, headers: &HeaderMap) -> ApiResult<Identity> {
    let who = identify_admin(state, headers).await?;
    if matches!(who.credential, Credential::Service(_)) {
        return Err(PynError::ServiceCredentialNotAllowed.into());
    }
    Ok(who)
}

/// Like `identify`, but a service credential is accepted too.
async fn identify_admin(state: &AppState, headers: &HeaderMap) -> ApiResult<Identity> {
    let dev_user = headers
        .get(api::DEV_USER_HEADER)
        .and_then(|v| v.to_str().ok());
    let who = match (bearer_token(headers), &state.dev_auth, dev_user) {
        (Some(token), _, _) => state.auth.authenticate(token).await?,
        (None, Some(dev), Some(user)) => {
            let who = dev.authenticate(user).await?;
            state.access.reject_org(&who.user).await?;
            who
        }
        _ => match session_api::session_cookie(headers) {
            Some(cookie) => state.access.authenticate_session(cookie).await?.0,
            None => return Err(PynError::Unauthenticated("missing credentials".into()).into()),
        },
    };
    crate::observe::note_account(&who.user);
    Ok(who)
}

/// Like `identify`, but a request with no credential at all is anonymous; a bad one still fails.
async fn identify_optional(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<Identity>> {
    if !sends_credential(state, headers) {
        return Ok(None);
    }
    identify(state, headers).await.map(Some)
}

fn sends_credential(state: &AppState, headers: &HeaderMap) -> bool {
    let dev = state.dev_auth.is_some() && headers.contains_key(api::DEV_USER_HEADER);
    bearer_token(headers).is_some() || dev || session_api::session_cookie(headers).is_some()
}

/// git-lfs sends its token as the Basic password.
fn sends_basic(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
}

/// Applies the access mode to a route declared `reach`. On a private server a signed-out caller is refused: a
/// repository route answers as for a private repository, any other with `unauthenticated`. A credential that is sent
/// is validated by the route's own extractor, except on `Public` routes, which read none.
pub(crate) async fn admit(
    state: &AppState,
    headers: &HeaderMap,
    reach: Reach,
    repo: Option<String>,
) -> ApiResult<()> {
    if state.access.access_mode() == AccessMode::Public {
        return Ok(());
    }
    let signed_in = match reach {
        Reach::Public => identify_optional(state, headers).await?.is_some(),
        Reach::PublicLfsRead => sends_credential(state, headers) || sends_basic(headers),
        _ => sends_credential(state, headers),
    };
    if signed_in {
        return Ok(());
    }
    match (reach, repo) {
        (Reach::PublicRepoRead | Reach::PublicRepoVisible, Some(repo)) => {
            Err(PynError::RepoNotFound(repo).into())
        }
        _ => Err(PynError::Unauthenticated("sign in to use this server".into()).into()),
    }
}

/// Like `identify`, but an account its owner scheduled for deletion is accepted: its restore routes need it.
async fn identify_pending_ok(state: &AppState, headers: &HeaderMap) -> ApiResult<Identity> {
    let who = match (bearer_token(headers), session_api::session_cookie(headers)) {
        (Some(token), _) => state.access.identify_allowing_pending(token).await?,
        (None, Some(cookie)) => {
            state
                .access
                .authenticate_session_allowing_pending(cookie)
                .await?
                .0
        }
        (None, None) => identify_admin(state, headers).await?,
    };
    if matches!(who.credential, Credential::Service(_)) {
        return Err(PynError::ServiceCredentialNotAllowed.into());
    }
    Ok(who)
}

fn declared(parts: &Parts) -> Reach {
    parts
        .extensions
        .get::<Declared>()
        .map_or(Reach::Open, |d| d.0)
}

/// A handler took an extractor its route's declared reach does not allow: a bug, never a client error.
fn mismatch(extractor: &str, reach: Reach) -> ApiError {
    ApiError(PynError::Storage(format!(
        "{extractor} on a route declared {}",
        reach.label()
    )))
}

/// The caller of a route declared `any_credential`.
pub(crate) struct AnyCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for AnyCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        match declared(parts) {
            Reach::AnyCredential => Ok(Self(identify(s, &parts.headers).await?)),
            other => Err(mismatch("AnyCaller", other)),
        }
    }
}

/// The caller of a route declared `optional_sign_in`: anonymous without a credential.
pub(crate) struct OptionalCaller(pub(crate) Option<Identity>);

impl FromRequestParts<AppState> for OptionalCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        match declared(parts) {
            Reach::OptionalSignIn => Ok(Self(identify_optional(s, &parts.headers).await?)),
            other => Err(mismatch("OptionalCaller", other)),
        }
    }
}

/// The caller of a route declared `full_scope` or `full_scope_pending`.
pub(crate) struct FullScopeCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for FullScopeCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let who = match declared(parts) {
            Reach::FullScope => identify(s, &parts.headers).await?,
            Reach::FullScopePending => identify_pending_ok(s, &parts.headers).await?,
            other => return Err(mismatch("FullScopeCaller", other)),
        };
        who.require_full_scope()?;
        Ok(Self(who))
    }
}

/// The caller of a route declared `namespace`.
pub(crate) struct NamespaceCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for NamespaceCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        match declared(parts) {
            Reach::Namespace => {
                let who = identify(s, &parts.headers).await?;
                who.require_namespace_management()?;
                Ok(Self(who))
            }
            other => Err(mismatch("NamespaceCaller", other)),
        }
    }
}

/// The caller of a route declared `admin_only`.
pub(crate) struct AdminCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for AdminCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        match declared(parts) {
            Reach::AdminOnly => {
                let who = identify_admin(s, &parts.headers).await?;
                if !s.access.is_server_admin(&who).await? {
                    return Err(PynError::ServerAdminRequired.into());
                }
                Ok(Self(who))
            }
            other => Err(mismatch("AdminCaller", other)),
        }
    }
}

/// The caller of a route declared `service` or `full_scope_or_service`.
pub(crate) struct ServiceCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for ServiceCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let reach = declared(parts);
        let (Reach::Service(scope) | Reach::FullScopeOrService(scope)) = reach else {
            return Err(mismatch("ServiceCaller", reach));
        };
        let who = identify_admin(s, &parts.headers).await?;
        match &who.credential {
            Credential::Service(c) if c.scopes.contains(&scope) => {}
            Credential::Service(_) => return Err(PynError::ServiceScopeRequired(scope).into()),
            _ if matches!(reach, Reach::Service(_)) => {
                if !s.access.is_server_admin(&who).await? {
                    return Err(PynError::ServerAdminRequired.into());
                }
            }
            _ => who.require_full_scope()?,
        }
        Ok(Self(who))
    }
}

/// The caller of a route declared `session`.
pub(crate) struct SessionCaller(pub(crate) SessionRecord);

impl FromRequestParts<AppState> for SessionCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        match declared(parts) {
            Reach::Session => {
                let cookie = session_api::session_cookie(&parts.headers)
                    .ok_or_else(|| PynError::Unauthenticated("not signed in".into()))?;
                let (_, record) = s
                    .access
                    .authenticate_session_allowing_pending(cookie)
                    .await?;
                Ok(Self(record))
            }
            other => Err(mismatch("SessionCaller", other)),
        }
    }
}

async fn address(parts: &mut Parts, s: &AppState) -> ApiResult<(String, String)> {
    let Path(params) = Path::<HashMap<String, String>>::from_request_parts(parts, s)
        .await
        .map_err(|_| PynError::InvalidRequest("a repository route without an address".into()))?;
    match (params.get("owner"), params.get("name")) {
        (Some(owner), Some(name)) => Ok((owner.clone(), name.clone())),
        _ => Err(PynError::InvalidRequest("a repository route without an address".into()).into()),
    }
}

/// The signed-in caller of a repository route and what they may do there.
pub(crate) struct RepoCaller {
    pub(crate) repo: OpenRepo,
    pub(crate) principal: Principal,
    pub(crate) identity: Identity,
}

impl FromRequestParts<AppState> for RepoCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let need = match declared(parts) {
            Reach::RepoScoped(permission) => Some(permission),
            Reach::RepoVisible => None,
            other => return Err(mismatch("RepoCaller", other)),
        };
        let identity = identify(s, &parts.headers).await?;
        let (owner, name) = address(parts, s).await?;
        let (repo, principal) = open_visible(s, Some(&identity), &owner, &name).await?;
        if let Some(permission) = need {
            principal.require(permission)?;
        }
        Ok(Self {
            repo,
            principal,
            identity,
        })
    }
}

/// The caller of a repository route that a public repository serves without a credential.
pub(crate) struct RepoReader {
    pub(crate) repo: OpenRepo,
    pub(crate) principal: Principal,
    pub(crate) identity: Option<Identity>,
}

impl FromRequestParts<AppState> for RepoReader {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let read = match declared(parts) {
            Reach::PublicRepoRead => true,
            Reach::PublicRepoVisible => false,
            other => return Err(mismatch("RepoReader", other)),
        };
        let identity = identify_optional(s, &parts.headers).await?;
        let (owner, name) = address(parts, s).await?;
        let (repo, principal) = open_visible(s, identity.as_ref(), &owner, &name).await?;
        if read {
            principal.require(Permission::Read)?;
        }
        Ok(Self {
            repo,
            principal,
            identity,
        })
    }
}

/// The caller of an LFS or tus route. Authorizing is left to the handler: these routes answer in their own error format
/// and read their body first.
pub(crate) struct LfsCaller {
    state: AppState,
    need: Option<Permission>,
    anonymous_read: bool,
}

impl FromRequestParts<AppState> for LfsCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let need = match declared(parts) {
            Reach::RepoScoped(permission) => Some(permission),
            Reach::RepoVisible => None,
            Reach::PublicLfsRead => Some(Permission::Read),
            other => return Err(mismatch("LfsCaller", other)),
        };
        Ok(Self {
            state: s.clone(),
            need,
            anonymous_read: matches!(declared(parts), Reach::PublicLfsRead),
        })
    }
}

impl LfsCaller {
    /// Authenticates with `headers` and opens the named repository, holding the declared permission.
    pub(crate) async fn authorize(
        &self,
        headers: &HeaderMap,
        owner: &str,
        name: &str,
    ) -> ApiResult<(OpenRepo, Principal, Identity)> {
        let identity = identify(&self.state, headers).await?;
        let (repo, principal) = open_visible(&self.state, Some(&identity), owner, name).await?;
        if let Some(permission) = self.need {
            principal.require(permission)?;
        }
        Ok((repo, principal, identity))
    }

    /// Like `authorize`, but a request with no credential opens a public repository as an anonymous reader. Only a route
    /// declared `public_lfs_read` may call it.
    pub(crate) async fn authorize_reader(
        &self,
        headers: &HeaderMap,
        owner: &str,
        name: &str,
    ) -> ApiResult<(OpenRepo, Principal)> {
        if !self.anonymous_read {
            return Err(ApiError(PynError::Storage(
                "authorize_reader on a route not declared public_lfs_read".into(),
            )));
        }
        let identity = identify_optional(&self.state, headers).await?;
        let (repo, principal) = open_visible(&self.state, identity.as_ref(), owner, name).await?;
        principal.require(Permission::Read)?;
        Ok((repo, principal))
    }
}

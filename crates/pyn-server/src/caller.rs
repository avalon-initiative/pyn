//! Who is calling: the extractors a handler takes to learn the caller, each tied to the reach declared for its route
//! in `routes.rs`. Only this module reads credentials; a handler that takes no extractor never sees an identity.

use std::collections::HashMap;

use axum::extract::{FromRequestParts, Path};
use std::net::IpAddr;

use axum::http::HeaderMap;
use axum::http::request::Parts;
use pyn_core::repositories::OpenRepo;
use pyn_core::{
    AccessMode, Credential, GrantLevel, GrantNeed, Identity, Principal, PynError,
    ServiceCredentialRecord, SessionRecord, UserId, Visibility, service_credential,
};
use pyn_proto as api;

use crate::client::source_ip;
use crate::routes::{Declared, Reach};
use crate::{ApiError, ApiResult, AppState, open_visible, session_api};

/// What identifying a caller reads from a request: its headers and the address it came from.
#[derive(Clone, Copy)]
pub(crate) struct Req<'a> {
    pub(crate) headers: &'a HeaderMap,
    pub(crate) source: Option<IpAddr>,
}

fn req<'a>(parts: &'a Parts, state: &AppState) -> Req<'a> {
    Req {
        headers: &parts.headers,
        source: source_ip(&parts.headers, &parts.extensions, state.trust_forwarded),
    }
}

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

/// Names the account behind a bearer token, the dev header or a session cookie. A service credential is refused, and
/// so is an account that must set up a second factor and has not.
async fn identify(state: &AppState, req: Req<'_>) -> ApiResult<Identity> {
    let who = identify_admin(state, req).await?;
    if let Credential::Service(c) = &who.credential {
        return Err(refuse(
            state,
            c,
            req,
            PynError::ServiceCredentialNotAllowed,
            "route not open to service credentials",
        )
        .await);
    }
    Ok(who)
}

/// Audits a refused service credential request and returns the refusal.
async fn refuse(
    state: &AppState,
    credential: &ServiceCredentialRecord,
    req: Req<'_>,
    error: PynError,
    reason: &str,
) -> ApiError {
    state
        .access
        .record_service_refusal(credential, reason, req.source)
        .await;
    error.into()
}

/// Like `identify`, but a service credential is accepted too.
async fn identify_admin(state: &AppState, req: Req<'_>) -> ApiResult<Identity> {
    let who = authenticate(state, req).await?;
    state.access.enforce_two_factor(&who).await?;
    Ok(who)
}

/// Like `identify`, but an account that must set up a second factor is accepted: the enrolment routes need it.
async fn identify_enrolling(state: &AppState, req: Req<'_>) -> ApiResult<Identity> {
    let who = authenticate(state, req).await?;
    if let Credential::Service(c) = &who.credential {
        return Err(refuse(
            state,
            c,
            req,
            PynError::ServiceCredentialNotAllowed,
            "route not open to service credentials",
        )
        .await);
    }
    Ok(who)
}

async fn authenticate(state: &AppState, req: Req<'_>) -> ApiResult<Identity> {
    let headers = req.headers;
    let dev_user = headers
        .get(api::DEV_USER_HEADER)
        .and_then(|v| v.to_str().ok());
    let who = match (bearer_token(headers), &state.dev_auth, dev_user) {
        (Some(token), _, _) => state.auth.authenticate(token, req.source).await?,
        (None, Some(dev), Some(user)) => {
            let who = dev.authenticate(user, req.source).await?;
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
async fn identify_optional(state: &AppState, req: Req<'_>) -> ApiResult<Option<Identity>> {
    if !sends_credential(state, req.headers) {
        return Ok(None);
    }
    identify(state, req).await.map(Some)
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
    req: Req<'_>,
    reach: Reach,
    repo: Option<String>,
) -> ApiResult<()> {
    let headers = req.headers;
    if state.access.access_mode() == AccessMode::Public {
        return Ok(());
    }
    let signed_in = match reach {
        Reach::Public => identify_optional(state, req).await?.is_some(),
        Reach::PublicLfsRead(_) => sends_credential(state, headers) || sends_basic(headers),
        _ => sends_credential(state, headers),
    };
    if signed_in {
        return Ok(());
    }
    match (reach, repo) {
        (Reach::PublicRepoRead(_), Some(repo)) => Err(PynError::RepoNotFound(repo).into()),
        _ => Err(PynError::Unauthenticated("sign in to use this server".into()).into()),
    }
}

/// Like `identify`, but an account its owner scheduled for deletion is accepted: its restore routes need it.
async fn identify_pending_ok(state: &AppState, req: Req<'_>) -> ApiResult<Identity> {
    let headers = req.headers;
    let who = match (bearer_token(headers), session_api::session_cookie(headers)) {
        (Some(token), _) if !service_credential::is_service(token) => {
            state.access.identify_allowing_pending(token).await?
        }
        (None, Some(cookie)) => {
            state
                .access
                .authenticate_session_allowing_pending(cookie)
                .await?
                .0
        }
        _ => identify_admin(state, req).await?,
    };
    if let Credential::Service(c) = &who.credential {
        return Err(refuse(
            state,
            c,
            req,
            PynError::ServiceCredentialNotAllowed,
            "route not open to service credentials",
        )
        .await);
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
            Reach::AnyCredential => Ok(Self(identify(s, req(parts, s)).await?)),
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
            Reach::OptionalSignIn => Ok(Self(identify_optional(s, req(parts, s)).await?)),
            other => Err(mismatch("OptionalCaller", other)),
        }
    }
}

/// The caller of a route declared `optional_full_scope`: anonymous without a credential, else full scope.
pub(crate) struct OptionalFullScopeCaller(pub(crate) Option<Identity>);

impl FromRequestParts<AppState> for OptionalFullScopeCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        match declared(parts) {
            Reach::OptionalFullScope => {
                let who = identify_optional(s, req(parts, s)).await?;
                if let Some(who) = &who {
                    who.require_full_scope()?;
                }
                Ok(Self(who))
            }
            other => Err(mismatch("OptionalFullScopeCaller", other)),
        }
    }
}

/// The caller of a route declared `full_scope`, `full_scope_pending` or `full_scope_enrolment`.
pub(crate) struct FullScopeCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for FullScopeCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let who = match declared(parts) {
            Reach::FullScope => identify(s, req(parts, s)).await?,
            Reach::FullScopePending => identify_pending_ok(s, req(parts, s)).await?,
            Reach::FullScopeEnrolment => identify_enrolling(s, req(parts, s)).await?,
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
                let who = identify(s, req(parts, s)).await?;
                who.require_namespace_management()?;
                Ok(Self(who))
            }
            other => Err(mismatch("NamespaceCaller", other)),
        }
    }
}

/// The caller of a route declared `org_scoped`, with the grant for the organization in the
/// path already checked.
pub(crate) struct OrgCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for OrgCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let reach = declared(parts);
        let Reach::OrgScoped(need) = reach else {
            return Err(mismatch("OrgCaller", reach));
        };
        let who = identify(s, req(parts, s)).await?;
        let Path(params) = Path::<HashMap<String, String>>::from_request_parts(parts, s)
            .await
            .map_err(|_| {
                PynError::InvalidRequest("an organization route without an organization".into())
            })?;
        let org = UserId::new(params.get("org").cloned().unwrap_or_default());
        who.require_org_grant(&org, need)
            .map_err(|_| PynError::GrantRequired(need))?;
        Ok(Self(who))
    }
}

/// The caller of a route declared `account_scoped`, with the grant already checked.
pub(crate) struct AccountCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for AccountCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let Reach::AccountScoped(need) = declared(parts) else {
            return Err(mismatch("AccountCaller", declared(parts)));
        };
        let who = identify(s, req(parts, s)).await?;
        who.require_account_grant(need)
            .map_err(|_| PynError::GrantRequired(need))?;
        Ok(Self(who))
    }
}

/// The caller of a route declared `admin_only`.
pub(crate) struct AdminCaller(pub(crate) Identity);

impl FromRequestParts<AppState> for AdminCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        match declared(parts) {
            Reach::AdminOnly => {
                let r = req(parts, s);
                let who = identify_admin(s, r).await?;
                if !s.access.is_server_admin(&who).await? {
                    if let Credential::Service(c) = &who.credential {
                        let err = PynError::ServerAdminRequired;
                        return Err(refuse(s, c, r, err, "server administrator required").await);
                    }
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
        let r = req(parts, s);
        let who = identify_admin(s, r).await?;
        match &who.credential {
            Credential::Service(c) if c.scopes.contains(&scope) => {}
            Credential::Service(c) => {
                let why = format!("scope {scope} required");
                return Err(refuse(s, c, r, PynError::ServiceScopeRequired(scope), &why).await);
            }
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
        let Reach::RepoScoped(need) = declared(parts) else {
            return Err(mismatch("RepoCaller", declared(parts)));
        };
        let identity = identify(s, req(parts, s)).await?;
        let (owner, name) = address(parts, s).await?;
        let (repo, principal) = open_visible(s, Some(&identity), &owner, &name).await?;
        meets(Some(&identity), &repo, &principal, need)?;
        Ok(Self {
            repo,
            principal,
            identity,
        })
    }
}

/// Checks a route's declared need: a token's grants, then the principal's permissions (the role as the token narrows
/// it). A read of a public repository needs neither, as anyone may make it.
fn meets(
    identity: Option<&Identity>,
    repo: &OpenRepo,
    principal: &Principal,
    need: GrantNeed,
) -> ApiResult<()> {
    let public_read =
        repo.record.visibility == Visibility::Public && need.level == GrantLevel::Read;
    if let (Some(who), false) = (identity, public_read) {
        who.require_grant(need)?;
    }
    principal.require_need(need)?;
    Ok(())
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
        let Reach::PublicRepoRead(need) = declared(parts) else {
            return Err(mismatch("RepoReader", declared(parts)));
        };
        let identity = identify_optional(s, req(parts, s)).await?;
        let (owner, name) = address(parts, s).await?;
        let (repo, principal) = open_visible(s, identity.as_ref(), &owner, &name).await?;
        meets(identity.as_ref(), &repo, &principal, need)?;
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
    source: Option<IpAddr>,
    need: GrantNeed,
    anonymous_read: bool,
}

impl FromRequestParts<AppState> for LfsCaller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, s: &AppState) -> ApiResult<Self> {
        let need = match declared(parts) {
            Reach::RepoScoped(need) | Reach::PublicLfsRead(need) => need,
            other => return Err(mismatch("LfsCaller", other)),
        };
        Ok(Self {
            state: s.clone(),
            source: req(parts, s).source,
            need,
            anonymous_read: matches!(declared(parts), Reach::PublicLfsRead(_)),
        })
    }
}

impl LfsCaller {
    fn req<'a>(&self, headers: &'a HeaderMap) -> Req<'a> {
        Req {
            headers,
            source: self.source,
        }
    }

    /// Authenticates with `headers` and opens the named repository, meeting the declared need.
    pub(crate) async fn authorize(
        &self,
        headers: &HeaderMap,
        owner: &str,
        name: &str,
    ) -> ApiResult<(OpenRepo, Principal, Identity)> {
        let identity = identify(&self.state, self.req(headers)).await?;
        let (repo, principal) = open_visible(&self.state, Some(&identity), owner, name).await?;
        meets(Some(&identity), &repo, &principal, self.need)?;
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
        let identity = identify_optional(&self.state, self.req(headers)).await?;
        let (repo, principal) = open_visible(&self.state, identity.as_ref(), owner, name).await?;
        meets(identity.as_ref(), &repo, &principal, self.need)?;
        Ok((repo, principal))
    }
}

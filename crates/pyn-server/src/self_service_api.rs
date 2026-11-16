//! The caller's own email addresses and username (#982), the moved-address header and current author names.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use pyn_core::{AccountEmail, PynError, UserId};
use pyn_proto as api;

use crate::{ApiResult, AppState, identify};

fn email_dto(e: AccountEmail) -> api::AccountEmail {
    api::AccountEmail {
        address: e.address,
        verified: e.verified_at.is_some(),
        verified_at: e.verified_at,
        primary: e.primary,
        added_at: e.added_at,
    }
}

#[utoipa::path(get, path = "/v1/me/emails", responses(
    (status = 200, body = Vec<api::AccountEmail>, description = "the caller's addresses, the primary first"),
))]
pub(crate) async fn list_emails(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::AccountEmail>>> {
    let who = identify(&s, &headers).await?;
    let held = s.access.list_emails(&who).await?;
    Ok(Json(held.into_iter().map(email_dto).collect()))
}

#[utoipa::path(post, path = "/v1/me/emails", request_body = api::AddEmailRequest, responses(
    (status = 201, body = api::AccountEmail, description = "added unverified; a link was mailed"),
    (status = 400, body = api::ErrorBody, description = "not a valid address"),
    (status = 409, body = api::ErrorBody, description = "email_unavailable (held by or pending for another account, never says whose), or email_refused"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
))]
pub(crate) async fn add_email(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::AddEmailRequest>,
) -> ApiResult<(StatusCode, Json<api::AccountEmail>)> {
    let who = identify(&s, &headers).await?;
    s.access.add_email(&who, &req.address).await?;
    let address = pyn_core::account::normalize_email(&req.address)?;
    let added = s
        .access
        .list_emails(&who)
        .await?
        .into_iter()
        .find(|e| e.address == address)
        .ok_or(PynError::EmailNotFound)?;
    Ok((StatusCode::CREATED, Json(email_dto(added))))
}

#[utoipa::path(post, path = "/v1/me/emails/{address}/resend", params(("address" = String, Path, description = "an unverified address of the caller")), responses(
    (status = 202),
    (status = 404, body = api::ErrorBody, description = "email_not_found (not held, or already verified)"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
))]
pub(crate) async fn resend_email(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(address): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access.resend_email(&who, &address).await?;
    Ok(StatusCode::ACCEPTED)
}

#[utoipa::path(put, path = "/v1/me/emails/{address}/primary", params(("address" = String, Path, description = "a verified address of the caller")), responses(
    (status = 204),
    (status = 404, body = api::ErrorBody, description = "email_not_found"),
    (status = 409, body = api::ErrorBody, description = "email_refused: the address is not verified"),
))]
pub(crate) async fn set_primary_email(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(address): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access.set_primary_email(&who, &address).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/me/emails/{address}", params(("address" = String, Path, description = "an address of the caller")), responses(
    (status = 204),
    (status = 404, body = api::ErrorBody, description = "email_not_found"),
    (status = 409, body = api::ErrorBody, description = "email_refused: the last verified address, on a server that requires one"),
))]
pub(crate) async fn delete_email(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(address): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access.remove_email(&who, &address).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/v1/me/username", request_body = api::RenameRequest, responses(
    (status = 200, body = api::Renamed, description = "renamed; the old name is free for anyone, and each repository's old address redirects until a live repository takes it"),
    (status = 400, body = api::ErrorBody, description = "invalid_request or reserved_name"),
    (status = 401, body = api::ErrorBody, description = "the password is wrong"),
    (status = 403, body = api::ErrorBody, description = "owner_frozen"),
    (status = 409, body = api::ErrorBody, description = "user_exists: a user or organization holds the name"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
))]
pub(crate) async fn rename_account(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::RenameRequest>,
) -> ApiResult<Json<api::Renamed>> {
    let who = identify(&s, &headers).await?;
    let done = s
        .access
        .rename_account(&who, &req.username, req.password.as_deref())
        .await?;
    Ok(Json(api::Renamed {
        old_name: done.old.to_string(),
        new_name: done.new.to_string(),
        repositories: done.repositories,
    }))
}

/// Adds `X-Pyn-Moved-To: owner/name` to a successful response for a repository address that only a rename keeps alive.
pub(crate) async fn moved_to_header(
    State(s): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let address = repo_address(request.uri().path());
    let mut response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    if let Some((owner, name)) = address
        && let Ok(Some(moved)) = s.repos.moved_to(&owner, &name).await
        && let Ok(value) = HeaderValue::from_str(&moved)
    {
        response.headers_mut().insert(api::MOVED_TO_HEADER, value);
    }
    response
}

fn repo_address(path: &str) -> Option<(String, String)> {
    let rest = path.strip_prefix("/v1/repos/")?;
    let mut parts = rest.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty()).then(|| (owner.to_string(), name.to_string()))
}

/// A response item that names an account at a point in time and can carry that account's current name.
pub(crate) trait Named {
    fn written(&self) -> (UserId, chrono::DateTime<chrono::Utc>);
    fn set_current(&mut self, now: Option<String>);
    fn set_public(&mut self, who: Option<api::PublicIdentity>);
}

/// A response item for one account, named by `user`, that carries that account's public identity.
pub(crate) trait Identified {
    fn user(&self) -> UserId;
    fn set_identity(&mut self, who: Option<api::PublicIdentity>);
}

impl Identified for api::Lock {
    fn user(&self) -> UserId {
        UserId::new(&self.owner)
    }
    fn set_identity(&mut self, who: Option<api::PublicIdentity>) {
        self.identity = who;
    }
}

impl Identified for api::Member {
    fn user(&self) -> UserId {
        UserId::new(&self.user)
    }
    fn set_identity(&mut self, who: Option<api::PublicIdentity>) {
        self.identity = who;
    }
}

impl Identified for api::OrgMember {
    fn user(&self) -> UserId {
        UserId::new(&self.user)
    }
    fn set_identity(&mut self, who: Option<api::PublicIdentity>) {
        self.identity = who;
    }
}

impl Named for api::Revision {
    fn written(&self) -> (UserId, chrono::DateTime<chrono::Utc>) {
        (UserId::new(&self.author), self.created_at)
    }
    fn set_current(&mut self, now: Option<String>) {
        self.author_current = now;
    }
    fn set_public(&mut self, who: Option<api::PublicIdentity>) {
        self.author_public = who;
    }
}

impl Named for api::StrandedVersion {
    fn written(&self) -> (UserId, chrono::DateTime<chrono::Utc>) {
        (UserId::new(&self.author), self.created_at)
    }
    fn set_current(&mut self, now: Option<String>) {
        self.author_current = now;
    }
    fn set_public(&mut self, who: Option<api::PublicIdentity>) {
        self.author_public = who;
    }
}

impl Named for api::Commit {
    fn written(&self) -> (UserId, chrono::DateTime<chrono::Utc>) {
        (
            UserId::new(&self.author.name),
            self.authored_at.unwrap_or(self.created_at),
        )
    }
    fn set_current(&mut self, now: Option<String>) {
        self.author_current = now;
    }
    fn set_public(&mut self, who: Option<api::PublicIdentity>) {
        self.author_public = who;
    }
}

impl Named for api::ActivityEntry {
    fn written(&self) -> (UserId, chrono::DateTime<chrono::Utc>) {
        (UserId::new(&self.actor), self.at)
    }
    fn set_current(&mut self, now: Option<String>) {
        self.actor_current = now;
    }
    fn set_public(&mut self, who: Option<api::PublicIdentity>) {
        self.actor_public = who;
    }
}

impl Named for api::AuditEntry {
    fn written(&self) -> (UserId, chrono::DateTime<chrono::Utc>) {
        (UserId::new(&self.actor), self.at)
    }
    fn set_current(&mut self, now: Option<String>) {
        self.actor_current = now;
    }
    fn set_public(&mut self, who: Option<api::PublicIdentity>) {
        self.actor_public = who;
    }
}

/// The public identity of each account, one lookup per distinct name; `None` where no active account holds it.
pub(crate) async fn identities(
    s: &AppState,
    users: &[UserId],
) -> ApiResult<Vec<Option<api::PublicIdentity>>> {
    let mut distinct: Vec<UserId> = Vec::new();
    for u in users {
        if !distinct.contains(u) {
            distinct.push(u.clone());
        }
    }
    let found = s.access.public_profiles(&distinct).await?;
    let known: std::collections::HashMap<_, _> = distinct
        .into_iter()
        .zip(found.into_iter().map(|f| {
            f.map(|(user, profile)| {
                let p = crate::profile_api::profile_dto(&user, profile, false);
                api::PublicIdentity {
                    user: user.to_string(),
                    display_name: p.display_name,
                    avatar_url: p.avatar_url,
                }
            })
        }))
        .collect();
    Ok(users.iter().map(|u| known[u].clone()).collect())
}

/// Fills the public identity of every item's account.
pub(crate) async fn annotate_identities<T: Identified>(
    s: &AppState,
    items: &mut [T],
) -> ApiResult<()> {
    let users: Vec<_> = items.iter().map(Identified::user).collect();
    for (item, who) in items.iter_mut().zip(identities(s, &users).await?) {
        item.set_identity(who);
    }
    Ok(())
}

/// For each author name and time, the renamed current name (decision #986) and the public identity of the account.
async fn resolve(
    s: &AppState,
    asked: &[(UserId, chrono::DateTime<chrono::Utc>)],
) -> ApiResult<Vec<(Option<String>, Option<api::PublicIdentity>)>> {
    let names = s.access.current_names(asked).await?;
    let users: Vec<_> = asked
        .iter()
        .zip(&names)
        .map(|((written, _), now)| now.clone().unwrap_or_else(|| written.clone()))
        .collect();
    let who = identities(s, &users).await?;
    Ok(names
        .into_iter()
        .map(|n| n.map(|n| n.to_string()))
        .zip(who)
        .collect())
}

/// Fills the current name and public identity on every item; the one resolver all history surfaces share.
pub(crate) async fn annotate<T: Named>(s: &AppState, items: &mut [T]) -> ApiResult<()> {
    let asked: Vec<_> = items.iter().map(Named::written).collect();
    for (item, (now, who)) in items.iter_mut().zip(resolve(s, &asked).await?) {
        item.set_current(now);
        item.set_public(who);
    }
    Ok(())
}

/// Like `annotate`, for revisions and the stranded versions they report.
pub(crate) async fn annotate_revisions(
    s: &AppState,
    revisions: &mut [api::Revision],
) -> ApiResult<()> {
    annotate(s, revisions).await?;
    for r in revisions.iter_mut().filter(|r| !r.stranded.is_empty()) {
        annotate(s, &mut r.stranded).await?;
    }
    Ok(())
}

/// Like `annotate_revisions`, for commits and the revisions inside them.
pub(crate) async fn annotate_commits(s: &AppState, commits: &mut [api::Commit]) -> ApiResult<()> {
    annotate(s, commits).await?;
    for c in commits.iter_mut() {
        annotate_revisions(s, &mut c.revisions).await?;
    }
    Ok(())
}

/// Like `annotate`, for each tree entry's last change and lock holder.
pub(crate) async fn annotate_tree(s: &AppState, entries: &mut [api::TreeEntry]) -> ApiResult<()> {
    let asked: Vec<_> = entries
        .iter()
        .filter_map(|e| e.last_change.as_ref())
        .map(Named::written)
        .collect();
    let mut found = resolve(s, &asked).await?.into_iter();
    for r in entries.iter_mut().filter_map(|e| e.last_change.as_mut()) {
        let (now, who) = found.next().unwrap_or_default();
        r.set_current(now);
        r.set_public(who);
    }
    annotate_held(s, entries.iter_mut().filter_map(|e| e.lock.as_mut())).await
}

/// Fills the public identity of each lock holder.
pub(crate) async fn annotate_held<'a>(
    s: &AppState,
    locks: impl Iterator<Item = &'a mut api::Lock>,
) -> ApiResult<()> {
    let mut locks: Vec<_> = locks.collect();
    let users: Vec<_> = locks.iter().map(|l| l.user()).collect();
    for (l, who) in locks.iter_mut().zip(identities(s, &users).await?) {
        l.set_identity(who);
    }
    Ok(())
}

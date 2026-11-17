//! Account profiles: the public page of a user, the signed-in user's own edits, and avatars.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pyn_core::profiles::Profiles;
use pyn_core::{Avatar, OrgProfile, Profile, ProfileFields, ProfileVisibility, PynError, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::caller::{AnyCaller, FullScopeCaller, OptionalCaller};
use crate::{ApiResult, AppState, FileContent};

const AVATAR_MAX: u64 = pyn_core::profile::MAX_AVATAR_BYTES;

pub(crate) fn profiles(s: &AppState) -> Profiles<'_> {
    Profiles {
        access: &s.access,
        repos: &s.repos,
        objects: &s.objects,
    }
}

fn visibility_dto(v: ProfileVisibility) -> api::ProfileVisibility {
    api::ProfileVisibility {
        display_name: v.display_name,
        bio: v.bio,
        location: v.location,
        company: v.company,
        links: v.links,
        avatar: v.avatar,
    }
}

/// The wire form of a profile; `own` adds its visibility settings. The avatar URL carries the start of the
/// image's hash so a new image is a new URL.
pub(crate) fn profile_dto(user: &UserId, profile: Profile, own: bool) -> api::Profile {
    let visibility = own.then(|| visibility_dto(profile.visibility));
    let f = profile.fields;
    api::Profile {
        display_name: f.display_name,
        bio: f.bio,
        location: f.location,
        company: f.company,
        links: f.links,
        avatar_url: profile
            .avatar
            .map(|a| format!("/v1/users/{user}/avatar?v={}", &a.content.to_hex()[..16])),
        visibility,
    }
}

#[utoipa::path(get, path = "/v1/users/{user}",
    params(("user" = String, Path, description = "the account's name")),
    responses((status = 200, body = api::PublicProfile, description = "readable signed out; never includes an email address, hides the fields the user made private (everyone but the user sees them absent, an administrator included), and lists only the repositories and organizations the caller may see"),
              (status = 401, body = api::ErrorBody, description = "credentials were sent but are not valid"),
              (status = 404, body = api::ErrorBody, description = "user_not_found: no such user, or an organization, or an account that is disabled or not yet approved")))]
pub(crate) async fn get_profile(
    State(s): State<AppState>,
    OptionalCaller(who): OptionalCaller,
    Path(user): Path<String>,
) -> ApiResult<Json<api::PublicProfile>> {
    let found = profiles(&s).public(&user, who.as_ref()).await?;
    let mut repositories = Vec::with_capacity(found.repositories.len());
    for (record, role) in found.repositories {
        let limit = s.repos.lock_limit(&record).await?;
        repositories.push(crate::repo_api::repo_info(&s, record, role, limit).await?);
    }
    let organizations = found
        .organizations
        .into_iter()
        .map(|(org, role)| crate::org_api::org_info(org, Some(role)))
        .collect();
    Ok(Json(api::PublicProfile {
        id: found.id.to_string(),
        user: found.user.to_string(),
        created_at: found.created_at,
        repository_count: repositories.len() as u64,
        profile: profile_dto(&found.user, found.profile, found.own),
        repositories,
        organizations,
    }))
}

#[derive(Deserialize)]
pub(crate) struct PeopleQuery {
    q: Option<String>,
    after: Option<String>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/users",
    params(("q" = Option<String>, Query, description = "keeps accounts whose name or visible display name contains this text, ignoring case; at most 64 characters"),
           ("after" = Option<String>, Query, description = "return accounts after this account name"),
           ("limit" = Option<usize>, Query, description = "page size, default 30, max 100")),
    responses((status = 200, body = api::PeoplePage, description = "active user accounts by name; never an email address, a hidden field, or an account that is disabled, not yet approved, scheduled for deletion or an organization"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: a search over 64 characters"),
              (status = 401, body = api::ErrorBody, description = "sign-in required"),
              (status = 403, body = api::ErrorBody, description = "forbidden for a token that is not full-scope, service_credential_not_allowed for a service credential")))]
pub(crate) async fn list_people(
    State(s): State<AppState>,
    _: FullScopeCaller,
    Query(q): Query<PeopleQuery>,
) -> ApiResult<Json<api::PeoplePage>> {
    let limit = q.limit.unwrap_or(30).clamp(1, 100);
    let after = q.after.map(UserId::new);
    let (found, next_after) = s
        .access
        .people(q.q.as_deref(), after.as_ref(), limit)
        .await?;
    let people = found
        .into_iter()
        .map(|(user, profile)| {
            let p = profile_dto(&user, profile, false);
            api::PersonEntry {
                identity: api::PublicIdentity {
                    user: user.to_string(),
                    display_name: p.display_name,
                    avatar_url: p.avatar_url,
                },
                bio: p.bio,
                location: p.location,
                company: p.company,
                links: p.links,
            }
        })
        .collect();
    Ok(Json(api::PeoplePage { people, next_after }))
}

#[utoipa::path(put, path = "/v1/me/profile", request_body = api::UpdateProfileRequest,
    responses((status = 200, body = api::Profile, description = "the stored profile"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: a field is too long, has control characters, or a link is not an http(s) URL"),
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope")))]
pub(crate) async fn update_profile(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
    Json(req): Json<api::UpdateProfileRequest>,
) -> ApiResult<Json<api::Profile>> {
    let fields = ProfileFields {
        display_name: req.display_name,
        bio: req.bio,
        location: req.location,
        company: req.company,
        links: req.links,
    };
    let visibility = req.visibility.map(|v| ProfileVisibility {
        display_name: v.display_name,
        bio: v.bio,
        location: v.location,
        company: v.company,
        links: v.links,
        avatar: v.avatar,
    });
    let saved = s.access.update_profile(&who, fields, visibility).await?;
    Ok(Json(profile_dto(&who.user, saved, true)))
}

/// The bytes the client sends as the avatar, refused early when they declare or hold more than the limit.
async fn avatar_bytes(headers: &HeaderMap, body: Body) -> Result<Vec<u8>, PynError> {
    let too_large = PynError::ObjectTooLarge { limit: AVATAR_MAX };
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > AVATAR_MAX) {
        return Err(too_large);
    }
    let limit = AVATAR_MAX as usize;
    let bytes = axum::body::to_bytes(body, limit)
        .await
        .map_err(|_| too_large)?;
    Ok(bytes.to_vec())
}

#[utoipa::path(put, path = "/v1/me/avatar",
    request_body(content = inline(FileContent), content_type = "image/png", description = "the image bytes (png, jpeg or webp, at most 1 MiB); the type is judged from the bytes, not the `Content-Type` header"),
    responses((status = 200, body = api::Profile, description = "the profile with the new `avatar_url`"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: an empty body"),
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope"),
              (status = 413, body = api::ErrorBody, description = "object_too_large: over 1 MiB"),
              (status = 415, body = api::ErrorBody, description = "unsupported_image: not a png, jpeg or webp"),
              (status = 429, body = api::ErrorBody, description = "too_many_attempts: too many avatar changes in an hour")))]
pub(crate) async fn put_avatar(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    body: Body,
) -> ApiResult<Json<api::Profile>> {
    let bytes = avatar_bytes(&headers, body).await?;
    let saved = profiles(&s).set_avatar(&who, bytes).await?;
    Ok(Json(profile_dto(&who.user, saved, true)))
}

#[utoipa::path(delete, path = "/v1/me/avatar",
    responses((status = 200, body = api::Profile, description = "the profile without an avatar; removing none is not an error"),
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope")))]
pub(crate) async fn delete_avatar(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
) -> ApiResult<Json<api::Profile>> {
    let saved = profiles(&s).remove_avatar(&who).await?;
    Ok(Json(profile_dto(&who.user, saved, true)))
}

#[derive(Deserialize)]
pub(crate) struct AvatarQuery {
    v: Option<String>,
}

#[utoipa::path(get, path = "/v1/users/{user}/avatar",
    params(("user" = String, Path, description = "the account's name"),
           ("v" = Option<String>, Query, description = "the version from `avatar_url`; when it is the current one the image is cached for a year")),
    responses((status = 200, content_type = "image/png", body = inline(FileContent), description = "the image, as `image/png`, `image/jpeg` or `image/webp` according to its bytes; readable signed out, with an `ETag` and `Cache-Control`"),
              (status = 304, description = "`If-None-Match` matches the current image"),
              (status = 401, body = api::ErrorBody, description = "credentials were sent but are not valid"),
              (status = 404, body = api::ErrorBody, description = "user_not_found, or avatar_not_found when the user has none or hides it from the caller")))]
pub(crate) async fn get_avatar(
    State(s): State<AppState>,
    headers: HeaderMap,
    OptionalCaller(who): OptionalCaller,
    Path(user): Path<String>,
    Query(q): Query<AvatarQuery>,
) -> ApiResult<Response> {
    let (avatar, bytes) = profiles(&s).avatar(&user, who.as_ref()).await?;
    Ok(avatar_response(&headers, q.v.as_deref(), &avatar, bytes))
}

/// The avatar image with its type, `ETag` and cache headers; a matching `If-None-Match` gets a `304`.
fn avatar_response(
    headers: &HeaderMap,
    version: Option<&str>,
    avatar: &Avatar,
    bytes: Vec<u8>,
) -> Response {
    let hash = avatar.content.to_hex();
    let etag = format!("\"{hash}\"");
    let current = version.is_some_and(|v| v.len() >= 8 && hash.starts_with(v));
    let cache = if current {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=300, must-revalidate"
    };
    let unchanged = headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag));
    let mut res = if unchanged {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut res = Body::from(bytes).into_response();
        res.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_static(avatar.kind.media_type()),
        );
        res
    };
    let h = res.headers_mut();
    h.insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(ETAG, HeaderValue::from_str(&etag).expect("hex"));
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        "content-security-policy",
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    res
}

fn org_profile_dto(org: &UserId, p: OrgProfile) -> api::OrgProfile {
    api::OrgProfile {
        name: org.to_string(),
        description: p.description,
        avatar_url: org_avatar_url(org, p.avatar.as_ref()),
    }
}

/// The avatar address of an organization, carrying the start of the image's hash.
pub(crate) fn org_avatar_url(org: &UserId, avatar: Option<&Avatar>) -> Option<String> {
    avatar.map(|a| format!("/v1/orgs/{org}/avatar?v={}", &a.content.to_hex()[..16]))
}

#[utoipa::path(get, path = "/v1/orgs/{org}/profile",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = api::OrgProfile, description = "any signed-in account can read it, as it can look the organization up; it holds nothing specific to the caller"),
              (status = 404, body = api::ErrorBody, description = "org_not_found (also for a user name)")))]
pub(crate) async fn get_org_profile(
    State(s): State<AppState>,
    _: AnyCaller,
    Path(org): Path<String>,
) -> ApiResult<Json<api::OrgProfile>> {
    let found = profiles(&s).org_profile(&org).await?;
    Ok(Json(org_profile_dto(&UserId::new(org), found)))
}

#[utoipa::path(put, path = "/v1/orgs/{org}/profile", request_body = api::UpdateOrgProfileRequest,
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = api::OrgProfile, description = "the stored profile"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: the description is too long or has control characters"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token that is not full-scope"),
              (status = 404, body = api::ErrorBody, description = "org_not_found"),
              (status = 409, body = api::ErrorBody, description = "owner_frozen")))]
pub(crate) async fn update_org_profile(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
    Path(org): Path<String>,
    Json(req): Json<api::UpdateOrgProfileRequest>,
) -> ApiResult<Json<api::OrgProfile>> {
    let org = UserId::new(org);
    let saved = s
        .access
        .update_org_description(&who, &org, req.description)
        .await?;
    Ok(Json(org_profile_dto(&org, saved)))
}

#[utoipa::path(put, path = "/v1/orgs/{org}/avatar",
    params(("org" = String, Path, description = "the organization's name")),
    request_body(content = inline(FileContent), content_type = "image/png", description = "the image bytes (png, jpeg or webp, at most 1 MiB); the type is judged from the bytes, not the `Content-Type` header"),
    responses((status = 200, body = api::OrgProfile, description = "the profile with the new `avatar_url`"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: an empty body"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token that is not full-scope"),
              (status = 404, body = api::ErrorBody, description = "org_not_found"),
              (status = 409, body = api::ErrorBody, description = "owner_frozen"),
              (status = 413, body = api::ErrorBody, description = "object_too_large: over 1 MiB"),
              (status = 415, body = api::ErrorBody, description = "unsupported_image: not a png, jpeg or webp"),
              (status = 429, body = api::ErrorBody, description = "too_many_attempts: too many avatar changes in an hour")))]
pub(crate) async fn put_org_avatar(
    State(s): State<AppState>,
    headers: HeaderMap,
    FullScopeCaller(who): FullScopeCaller,
    Path(org): Path<String>,
    body: Body,
) -> ApiResult<Json<api::OrgProfile>> {
    let bytes = avatar_bytes(&headers, body).await?;
    let saved = profiles(&s).set_org_avatar(&who, &org, bytes).await?;
    Ok(Json(org_profile_dto(&UserId::new(org), saved)))
}

#[utoipa::path(delete, path = "/v1/orgs/{org}/avatar",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = api::OrgProfile, description = "the profile without an avatar; removing none is not an error"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token that is not full-scope"),
              (status = 404, body = api::ErrorBody, description = "org_not_found"),
              (status = 409, body = api::ErrorBody, description = "owner_frozen")))]
pub(crate) async fn delete_org_avatar(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
    Path(org): Path<String>,
) -> ApiResult<Json<api::OrgProfile>> {
    let saved = profiles(&s).remove_org_avatar(&who, &org).await?;
    Ok(Json(org_profile_dto(&UserId::new(org), saved)))
}

#[utoipa::path(get, path = "/v1/orgs/{org}/avatar",
    params(("org" = String, Path, description = "the organization's name"),
           ("v" = Option<String>, Query, description = "the version from `avatar_url`; when it is the current one the image is cached for a year")),
    responses((status = 200, content_type = "image/png", body = inline(FileContent), description = "the image, as `image/png`, `image/jpeg` or `image/webp` according to its bytes; readable signed out, with an `ETag` and `Cache-Control`"),
              (status = 304, description = "`If-None-Match` matches the current image"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, or avatar_not_found when it has none")))]
pub(crate) async fn get_org_avatar(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Query(q): Query<AvatarQuery>,
) -> ApiResult<Response> {
    let (avatar, bytes) = profiles(&s).org_avatar(&org).await?;
    Ok(avatar_response(&headers, q.v.as_deref(), &avatar, bytes))
}

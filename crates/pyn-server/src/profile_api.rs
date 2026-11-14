//! Account profiles: the public page of a user, the signed-in user's own edits, and avatars.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pyn_core::profiles::Profiles;
use pyn_core::{Profile, ProfileFields, PynError, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::{ApiResult, AppState, FileContent, identify, identify_optional};

const AVATAR_MAX: u64 = pyn_core::profile::MAX_AVATAR_BYTES;

pub(crate) fn profiles(s: &AppState) -> Profiles<'_> {
    Profiles {
        access: &s.access,
        repos: &s.repos,
        objects: &s.objects,
    }
}

/// The wire form of a profile. The avatar URL carries the start of the image's hash so a new image is a new URL.
pub(crate) fn profile_dto(user: &UserId, profile: Profile) -> api::Profile {
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
    }
}

#[utoipa::path(get, path = "/v1/users/{user}",
    params(("user" = String, Path, description = "the account's name")),
    responses((status = 200, body = api::PublicProfile, description = "readable signed out; never includes an email address, and lists only the repositories and organizations the caller may see"),
              (status = 401, body = api::ErrorBody, description = "credentials were sent but are not valid"),
              (status = 404, body = api::ErrorBody, description = "user_not_found: no such user, or an organization, or an account that is disabled or not yet approved")))]
pub(crate) async fn get_profile(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> ApiResult<Json<api::PublicProfile>> {
    let who = identify_optional(&s, &headers).await?;
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
        profile: profile_dto(&found.user, found.profile),
        repositories,
        organizations,
    }))
}

#[utoipa::path(put, path = "/v1/me/profile", request_body = api::UpdateProfileRequest,
    responses((status = 200, body = api::Profile, description = "the stored profile"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: a field is too long, has control characters, or a link is not an http(s) URL"),
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope")))]
pub(crate) async fn update_profile(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::UpdateProfileRequest>,
) -> ApiResult<Json<api::Profile>> {
    let who = identify(&s, &headers).await?;
    let fields = ProfileFields {
        display_name: req.display_name,
        bio: req.bio,
        location: req.location,
        company: req.company,
        links: req.links,
    };
    let saved = s.access.update_profile(&who, fields).await?;
    Ok(Json(profile_dto(&who.user, saved)))
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
    body: Body,
) -> ApiResult<Json<api::Profile>> {
    let who = identify(&s, &headers).await?;
    let bytes = avatar_bytes(&headers, body).await?;
    let saved = profiles(&s).set_avatar(&who, bytes).await?;
    Ok(Json(profile_dto(&who.user, saved)))
}

#[utoipa::path(delete, path = "/v1/me/avatar",
    responses((status = 200, body = api::Profile, description = "the profile without an avatar; removing none is not an error"),
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope")))]
pub(crate) async fn delete_avatar(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<api::Profile>> {
    let who = identify(&s, &headers).await?;
    let saved = profiles(&s).remove_avatar(&who).await?;
    Ok(Json(profile_dto(&who.user, saved)))
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
              (status = 404, body = api::ErrorBody, description = "user_not_found, or avatar_not_found when the user has none")))]
pub(crate) async fn get_avatar(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
    Query(q): Query<AvatarQuery>,
) -> ApiResult<Response> {
    let (avatar, bytes) = profiles(&s).avatar(&user).await?;
    let hash = avatar.content.to_hex();
    let etag = format!("\"{hash}\"");
    let current =
        q.v.as_deref()
            .is_some_and(|v| v.len() >= 8 && hash.starts_with(v));
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
    Ok(res)
}

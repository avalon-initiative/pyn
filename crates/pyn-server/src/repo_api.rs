//! Repository registry and the repository-scoped access endpoints (members, roles, invitations).

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Duration;
use pyn_core::{
    AccountStatus, InviteId, InviteRecord, LockLimit, Permission, RepoRecord, RepoSettings,
    RepoUpdate, Role, UserId,
};
use pyn_proto as api;
use serde::Deserialize;

use crate::access_api::names;
use crate::{
    ApiResult, AppState, RepoAddress, authorize_repo, identify, identify_optional, open_visible,
};

fn visibility_dto(v: pyn_core::Visibility) -> api::Visibility {
    match v {
        pyn_core::Visibility::Public => api::Visibility::Public,
        pyn_core::Visibility::Private => api::Visibility::Private,
    }
}

fn visibility_from(v: api::Visibility) -> pyn_core::Visibility {
    match v {
        api::Visibility::Public => pyn_core::Visibility::Public,
        api::Visibility::Private => pyn_core::Visibility::Private,
    }
}

fn repo_info(r: RepoRecord, role: Option<Role>, limit: LockLimit) -> api::RepoInfo {
    api::RepoInfo {
        owner: r.owner.to_string(),
        name: r.name,
        visibility: visibility_dto(r.visibility),
        lease_hours: r.settings.lease_hours,
        max_locks_per_user: limit.max,
        max_locks_per_user_setting: r.settings.max_locks,
        max_locks_set_by_policy: limit.from_policy,
        created_at: r.created_at,
        role: role.map(|r| r.to_string()),
    }
}

#[derive(Deserialize)]
pub(crate) struct ListReposQuery {
    owner: Option<String>,
}

#[utoipa::path(get, path = "/v1/repos",
    params(("owner" = Option<String>, Query, description = "only this owner's repositories")),
    responses((status = 200, body = Vec<api::RepoInfo>, description = "the repositories the caller has a role in; public repositories without a role are read by address"),
              (status = 401, body = api::ErrorBody)))]
pub(crate) async fn list_repos(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListReposQuery>,
) -> ApiResult<Json<Vec<api::RepoInfo>>> {
    let who = identify(&s, &headers).await?;
    let owner = q.owner.map(UserId::new);
    let repos = s.repos.list(&who, owner.as_ref()).await?;
    let mut infos = Vec::with_capacity(repos.len());
    for (record, role) in repos {
        let limit = s.repos.lock_limit(&record).await?;
        infos.push(repo_info(record, role, limit));
    }
    Ok(Json(infos))
}

#[utoipa::path(get, path = "/v1/me/locks",
    responses((status = 200, body = Vec<api::MyLock>, description = "live locks in repositories the caller can read, ordered by repository then path"),
              (status = 401, body = api::ErrorBody)))]
pub(crate) async fn my_locks(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::MyLock>>> {
    let who = identify(&s, &headers).await?;
    let locks = s.repos.locks_of(&who).await?;
    Ok(Json(
        locks
            .into_iter()
            .map(|(r, l)| api::MyLock {
                owner: r.owner.to_string(),
                name: r.name,
                path: l.path.to_string(),
                acquired_at: l.acquired_at,
                expires_at: l.expires_at,
            })
            .collect(),
    ))
}

#[utoipa::path(post, path = "/v1/repos", request_body = api::CreateRepoRequest, responses(
    (status = 201, body = api::RepoInfo, description = "the caller becomes its admin"),
    (status = 400, body = api::ErrorBody, description = "invalid_repo_name or invalid_request (a lease or lock limit out of range)"),
    (status = 403, body = api::ErrorBody, description = "not_namespace_owner; in an organization not_org_member or repo_create_forbidden (the policy refuses that visibility); or a token without manage_roles or limited to repositories"),
    (status = 409, body = api::ErrorBody, description = "repo_exists, or org_deleting: the organization is being deleted"),
))]
pub(crate) async fn create_repo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CreateRepoRequest>,
) -> ApiResult<(StatusCode, Json<api::RepoInfo>)> {
    let who = identify(&s, &headers).await?;
    let owner = req.owner.map_or_else(|| who.user.clone(), UserId::new);
    let settings = (req.lease_hours.is_some() || req.max_locks_per_user.is_some()).then(|| {
        let defaults = RepoSettings::default();
        RepoSettings {
            lease_hours: req.lease_hours.unwrap_or(defaults.lease_hours),
            max_locks: req.max_locks_per_user,
        }
    });
    let record = s
        .repos
        .create(
            &who,
            &owner,
            &req.name,
            req.visibility.map(visibility_from),
            settings,
        )
        .await?;
    let limit = s.repos.lock_limit(&record).await?;
    Ok((
        StatusCode::CREATED,
        Json(repo_info(record, Some(Role::Admin), limit)),
    ))
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}", params(RepoAddress), responses(
    (status = 200, body = api::RepoInfo),
    (status = 404, body = api::ErrorBody, description = "repo_not_found"),
))]
pub(crate) async fn get_repo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<api::RepoInfo>> {
    let who = identify_optional(&s, &headers).await?;
    let (open, _) = open_visible(&s, who.as_ref(), &owner, &name).await?;
    let role = match &who {
        Some(who) => s.access.role_in(&open.record.id, &who.user).await?,
        None => None,
    };
    let limit = open.service.lock_limit();
    Ok(Json(repo_info(open.record, role, limit)))
}

#[utoipa::path(patch, path = "/v1/repos/{owner}/{name}", params(RepoAddress), request_body = api::UpdateRepoRequest, responses(
    (status = 200, body = api::RepoInfo),
    (status = 400, body = api::ErrorBody, description = "invalid_request, including a lock limit change while the policy file sets one"),
    (status = 403, body = api::ErrorBody, description = "only the owner, with admin rights in the repository"),
    (status = 404, body = api::ErrorBody, description = "repo_not_found"),
    (status = 409, body = api::ErrorBody, description = "repo_exists"),
))]
pub(crate) async fn update_repo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::UpdateRepoRequest>,
) -> ApiResult<Json<api::RepoInfo>> {
    let who = identify(&s, &headers).await?;
    let (open, _) = open_visible(&s, Some(&who), &owner, &name).await?;
    let update = RepoUpdate {
        name: req.name,
        visibility: req.visibility.map(visibility_from),
        settings: (req.lease_hours.is_some() || req.max_locks_per_user.is_some()).then(|| {
            let current = open.record.settings;
            RepoSettings {
                lease_hours: req.lease_hours.unwrap_or(current.lease_hours),
                max_locks: req.max_locks_per_user.unwrap_or(current.max_locks),
            }
        }),
    };
    let record = s.repos.update(&who, &open.record, update).await?;
    let role = s.access.role_in(&record.id, &who.user).await?;
    let limit = s.repos.lock_limit(&record).await?;
    Ok(Json(repo_info(record, role, limit)))
}

#[utoipa::path(delete, path = "/v1/repos/{owner}/{name}", params(RepoAddress), responses(
    (status = 204, description = "removes the repository's files, history, locks and access; its audit log is kept"),
    (status = 403, body = api::ErrorBody, description = "only the owner, with admin rights in the repository"),
    (status = 404, body = api::ErrorBody, description = "repo_not_found"),
))]
pub(crate) async fn delete_repo(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    let (open, _) = open_visible(&s, Some(&who), &owner, &name).await?;
    s.repos.delete(&who, &open.record).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/me", params(RepoAddress), responses(
    (status = 200, body = api::Me),
    (status = 404, body = api::ErrorBody, description = "repo_not_found"),
))]
pub(crate) async fn repo_me(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<api::Me>> {
    let (_, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    Ok(Json(api::Me {
        user: who.user.to_string(),
        permissions: names(&who.permissions),
    }))
}

fn parse_permissions(
    names: &[String],
) -> Result<std::collections::BTreeSet<Permission>, pyn_core::PynError> {
    names.iter().map(|n| Permission::from_str(n)).collect()
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/roles", params(RepoAddress),
    responses((status = 200, body = Vec<api::RoleGrant>)))]
pub(crate) async fn list_roles(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<api::RoleGrant>>> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    let defs = s.access.role_definitions(&repo.record.id).await?;
    let grants = Role::ALL
        .into_iter()
        .map(|role| api::RoleGrant {
            role: role.to_string(),
            permissions: names(defs.get(role)),
        })
        .collect();
    Ok(Json(grants))
}

#[utoipa::path(put, path = "/v1/repos/{owner}/{name}/roles/{role}", params(RepoAddress), request_body = api::SetRoleRequest, responses(
    (status = 204),
    (status = 403, body = api::ErrorBody, description = "needs manage_roles"),
))]
pub(crate) async fn set_role(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, role)): Path<(String, String, String)>,
    Json(req): Json<api::SetRoleRequest>,
) -> ApiResult<StatusCode> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    let permissions = parse_permissions(&req.permissions)?;
    s.access
        .set_role_permissions(&who, &repo.record.id, Role::from_str(&role)?, permissions)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/members", params(RepoAddress),
    responses((status = 200, body = Vec<api::Member>, description = "everyone with access, one row each: the effective role and its source (direct, team or org_owner); needs manage_users")))]
pub(crate) async fn list_members(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<api::Member>>> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    let members = s.access.members(&who, &repo.record.id).await?;
    Ok(Json(
        members
            .into_iter()
            .map(|m| api::Member {
                user: m.user.to_string(),
                role: m.role.to_string(),
                source: m.source.to_string(),
            })
            .collect(),
    ))
}

#[utoipa::path(put, path = "/v1/repos/{owner}/{name}/members/{user}", params(RepoAddress), request_body = api::SetMemberRequest, responses(
    (status = 204),
    (status = 403, body = api::ErrorBody, description = "needs manage_users and every permission the role grants"),
))]
pub(crate) async fn set_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, user)): Path<(String, String, String)>,
    Json(req): Json<api::SetMemberRequest>,
) -> ApiResult<StatusCode> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    s.access
        .set_user_role(
            &who,
            &repo.record.id,
            &UserId::new(user),
            Role::from_str(&req.role)?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn invite_info(i: InviteRecord) -> api::InviteInfo {
    api::InviteInfo {
        id: i.id.to_string(),
        role: i.role.to_string(),
        created_by: i.created_by.to_string(),
        created_at: i.created_at,
        expires_at: i.expires_at,
        used_at: i.used_at,
        used_by: i.used_by.map(|u| u.to_string()),
        revoked_at: i.revoked_at,
    }
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/users", params(RepoAddress), request_body = api::AddUserRequest, responses(
    (status = 201, body = api::Registered),
    (status = 403, body = api::ErrorBody, description = "needs manage_users"),
    (status = 409, body = api::ErrorBody, description = "user_exists"),
))]
pub(crate) async fn add_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::AddUserRequest>,
) -> ApiResult<(StatusCode, Json<api::Registered>)> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    let role = Role::from_str(&req.role)?;
    let user = s
        .access
        .add_user(&who, &repo.record.id, &req.username, &req.password, role)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::Registered {
            user: user.to_string(),
            status: AccountStatus::Active.to_string(),
        }),
    ))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/invites", params(RepoAddress), request_body = api::CreateInviteRequest, responses(
    (status = 200, body = api::CreatedInvite),
    (status = 403, body = api::ErrorBody, description = "needs manage_users and every permission the role grants"),
))]
pub(crate) async fn create_invite(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::CreateInviteRequest>,
) -> ApiResult<Json<api::CreatedInvite>> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    let role = Role::from_str(&req.role)?;
    let (record, code) = s
        .access
        .create_invite(&who, &repo.record.id, role, Duration::hours(req.hours))
        .await?;
    Ok(Json(api::CreatedInvite {
        code,
        info: invite_info(record),
    }))
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/invites", params(RepoAddress),
    responses((status = 200, body = Vec<api::InviteInfo>)))]
pub(crate) async fn list_invites(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<api::InviteInfo>>> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    let invites = s.access.list_invites(&who, &repo.record.id).await?;
    Ok(Json(invites.into_iter().map(invite_info).collect()))
}

#[utoipa::path(delete, path = "/v1/repos/{owner}/{name}/invites/{id}", params(RepoAddress),
    responses((status = 204), (status = 400, body = api::ErrorBody)))]
pub(crate) async fn revoke_invite(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, id)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    s.access
        .revoke_invite(&who, &repo.record.id, &InviteId(id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

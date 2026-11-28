//! Server invitations, accounts an administrator adds, and resending a confirmation. Server administrators only.

use std::str::FromStr;

use crate::json::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use pyn_core::{
    InvitationGrant, InviteId, NewServerInvitation, OrgRole, PynError, Role, ServerInvitation,
    UserId,
};
use pyn_proto as api;

use crate::caller::AdminCaller;
use crate::{ApiResult, AppState};

fn status(i: &ServerInvitation, now: chrono::DateTime<chrono::Utc>) -> api::ServerInvitationStatus {
    if i.used_at.is_some() {
        api::ServerInvitationStatus::Used
    } else if i.revoked_at.is_some() {
        api::ServerInvitationStatus::Revoked
    } else if i.expires_at <= now {
        api::ServerInvitationStatus::Expired
    } else {
        api::ServerInvitationStatus::Pending
    }
}

async fn invitation_info(
    s: &AppState,
    i: ServerInvitation,
) -> ApiResult<api::ServerInvitationInfo> {
    let grant = match &i.grant {
        Some(InvitationGrant::Repository { repo, role }) => Some(api::ServerInvitationGrant {
            repository: Some(s.repos.address_of(repo).await?),
            organization: None,
            role: role.as_str().to_string(),
        }),
        Some(InvitationGrant::Organization { org, role }) => Some(api::ServerInvitationGrant {
            repository: None,
            organization: Some(org.to_string()),
            role: role.to_string(),
        }),
        None => None,
    };
    Ok(api::ServerInvitationInfo {
        id: i.id.to_string(),
        status: status(&i, s.access.now()),
        email: i.email,
        grant,
        created_by: i.created_by.to_string(),
        created_at: i.created_at,
        expires_at: i.expires_at,
        used_at: i.used_at,
        used_by: i.used_by.map(|u| u.to_string()),
        revoked_at: i.revoked_at,
    })
}

async fn grant_from(
    s: &AppState,
    g: api::ServerInvitationGrant,
) -> Result<InvitationGrant, PynError> {
    match (g.repository, g.organization) {
        (Some(address), None) => {
            let (owner, name) = address.split_once('/').ok_or_else(|| {
                PynError::InvalidRequest("repository is written owner/name".into())
            })?;
            let repo = s.repos.open(owner, name).await?.record.id;
            Ok(InvitationGrant::Repository {
                repo,
                role: Role::from_str(&g.role)?,
            })
        }
        (None, Some(org)) => Ok(InvitationGrant::Organization {
            org: UserId::new(org),
            role: OrgRole::from_str(&g.role).map_err(|_| {
                PynError::InvalidRequest(format!("unknown role {:?}; use owner or member", g.role))
            })?,
        }),
        _ => Err(PynError::InvalidRequest(
            "a grant names a repository or an organization, not both and not neither".into(),
        )),
    }
}

#[utoipa::path(post, path = "/v1/admin/invitations", operation_id = "create_server_invitation",
    request_body = api::CreateServerInvitationRequest, responses(
    (status = 201, body = api::CreatedServerInvitation, description = "the code is shown once; `emailed` says whether a mailer took the message"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: a bad address, an expiry outside 1 to 90 days, an unknown role, or a grant naming both or neither of repository and organization"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required: also for any service credential or narrower token"),
    (status = 404, body = api::ErrorBody, description = "repo_not_found or org_not_found for the grant's target"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts: too many messages to that address this hour"),
))]
pub(crate) async fn create_invitation(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
    Json(req): Json<api::CreateServerInvitationRequest>,
) -> ApiResult<(StatusCode, Json<api::CreatedServerInvitation>)> {
    let grant = match req.grant {
        Some(g) => Some(grant_from(&s, g).await?),
        None => None,
    };
    let created = s
        .access
        .create_server_invitation(
            &who,
            NewServerInvitation {
                email: req.email,
                days: req.expires_in_days.map(i64::from),
                grant,
            },
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::CreatedServerInvitation {
            code: created.code,
            emailed: created.emailed,
            invitation: invitation_info(&s, created.invitation).await?,
        }),
    ))
}

#[utoipa::path(get, path = "/v1/admin/invitations", operation_id = "list_server_invitations",
    responses((status = 200, body = Vec<api::ServerInvitationInfo>, description = "newest first; never with a code"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required")))]
pub(crate) async fn list_invitations(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
) -> ApiResult<Json<Vec<api::ServerInvitationInfo>>> {
    let mut out = Vec::new();
    for i in s.access.list_server_invitations(&who).await? {
        out.push(invitation_info(&s, i).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(delete, path = "/v1/admin/invitations/{id}", operation_id = "revoke_server_invitation",
    params(("id" = String, Path, description = "the invitation's id")),
    responses((status = 204, description = "the code stops working; revoking twice is harmless"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: the invitation was already used"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required"),
              (status = 404, body = api::ErrorBody, description = "server_invitation_not_found")))]
pub(crate) async fn revoke_invitation(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    s.access
        .revoke_server_invitation(&who, &InviteId(id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/admin/users", operation_id = "add_account",
    request_body = api::AddAccountRequest, responses(
    (status = 201, body = api::AccountInfo, description = "an active account whose password is one-time: its first sign-in answers `password_change_required`"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: a bad name or a password under 10 characters"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required"),
    (status = 409, body = api::ErrorBody, description = "user_exists, or name_reserved"),
))]
pub(crate) async fn add_account(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
    Json(req): Json<api::AddAccountRequest>,
) -> ApiResult<(StatusCode, Json<api::AccountInfo>)> {
    let account = s
        .access
        .admin_add_account(&who, &req.username, &req.password)
        .await?;
    Ok((StatusCode::CREATED, Json(crate::admin_api::info(account))))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/resend-confirmation", operation_id = "resend_confirmation",
    params(("user" = String, Path, description = "the account's user name")),
    responses(
    (status = 202, description = "a fresh link was sent; a failed delivery is only logged"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
    (status = 409, body = api::ErrorBody, description = "email_already_confirmed, or email_verification_off when the server does not verify addresses"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts: three messages to one address per hour"),
))]
pub(crate) async fn resend_confirmation(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
    Path(user): Path<String>,
) -> ApiResult<StatusCode> {
    s.access
        .admin_resend_confirmation(&who, &UserId::new(user))
        .await?;
    Ok(StatusCode::ACCEPTED)
}

//! Webhook subscriptions: administrators and service credentials with `manage_webhooks`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{CreateWebhook, SubscriptionUpdate, WebhookDetail};
use pyn_proto as api;

use crate::{ApiResult, AppState, identify_admin};

fn webhook_dto(d: WebhookDetail) -> api::Webhook {
    let s = d.subscription;
    api::Webhook {
        id: s.id,
        name: s.name,
        url: s.url,
        event_types: s.event_types,
        enabled: s.enabled,
        created_at: s.created_at,
        updated_at: s.updated_at,
        created_by: s.created_by.to_string(),
        secrets: d
            .secrets
            .into_iter()
            .map(|x| api::WebhookSecretInfo {
                id: x.id,
                created_at: x.created_at,
                encrypted: x.encrypted,
            })
            .collect(),
    }
}

#[utoipa::path(post, path = "/v1/admin/webhooks", operation_id = "create_webhook", request_body = api::CreateWebhookRequest, responses(
    (status = 201, body = api::CreatedWebhook, description = "the signing secret is shown once"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: bad name, url (https only, no user info or fragment) or event type"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
))]
pub(crate) async fn create_webhook(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CreateWebhookRequest>,
) -> ApiResult<(StatusCode, Json<api::CreatedWebhook>)> {
    let who = identify_admin(&s, &headers).await?;
    let (detail, issued) = s
        .access
        .create_webhook(
            &who,
            CreateWebhook {
                name: req.name,
                url: req.url,
                event_types: req.event_types,
                enabled: req.enabled,
            },
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::CreatedWebhook {
            webhook: webhook_dto(detail),
            secret: issued.value,
        }),
    ))
}

#[utoipa::path(get, path = "/v1/admin/webhooks", operation_id = "list_webhooks",
    responses((status = 200, body = Vec<api::Webhook>, description = "oldest first; secret values are never returned"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks")))]
pub(crate) async fn list_webhooks(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::Webhook>>> {
    let who = identify_admin(&s, &headers).await?;
    let all = s.access.list_webhooks(&who).await?;
    Ok(Json(all.into_iter().map(webhook_dto).collect()))
}

#[utoipa::path(get, path = "/v1/admin/webhooks/{id}", operation_id = "get_webhook",
    params(("id" = i64, Path, description = "the subscription's id")),
    responses((status = 200, body = api::Webhook),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
              (status = 404, body = api::ErrorBody, description = "webhook_not_found")))]
pub(crate) async fn get_webhook(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<Json<api::Webhook>> {
    let who = identify_admin(&s, &headers).await?;
    Ok(Json(webhook_dto(s.access.get_webhook(&who, id).await?)))
}

#[utoipa::path(patch, path = "/v1/admin/webhooks/{id}", operation_id = "update_webhook",
    params(("id" = i64, Path, description = "the subscription's id")),
    request_body = api::UpdateWebhookRequest, responses(
    (status = 200, body = api::Webhook),
    (status = 400, body = api::ErrorBody, description = "invalid_request: nothing to change, or a bad name, url or event type"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
    (status = 404, body = api::ErrorBody, description = "webhook_not_found"),
))]
pub(crate) async fn update_webhook(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(req): Json<api::UpdateWebhookRequest>,
) -> ApiResult<Json<api::Webhook>> {
    let who = identify_admin(&s, &headers).await?;
    let update = SubscriptionUpdate {
        name: req.name,
        url: req.url,
        event_types: req.event_types,
        enabled: req.enabled,
    };
    Ok(Json(webhook_dto(
        s.access.update_webhook(&who, id, update).await?,
    )))
}

#[utoipa::path(delete, path = "/v1/admin/webhooks/{id}", operation_id = "delete_webhook",
    params(("id" = i64, Path, description = "the subscription's id")),
    responses((status = 204, description = "its secrets and deliveries are deleted with it"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
              (status = 404, body = api::ErrorBody, description = "webhook_not_found")))]
pub(crate) async fn delete_webhook(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    let who = identify_admin(&s, &headers).await?;
    s.access.delete_webhook(&who, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/admin/webhooks/{id}/secrets", operation_id = "rotate_webhook_secret",
    params(("id" = i64, Path, description = "the subscription's id")),
    responses((status = 201, body = api::RotatedWebhookSecret, description = "adds a second active secret, shown once; deliveries carry a signature for each active secret until the old one is retired"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
              (status = 404, body = api::ErrorBody, description = "webhook_not_found"),
              (status = 409, body = api::ErrorBody, description = "webhook_secret_limit: two secrets are already active; retire one first")))]
pub(crate) async fn rotate_webhook_secret(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<api::RotatedWebhookSecret>)> {
    let who = identify_admin(&s, &headers).await?;
    let issued = s.access.rotate_webhook_secret(&who, id).await?;
    Ok((
        StatusCode::CREATED,
        Json(api::RotatedWebhookSecret {
            secret_id: issued.info.id,
            created_at: issued.info.created_at,
            secret: issued.value,
        }),
    ))
}

#[utoipa::path(delete, path = "/v1/admin/webhooks/{id}/secrets/{secret_id}", operation_id = "retire_webhook_secret",
    params(("id" = i64, Path, description = "the subscription's id"),
           ("secret_id" = i64, Path, description = "an active secret of that subscription")),
    responses((status = 204, description = "the secret stops signing deliveries"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
              (status = 404, body = api::ErrorBody, description = "webhook_not_found, or webhook_secret_not_found when it is not an active secret of that subscription"),
              (status = 409, body = api::ErrorBody, description = "webhook_last_secret: rotate in a new secret first")))]
pub(crate) async fn retire_webhook_secret(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((id, secret_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    let who = identify_admin(&s, &headers).await?;
    s.access.retire_webhook_secret(&who, id, secret_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

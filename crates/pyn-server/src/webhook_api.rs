//! Webhook subscriptions: administrators and service credentials with `manage_webhooks`.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use pyn_core::{
    CreateWebhook, Delivery, DeliveryStatus, PynError, SubscriptionUpdate, WebhookDetail,
};
use pyn_proto as api;
use serde::Deserialize;

use crate::caller::ServiceCaller;
use crate::{ApiResult, AppState};

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

/// Refuses a type outside the closed set; the core only checks the form.
fn check_known_types(types: &[String]) -> Result<(), PynError> {
    match types
        .iter()
        .find(|t| !api::KNOWN_EVENT_TYPES.contains(&t.as_str()))
    {
        Some(t) => Err(PynError::InvalidRequest(format!(
            "unknown event type {t:?}: list them with GET /v1/admin/webhooks/event-types"
        ))),
        None => Ok(()),
    }
}

#[utoipa::path(get, path = "/v1/admin/webhooks/event-types", operation_id = "list_webhook_event_types",
    responses((status = 200, body = Vec<api::WebhookEventTypeInfo>, description = "every type a subscription may list, in the documented order, with a one-line description"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks")))]
pub(crate) async fn list_webhook_event_types(
    ServiceCaller(_who): ServiceCaller,
) -> Json<Vec<api::WebhookEventTypeInfo>> {
    Json(
        api::KNOWN_EVENT_TYPES
            .iter()
            .map(|t| api::WebhookEventTypeInfo {
                name: (*t).to_string(),
                description: api::event_type_description(t)
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect(),
    )
}

#[utoipa::path(post, path = "/v1/admin/webhooks", operation_id = "create_webhook", request_body = api::CreateWebhookRequest, responses(
    (status = 201, body = api::CreatedWebhook, description = "the signing secret is shown once"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: bad name, url (https only, no user info or fragment) or an event type outside the known set"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
))]
pub(crate) async fn create_webhook(
    State(s): State<AppState>,
    ServiceCaller(who): ServiceCaller,
    Json(req): Json<api::CreateWebhookRequest>,
) -> ApiResult<(StatusCode, Json<api::CreatedWebhook>)> {
    check_known_types(&req.event_types)?;
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
    ServiceCaller(who): ServiceCaller,
) -> ApiResult<Json<Vec<api::Webhook>>> {
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
    ServiceCaller(who): ServiceCaller,
    Path(id): Path<i64>,
) -> ApiResult<Json<api::Webhook>> {
    Ok(Json(webhook_dto(s.access.get_webhook(&who, id).await?)))
}

#[utoipa::path(patch, path = "/v1/admin/webhooks/{id}", operation_id = "update_webhook",
    params(("id" = i64, Path, description = "the subscription's id")),
    request_body = api::UpdateWebhookRequest, responses(
    (status = 200, body = api::Webhook),
    (status = 400, body = api::ErrorBody, description = "invalid_request: nothing to change, or a bad name, url or an event type outside the known set"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
    (status = 404, body = api::ErrorBody, description = "webhook_not_found"),
))]
pub(crate) async fn update_webhook(
    State(s): State<AppState>,
    ServiceCaller(who): ServiceCaller,
    Path(id): Path<i64>,
    Json(req): Json<api::UpdateWebhookRequest>,
) -> ApiResult<Json<api::Webhook>> {
    check_known_types(req.event_types.as_deref().unwrap_or_default())?;
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
    ServiceCaller(who): ServiceCaller,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
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
    ServiceCaller(who): ServiceCaller,
    Path(id): Path<i64>,
) -> ApiResult<(StatusCode, Json<api::RotatedWebhookSecret>)> {
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
    ServiceCaller(who): ServiceCaller,
    Path((id, secret_id)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    s.access.retire_webhook_secret(&who, id, secret_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn delivery_dto(d: Delivery) -> api::WebhookDelivery {
    api::WebhookDelivery {
        id: d.id,
        event_id: d.event_id,
        status: match d.status {
            DeliveryStatus::Pending => api::WebhookDeliveryStatus::Pending,
            DeliveryStatus::Delivered => api::WebhookDeliveryStatus::Delivered,
            DeliveryStatus::Failed => api::WebhookDeliveryStatus::Failed,
        },
        attempts: d.attempt_count,
        next_attempt_at: d.next_attempt_at,
        last_status_code: d.last_status_code,
        last_error: d.last_error,
        created_at: d.created_at,
        delivered_at: d.delivered_at,
        failed_at: d.failed_at,
    }
}

#[derive(Deserialize)]
pub(crate) struct DeliveryParams {
    status: Option<String>,
    before: Option<i64>,
    limit: Option<u32>,
}

#[utoipa::path(get, path = "/v1/admin/webhooks/{id}/deliveries", operation_id = "list_webhook_deliveries",
    params(("id" = i64, Path, description = "the subscription's id"),
           ("status" = Option<api::WebhookDeliveryStatus>, Query, description = "only deliveries in this status"),
           ("before" = Option<i64>, Query, description = "deliveries of events older than this event id"),
           ("limit" = Option<u32>, Query, description = "page size, default 50, max 200")),
    responses((status = 200, body = api::WebhookDeliveryPage, description = "newest event first; deliveries are kept 30 days"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: unknown status"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
              (status = 404, body = api::ErrorBody, description = "webhook_not_found")))]
pub(crate) async fn list_webhook_deliveries(
    State(s): State<AppState>,
    ServiceCaller(who): ServiceCaller,
    Path(id): Path<i64>,
    Query(q): Query<DeliveryParams>,
) -> ApiResult<Json<api::WebhookDeliveryPage>> {
    let status = q
        .status
        .map(|v| {
            v.parse::<DeliveryStatus>().map_err(|_| {
                PynError::InvalidRequest(format!(
                    "unknown status {v}: use pending, delivered or failed"
                ))
            })
        })
        .transpose()?;
    let page = s
        .access
        .list_webhook_deliveries(&who, id, status, q.before, q.limit)
        .await?;
    Ok(Json(api::WebhookDeliveryPage {
        deliveries: page.deliveries.into_iter().map(delivery_dto).collect(),
        next_before: page.next_before,
    }))
}

#[utoipa::path(post, path = "/v1/admin/webhooks/{id}/deliveries/{delivery_id}/redeliver", operation_id = "redeliver_webhook",
    params(("id" = i64, Path, description = "the subscription's id"),
           ("delivery_id" = String, Path, description = "the delivery's id, as sent in `Pyn-Delivery`")),
    responses((status = 200, body = api::WebhookDelivery, description = "back to pending with no attempts and due now; the delivery id is unchanged so receivers can dedupe"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required, or service_scope_required for a service credential without manage_webhooks"),
              (status = 404, body = api::ErrorBody, description = "webhook_not_found, or webhook_delivery_not_found when it is not a delivery of that subscription")))]
pub(crate) async fn redeliver_webhook(
    State(s): State<AppState>,
    ServiceCaller(who): ServiceCaller,
    Path((id, delivery_id)): Path<(i64, String)>,
) -> ApiResult<Json<api::WebhookDelivery>> {
    let d = s.access.redeliver_webhook(&who, id, &delivery_id).await?;
    Ok(Json(delivery_dto(d)))
}

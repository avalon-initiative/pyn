//! The HTTP contract of webhook subscription administration: access, secrets shown once, validation, audit.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, RegistrationMode, Repositories, Rules, SecretKey,
    SystemClock, UserId, WebhookSettings,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

async fn server(settings: WebhookSettings) -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone())
        .with_webhooks(Arc::new(MemoryWebhookStore::new()), settings),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    ));
    router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    })
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.json::<api::ErrorBody>().code == code
    }
}

/// `who` is a dev-header user, a bearer secret, or empty for no credential.
async fn send(app: &Router, who: &str, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let builder = if who.is_empty() {
        builder
    } else if who.starts_with("pyn_") || who.starts_with("pyns_") {
        builder.header("authorization", format!("Bearer {who}"))
    } else {
        builder.header(api::DEV_USER_HEADER, who)
    };
    let req = builder
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let r = send(app, "root", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(app, "root", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

async fn service(app: &Router, name: &str, scopes: &[&str]) -> String {
    let r = send(
        app,
        "root",
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": name, "scopes": scopes})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    r.json::<api::CreatedServiceCredential>().secret
}

async fn create(app: &Router, who: &str) -> api::CreatedWebhook {
    let r = send(
        app,
        who,
        "POST",
        "/v1/admin/webhooks",
        Some(json!({"name": "ci", "url": "https://example.com/hook", "event_types": ["repository.created"]})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.json()
}

#[tokio::test]
async fn the_lifecycle_works_over_http_and_the_secret_is_shown_once() {
    let app = server(WebhookSettings::default()).await;
    let made = create(&app, "root").await;
    assert!(made.secret.starts_with("whsec_") && made.secret.len() == 6 + 64);
    let id = made.webhook.id;
    assert!(made.webhook.enabled);
    assert_eq!(made.webhook.secrets.len(), 1);
    assert_eq!(made.webhook.created_by, "root");

    for (method, uri) in [
        ("GET", "/v1/admin/webhooks".to_string()),
        ("GET", format!("/v1/admin/webhooks/{id}")),
    ] {
        let r = send(&app, "root", method, &uri, None).await;
        assert_eq!(r.status, StatusCode::OK);
        assert!(!String::from_utf8_lossy(&r.body).contains(&made.secret));
    }

    let r = send(
        &app,
        "root",
        "PATCH",
        &format!("/v1/admin/webhooks/{id}"),
        Some(json!({"name": "renamed", "enabled": false, "event_types": []})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let w = r.json::<api::Webhook>();
    assert_eq!((w.name.as_str(), w.enabled), ("renamed", false));
    assert!(w.event_types.is_empty());
    assert_eq!(w.url, "https://example.com/hook");

    let r = send(
        &app,
        "root",
        "PATCH",
        &format!("/v1/admin/webhooks/{id}"),
        Some(json!({})),
    )
    .await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));

    let r = send(
        &app,
        "root",
        "DELETE",
        &format!("/v1/admin/webhooks/{id}"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(
        &app,
        "root",
        "GET",
        &format!("/v1/admin/webhooks/{id}"),
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "webhook_not_found"));
    let r = send(
        &app,
        "root",
        "DELETE",
        &format!("/v1/admin/webhooks/{id}"),
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "webhook_not_found"));
}

#[tokio::test]
async fn rotation_keeps_two_secrets_refuses_a_third_and_retires_one() {
    let app = server(WebhookSettings::default()).await;
    let made = create(&app, "root").await;
    let id = made.webhook.id;
    let first = made.webhook.secrets[0].id;

    let r = send(
        &app,
        "root",
        "POST",
        &format!("/v1/admin/webhooks/{id}/secrets"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let rotated = r.json::<api::RotatedWebhookSecret>();
    assert_ne!(rotated.secret, made.secret);
    let r = send(
        &app,
        "root",
        "GET",
        &format!("/v1/admin/webhooks/{id}"),
        None,
    )
    .await;
    let w = r.json::<api::Webhook>();
    assert_eq!(w.secrets.len(), 2);
    assert!(!String::from_utf8_lossy(&r.body).contains(&rotated.secret));

    let r = send(
        &app,
        "root",
        "POST",
        &format!("/v1/admin/webhooks/{id}/secrets"),
        None,
    )
    .await;
    assert!(r.is(StatusCode::CONFLICT, "webhook_secret_limit"));

    let r = send(
        &app,
        "root",
        "DELETE",
        &format!("/v1/admin/webhooks/{id}/secrets/{first}"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(
        &app,
        "root",
        "DELETE",
        &format!("/v1/admin/webhooks/{id}/secrets/{first}"),
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "webhook_secret_not_found"));
    let r = send(
        &app,
        "root",
        "DELETE",
        &format!("/v1/admin/webhooks/{id}/secrets/{}", rotated.secret_id),
        None,
    )
    .await;
    assert!(r.is(StatusCode::CONFLICT, "webhook_last_secret"));
    let r = send(&app, "root", "POST", "/v1/admin/webhooks/999/secrets", None).await;
    assert!(r.is(StatusCode::NOT_FOUND, "webhook_not_found"));
}

#[tokio::test]
async fn access_follows_the_scope_rules() {
    let app = server(WebhookSettings::default()).await;
    let alice = token_for(&app, "alice").await;
    let wrong = service(&app, "limits", &["manage_limits"]).await;
    let scoped = service(&app, "hooks", &["manage_webhooks"]).await;
    let body = json!({"name": "ci", "url": "https://example.com/hook"});

    let r = send(&app, "", "GET", "/v1/admin/webhooks", None).await;
    assert!(r.is(StatusCode::UNAUTHORIZED, "unauthenticated"));
    let r = send(&app, "", "POST", "/v1/admin/webhooks", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    for who in [&alice] {
        let r = send(&app, who, "GET", "/v1/admin/webhooks", None).await;
        assert!(
            r.is(StatusCode::FORBIDDEN, "server_admin_required"),
            "{who}"
        );
        let r = send(&app, who, "POST", "/v1/admin/webhooks", Some(body.clone())).await;
        assert!(
            r.is(StatusCode::FORBIDDEN, "server_admin_required"),
            "{who}"
        );
    }

    let r = send(&app, &wrong, "GET", "/v1/admin/webhooks", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = send(
        &app,
        &wrong,
        "POST",
        "/v1/admin/webhooks",
        Some(body.clone()),
    )
    .await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = send(&app, &wrong, "DELETE", "/v1/admin/webhooks/1", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));

    let made = create(&app, &scoped).await;
    let r = send(&app, &scoped, "GET", "/v1/admin/webhooks", None).await;
    assert_eq!(r.json::<Vec<api::Webhook>>().len(), 1);
    let r = send(
        &app,
        &scoped,
        "POST",
        &format!("/v1/admin/webhooks/{}/secrets", made.webhook.id),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(&app, &scoped, "GET", "/v1/admin/users", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
}

#[tokio::test]
async fn urls_are_validated_and_http_needs_the_operator_setting() {
    let app = server(WebhookSettings::default()).await;
    for url in [
        "http://example.com/hook",
        "https://u:p@example.com/hook",
        "https://example.com/hook#frag",
        "ftp://example.com/",
        "nonsense",
    ] {
        let r = send(
            &app,
            "root",
            "POST",
            "/v1/admin/webhooks",
            Some(json!({"name": "ci", "url": url})),
        )
        .await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"), "{url}");
    }
    let r = send(
        &app,
        "root",
        "POST",
        "/v1/admin/webhooks",
        Some(json!({"name": "ci", "url": "https://example.com/", "event_types": ["Bad Type"]})),
    )
    .await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    let made = create(&app, "root").await;
    let r = send(
        &app,
        "root",
        "PATCH",
        &format!("/v1/admin/webhooks/{}", made.webhook.id),
        Some(json!({"url": "http://example.com/"})),
    )
    .await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));

    let lax = server(WebhookSettings {
        allow_http: true,
        ..WebhookSettings::default()
    })
    .await;
    let r = send(
        &lax,
        "root",
        "POST",
        "/v1/admin/webhooks",
        Some(json!({"name": "dev", "url": "http://localhost:9000/hook"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
}

#[tokio::test]
async fn changes_reach_the_server_audit_log_without_secrets() {
    let app = server(WebhookSettings {
        secret_key: Some(SecretKey::from_hex(KEY).unwrap()),
        allow_http: false,
    })
    .await;
    let made = create(&app, "root").await;
    assert!(made.webhook.secrets[0].encrypted);
    let id = made.webhook.id;
    let r = send(
        &app,
        "root",
        "POST",
        &format!("/v1/admin/webhooks/{id}/secrets"),
        None,
    )
    .await;
    let rotated = r.json::<api::RotatedWebhookSecret>();
    send(
        &app,
        "root",
        "PATCH",
        &format!("/v1/admin/webhooks/{id}"),
        Some(json!({"enabled": false})),
    )
    .await;
    send(
        &app,
        "root",
        "DELETE",
        &format!("/v1/admin/webhooks/{id}"),
        None,
    )
    .await;

    let r = send(&app, "root", "GET", "/v1/admin/audit?limit=100", None).await;
    let text = String::from_utf8_lossy(&r.body).to_string();
    let page = r.json::<api::AuditPage>();
    let actions: Vec<&str> = page
        .entries
        .iter()
        .map(|e| e.action.as_str())
        .filter(|a| a.starts_with("webhook_"))
        .collect();
    assert_eq!(
        actions,
        [
            "webhook_deleted",
            "webhook_updated",
            "webhook_secret_rotated",
            "webhook_created"
        ]
    );
    assert!(!text.contains(&made.secret) && !text.contains(&rotated.secret));
}

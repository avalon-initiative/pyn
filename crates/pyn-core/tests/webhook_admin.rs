//! Webhook subscription administration on the memory store: scope, secrets, validation and audit.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryWebhookStore};
use pyn_core::webhook::WebhookStore;
use pyn_core::{
    AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, CreateWebhook, Credential,
    Identity, ManualClock, PynError, SecretKey, ServiceScope, SubscriptionUpdate, UserId,
    WebhookSettings, reveal_secret,
};

const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

struct World {
    access: Arc<AccessService>,
    audit: Arc<MemoryAuditStore>,
    store: Arc<MemoryWebhookStore>,
}

fn world_with(settings: WebhookSettings) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 20, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let store = Arc::new(MemoryWebhookStore::new());
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock)
            .with_audit(audit.clone())
            .with_webhooks(store.clone(), settings),
    );
    World {
        access,
        audit,
        store,
    }
}

fn world() -> World {
    world_with(WebhookSettings::default())
}

async fn admin(w: &World) -> Identity {
    let root = UserId::new("root");
    w.access.admin_for_tests(&root).await.unwrap();
    Identity {
        user: root,
        credential: Credential::Session,
    }
}

async fn service(w: &World, name: &str, scopes: &[ServiceScope]) -> Identity {
    let root = admin(w).await;
    let (_, secret) = w
        .access
        .create_service_credential(&root, name, scopes.iter().copied().collect::<BTreeSet<_>>())
        .await
        .unwrap();
    w.access.identify(&secret).await.unwrap()
}

fn req(url: &str) -> CreateWebhook {
    CreateWebhook {
        name: "billing".into(),
        url: url.into(),
        event_types: vec!["repository.created".into()],
        enabled: true,
    }
}

#[tokio::test]
async fn an_administrator_creates_lists_updates_and_deletes() {
    let w = world();
    let root = admin(&w).await;
    let (detail, issued) = w
        .access
        .create_webhook(&root, req("https://example.com/hook"))
        .await
        .unwrap();
    let id = detail.subscription.id;
    assert_eq!(detail.secrets.len(), 1);
    assert_eq!(detail.secrets[0].id, issued.info.id);
    assert!(issued.value.starts_with("whsec_"));

    let listed = w.access.list_webhooks(&root).await.unwrap();
    assert_eq!(listed, vec![detail.clone()]);

    let updated = w
        .access
        .update_webhook(
            &root,
            id,
            SubscriptionUpdate {
                url: Some("https://example.org/other".into()),
                enabled: Some(false),
                event_types: Some(vec![]),
                ..SubscriptionUpdate::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.subscription.url, "https://example.org/other");
    assert!(!updated.subscription.enabled);
    assert!(updated.subscription.event_types.is_empty());
    assert_eq!(updated.subscription.name, "billing");

    w.access.delete_webhook(&root, id).await.unwrap();
    assert!(matches!(
        w.access.get_webhook(&root, id).await,
        Err(PynError::WebhookNotFound(_))
    ));
    assert!(matches!(
        w.access.delete_webhook(&root, id).await,
        Err(PynError::WebhookNotFound(_))
    ));
}

#[tokio::test]
async fn an_update_with_nothing_to_change_is_refused() {
    let w = world();
    let root = admin(&w).await;
    let (d, _) = w
        .access
        .create_webhook(&root, req("https://example.com/hook"))
        .await
        .unwrap();
    let err = w
        .access
        .update_webhook(&root, d.subscription.id, SubscriptionUpdate::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)));
}

#[tokio::test]
async fn only_administrators_and_the_manage_webhooks_scope_may_manage() {
    let w = world();
    let user = Identity {
        user: UserId::new("alice"),
        credential: Credential::Session,
    };
    assert!(matches!(
        w.access.list_webhooks(&user).await,
        Err(PynError::ServerAdminRequired)
    ));
    let wrong = service(&w, "wrong", &[ServiceScope::ManageLimits]).await;
    for result in [
        w.access
            .create_webhook(&wrong, req("https://example.com/"))
            .await
            .map(|_| ()),
        w.access.list_webhooks(&wrong).await.map(|_| ()),
        w.access.delete_webhook(&wrong, 1).await,
    ] {
        assert!(matches!(
            result,
            Err(PynError::ServiceScopeRequired(ServiceScope::ManageWebhooks))
        ));
    }
    let right = service(&w, "hooks", &[ServiceScope::ManageWebhooks]).await;
    w.access
        .create_webhook(&right, req("https://example.com/"))
        .await
        .unwrap();
    assert_eq!(w.access.list_webhooks(&right).await.unwrap().len(), 1);
}

#[tokio::test]
async fn urls_must_be_https_unless_the_operator_allows_http() {
    let w = world();
    let root = admin(&w).await;
    for bad in [
        "http://example.com/",
        "https://user:pw@example.com/",
        "https://example.com/#x",
        "not a url",
    ] {
        let err = w.access.create_webhook(&root, req(bad)).await.unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad}");
    }
    assert!(w.access.list_webhooks(&root).await.unwrap().is_empty());

    let lax = world_with(WebhookSettings {
        allow_http: true,
        ..WebhookSettings::default()
    });
    let root = admin(&lax).await;
    lax.access
        .create_webhook(&root, req("http://localhost:9000/hook"))
        .await
        .unwrap();
    let (d, _) = lax
        .access
        .create_webhook(&root, req("https://example.com/"))
        .await
        .unwrap();
    let err = lax
        .access
        .update_webhook(
            &root,
            d.subscription.id,
            SubscriptionUpdate {
                url: Some("ftp://example.com/".into()),
                ..SubscriptionUpdate::default()
            },
        )
        .await;
    assert!(matches!(err, Err(PynError::InvalidRequest(_))));
}

#[tokio::test]
async fn rotation_allows_two_active_secrets_and_a_third_is_refused() {
    let w = world();
    let root = admin(&w).await;
    let (d, first) = w
        .access
        .create_webhook(&root, req("https://example.com/hook"))
        .await
        .unwrap();
    let id = d.subscription.id;
    let second = w.access.rotate_webhook_secret(&root, id).await.unwrap();
    assert_ne!(first.value, second.value);
    assert_eq!(
        w.access.get_webhook(&root, id).await.unwrap().secrets.len(),
        2
    );

    let err = w.access.rotate_webhook_secret(&root, id).await.unwrap_err();
    assert!(matches!(err, PynError::WebhookSecretLimit(2)));

    w.access
        .retire_webhook_secret(&root, id, first.info.id)
        .await
        .unwrap();
    let left = w.access.get_webhook(&root, id).await.unwrap().secrets;
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, second.info.id);

    assert!(matches!(
        w.access
            .retire_webhook_secret(&root, id, first.info.id)
            .await,
        Err(PynError::WebhookSecretNotFound { .. })
    ));
    assert!(matches!(
        w.access
            .retire_webhook_secret(&root, id, second.info.id)
            .await,
        Err(PynError::WebhookLastSecret)
    ));
    assert!(matches!(
        w.access.rotate_webhook_secret(&root, 999).await,
        Err(PynError::WebhookNotFound(999))
    ));
    w.access.rotate_webhook_secret(&root, id).await.unwrap();
}

#[tokio::test]
async fn secrets_are_stored_plain_without_a_key_and_encrypted_with_one() {
    let plain = world();
    let root = admin(&plain).await;
    let (d, issued) = plain
        .access
        .create_webhook(&root, req("https://example.com/hook"))
        .await
        .unwrap();
    assert!(!d.secrets[0].encrypted);
    let stored = plain
        .store
        .list_active_secrets(d.subscription.id)
        .await
        .unwrap();
    assert!(!stored[0].encrypted && stored[0].key_id.is_none());
    assert_eq!(
        reveal_secret(None, &stored[0]).unwrap(),
        issued.value.as_bytes()
    );

    let key = SecretKey::from_hex(KEY).unwrap();
    let enc = world_with(WebhookSettings {
        secret_key: Some(key.clone()),
        allow_http: false,
    });
    let root = admin(&enc).await;
    let (d, issued) = enc
        .access
        .create_webhook(&root, req("https://example.com/hook"))
        .await
        .unwrap();
    let rotated = enc
        .access
        .rotate_webhook_secret(&root, d.subscription.id)
        .await
        .unwrap();
    let stored = enc
        .store
        .list_active_secrets(d.subscription.id)
        .await
        .unwrap();
    assert!(stored.iter().all(|s| s.encrypted));
    assert!(
        stored
            .iter()
            .all(|s| s.key_id.as_deref() == Some(key.key_id()))
    );
    for (row, value) in stored.iter().zip([&issued.value, &rotated.value]) {
        assert!(
            !row.secret
                .windows(value.len())
                .any(|w| w == value.as_bytes())
        );
        assert_eq!(reveal_secret(Some(&key), row).unwrap(), value.as_bytes());
    }
    assert!(reveal_secret(None, &stored[0]).is_err());
}

#[tokio::test]
async fn changes_are_audited_and_no_secret_or_url_path_is_recorded() {
    let w = world();
    let root = admin(&w).await;
    let (d, first) = w
        .access
        .create_webhook(&root, req("https://example.com/hook/token123?k=zzz"))
        .await
        .unwrap();
    let id = d.subscription.id;
    let second = w.access.rotate_webhook_secret(&root, id).await.unwrap();
    w.access
        .update_webhook(
            &root,
            id,
            SubscriptionUpdate {
                enabled: Some(false),
                ..SubscriptionUpdate::default()
            },
        )
        .await
        .unwrap();
    w.access
        .retire_webhook_secret(&root, id, first.info.id)
        .await
        .unwrap();
    w.access.delete_webhook(&root, id).await.unwrap();

    let events = w
        .audit
        .list(
            &AuditScope::Server,
            &AuditQuery {
                limit: 50,
                ..AuditQuery::default()
            },
        )
        .await
        .unwrap();
    let mut actions: Vec<AuditAction> = events
        .iter()
        .map(|e| e.action)
        .filter(|a| a.as_str().starts_with("webhook_"))
        .collect();
    actions.reverse();
    assert_eq!(
        actions,
        [
            AuditAction::WebhookCreated,
            AuditAction::WebhookSecretRotated,
            AuditAction::WebhookUpdated,
            AuditAction::WebhookSecretRetired,
            AuditAction::WebhookDeleted,
        ]
    );
    for e in &events {
        for hidden in [&first.value, &second.value] {
            assert!(!e.detail.contains(hidden.as_str()));
        }
        assert!(
            !e.detail.contains("token123") && !e.detail.contains("zzz"),
            "{}",
            e.detail
        );
        assert_eq!(e.actor, UserId::new("root"));
    }
    let created = events
        .iter()
        .find(|e| e.action == AuditAction::WebhookCreated)
        .unwrap();
    assert!(created.detail.contains("https://example.com"));
}

#[test]
fn the_new_scope_and_actions_parse_and_display() {
    assert_eq!(
        "manage_webhooks".parse::<ServiceScope>().unwrap(),
        ServiceScope::ManageWebhooks
    );
    assert_eq!(ServiceScope::ManageWebhooks.to_string(), "manage_webhooks");
    for a in [
        AuditAction::WebhookCreated,
        AuditAction::WebhookUpdated,
        AuditAction::WebhookDeleted,
        AuditAction::WebhookSecretRotated,
        AuditAction::WebhookSecretRetired,
    ] {
        assert_eq!(a.as_str().parse::<AuditAction>().unwrap(), a);
        assert!(AuditAction::ALL.contains(&a));
    }
}

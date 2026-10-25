//! The administration service over the Postgres webhook store: rotation limit, retire and encryption at rest.

use std::sync::Arc;

use pyn_core::memory::MemoryAccessStore;
use pyn_core::webhook::WebhookStore;
use pyn_core::{
    AccessService, CreateWebhook, Credential, Identity, PynError, SecretKey, SystemClock, UserId,
    WebhookSettings, reveal_secret,
};
use pyn_postgres::PgMetadataStore;

const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn secrets_rotate_retire_and_are_encrypted_in_postgres() {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let pg = Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    );
    let key = SecretKey::from_hex(KEY).unwrap();
    let access = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        Arc::new(SystemClock),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_webhooks(
        pg.clone(),
        WebhookSettings {
            secret_key: Some(key.clone()),
            allow_http: false,
        },
    );
    let root = UserId::new("root");
    access.admin_for_tests(&root).await.unwrap();
    let who = Identity {
        user: root,
        credential: Credential::Session,
    };

    let (detail, first) = access
        .create_webhook(
            &who,
            CreateWebhook {
                name: "ci".into(),
                url: "https://example.com/hook".into(),
                event_types: vec!["repository.created".into()],
                enabled: true,
            },
        )
        .await
        .unwrap();
    let id = detail.subscription.id;
    let second = access.rotate_webhook_secret(&who, id).await.unwrap();
    assert!(matches!(
        access.rotate_webhook_secret(&who, id).await,
        Err(PynError::WebhookSecretLimit(2))
    ));

    let stored = pg.list_active_secrets(id).await.unwrap();
    assert_eq!(stored.len(), 2);
    for (row, value) in stored.iter().zip([&first.value, &second.value]) {
        assert!(row.encrypted);
        assert_eq!(row.key_id.as_deref(), Some(key.key_id()));
        assert_eq!(reveal_secret(Some(&key), row).unwrap(), value.as_bytes());
    }

    access
        .retire_webhook_secret(&who, id, first.info.id)
        .await
        .unwrap();
    assert_eq!(access.get_webhook(&who, id).await.unwrap().secrets.len(), 1);
    access.delete_webhook(&who, id).await.unwrap();
    assert!(pg.get_subscription(id).await.unwrap().is_none());
}

//! Lock changes queue wire events with the change, and a signed delivery reaches a loopback receiver only when allowed.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Clock, Credential, FixedJitter, Identity,
    ManualClock, NewSecret, NewSubscription, RegistrationMode, RepoPath, Repositories, Rules,
    UserId, WebhookSettings, WebhookStore,
};
use pyn_proto::{Event, EventData};
use pyn_server::webhook_delivery::{DeliveryWorker, SafeSender, WorkerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

struct Fixture {
    access: Arc<AccessService>,
    clock: Arc<ManualClock>,
    hooks: Arc<MemoryWebhookStore>,
    repos: Repositories,
}

async fn fixture(url: &str, event_types: &[&str]) -> Fixture {
    let now = Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap();
    let clock = Arc::new(ManualClock::new(now));
    let hooks = Arc::new(MemoryWebhookStore::new());
    let meta = Arc::new(MemoryMetadataStore::new().with_outbox(hooks.clone()));
    let store = Arc::new(MemoryAccessStore::new().with_outbox(hooks.clone()));
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
            clock.clone(),
            Arc::new(pyn_server::events::WebhookLimitEvents::new(
                hooks.clone(),
                "https://pyn.example".into(),
            )),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone())
        .with_webhooks(hooks.clone(), WebhookSettings::default()),
    );
    for user in ["alice", "bob"] {
        store.ensure_user(&UserId::new(user), now).await.unwrap();
    }
    let sub = hooks
        .create_subscription(
            NewSubscription {
                name: "locks".into(),
                url: url.into(),
                event_types: event_types.iter().map(|t| t.to_string()).collect(),
                enabled: true,
                created_by: UserId::new("alice"),
            },
            now,
        )
        .await
        .unwrap();
    hooks
        .add_secret(
            sub.id,
            NewSecret {
                secret: b"whsec_test".to_vec(),
                encrypted: false,
                key_id: None,
            },
            now,
        )
        .await
        .unwrap();
    let repos = Repositories::new(
        meta,
        Arc::new(MemoryObjectStore::new()),
        audit,
        access.clone(),
        clock.clone(),
        Rules::empty(),
    );
    Fixture {
        access,
        clock,
        hooks,
        repos,
    }
}

#[tokio::test]
async fn lock_changes_queue_events_with_the_change() {
    let f = fixture("https://example.test/hook", &[]).await;
    let (alice, bob) = (who("alice"), who("bob"));
    f.repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    let svc = f.repos.open("alice", "game").await.unwrap().service;
    let path = RepoPath::try_from("Content/a.umap".to_string()).unwrap();

    svc.checkout(&path, &alice.user, None).await.unwrap();
    svc.checkout(&path, &alice.user, None).await.unwrap();
    svc.release(&path, &alice.user).await.unwrap();
    svc.checkout(&path, &alice.user, None).await.unwrap();
    svc.force_unlock(&path, &bob.user, "stuck").await.unwrap();
    svc.checkout(&path, &alice.user, None).await.unwrap();
    let record = f.repos.open("alice", "game").await.unwrap().record;
    let principal = f.access.principal(&record.id, &alice.user).await.unwrap();
    let content = svc.upload_blob(b"v1".to_vec()).await.unwrap();
    let rev = svc
        .checkin(&principal, &path, content, None, "v1".into())
        .await
        .unwrap();
    let first = svc
        .checkout(&path, &alice.user, Some(rev.id))
        .await
        .unwrap();
    f.clock.advance(Duration::days(30));
    svc.checkout(&path, &bob.user, Some(rev.id)).await.unwrap();

    let due = f
        .hooks
        .claim_due(
            100,
            Duration::minutes(1),
            f.clock.now() + Duration::hours(1),
        )
        .await
        .unwrap();
    let mut events: Vec<(i64, Event)> = due
        .iter()
        .map(|d| (d.event.id, serde_json::from_str(&d.event.payload).unwrap()))
        .collect();
    events.sort_by_key(|(id, _)| *id);
    let data: Vec<EventData> = events
        .into_iter()
        .map(|(_, e)| e.data)
        .filter(|d| d.event_type().starts_with("lock."))
        .collect();
    let kinds: Vec<&str> = data.iter().map(EventData::event_type).collect();
    assert_eq!(
        kinds,
        [
            "lock.acquired",
            "lock.released",
            "lock.acquired",
            "lock.forced",
            "lock.acquired",
            "lock.released",
            "lock.acquired",
            "lock.expired",
            "lock.acquired",
        ]
    );
    assert!(matches!(&data[0], EventData::LockAcquired(d)
        if d.owner == "alice" && d.repo == "game" && d.path == "Content/a.umap" && d.holder == "alice"));
    assert!(matches!(&data[3], EventData::LockForced(d)
        if d.holder == "alice" && d.actor == "bob" && d.reason.as_deref() == Some("stuck")));
    assert!(
        matches!(&data[1], EventData::LockReleased(d) if d.holder == "alice" && d.reason.is_none())
    );
    assert!(matches!(&data[5], EventData::LockReleased(d)
        if d.holder == "alice" && d.path == "Content/a.umap" && d.reason.as_deref() == Some("checkin")));
    assert!(matches!(&data[7], EventData::LockExpired(d)
        if d.holder == "alice" && d.expires_at == first.expires_at));
    assert!(matches!(&data[8], EventData::LockAcquired(d) if d.holder == "bob"));
}

#[tokio::test]
async fn a_refused_or_missing_lock_change_queues_nothing() {
    let f = fixture("https://example.test/hook", &[]).await;
    let (alice, bob) = (who("alice"), who("bob"));
    f.repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    let svc = f.repos.open("alice", "game").await.unwrap().service;
    let path = RepoPath::try_from("Content/a.umap".to_string()).unwrap();
    svc.checkout(&path, &alice.user, None).await.unwrap();
    assert!(svc.checkout(&path, &bob.user, None).await.is_err());
    assert!(svc.release(&path, &bob.user).await.is_err());
    let due = f
        .hooks
        .claim_due(100, Duration::minutes(1), f.clock.now())
        .await
        .unwrap();
    let locks = due
        .iter()
        .filter(|d| d.event.event_type.starts_with("lock."))
        .count();
    assert_eq!(locks, 1);
}

/// Answers every request with 200 and counts the connections.
async fn receiver() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = hits.clone();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            while !data.windows(4).any(|w| w == b"\r\n\r\n") || !data.ends_with(b"}") {
                match socket.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                }
            }
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });
    (url, hits)
}

async fn deliver_one_lock_event(sender: SafeSender, url: &str) -> (usize, usize) {
    let f = fixture(url, &["lock.acquired"]).await;
    let alice = who("alice");
    f.repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    let svc = f.repos.open("alice", "game").await.unwrap().service;
    let path = RepoPath::try_from("Content/a.umap".to_string()).unwrap();
    svc.checkout(&path, &alice.user, None).await.unwrap();
    let worker = DeliveryWorker::new(
        f.hooks.clone(),
        f.clock.clone(),
        Arc::new(sender),
        Arc::new(FixedJitter(0.5)),
        None,
        "https://pyn.example".into(),
        WorkerConfig::default(),
    );
    let stats = worker.run_once(f.clock.now()).await.unwrap();
    (stats.delivered, stats.failed)
}

#[tokio::test]
async fn a_lock_event_reaches_a_loopback_receiver_only_with_the_switch_on() {
    let (url, hits) = receiver().await;
    let off = SafeSender::new(true, vec![]);
    assert_eq!(deliver_one_lock_event(off, &url).await, (0, 1));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);

    let on = SafeSender::new(true, vec![]).allowing_loopback();
    assert_eq!(deliver_one_lock_event(on, &url).await, (1, 0));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

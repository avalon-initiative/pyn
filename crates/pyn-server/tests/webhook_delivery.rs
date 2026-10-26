//! The delivery worker against the in-memory store with a scripted sender and a manual clock.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use pyn_core::memory::MemoryWebhookStore;
use pyn_core::webhook_secret::store_secret;
use pyn_core::{
    DeliveryQuery, DeliveryStatus, FixedJitter, ManualClock, NewSubscription, SecretKey, UserId,
    WebhookStore,
};
use pyn_server::webhook_delivery::{
    DeliveryWorker, HttpSender, OutgoingRequest, WorkerConfig, signature_header,
};

type Reply = Result<u16, String>;

#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    sent: Mutex<Vec<OutgoingRequest>>,
}

impl Scripted {
    fn replying(replies: Vec<Reply>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            ..Self::default()
        })
    }

    fn sent(&self) -> Vec<OutgoingRequest> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpSender for Scripted {
    async fn send(&self, request: OutgoingRequest) -> Reply {
        self.sent.lock().unwrap().push(request);
        self.replies.lock().unwrap().pop_front().unwrap_or(Ok(200))
    }
}

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap()
}

fn header<'a>(request: &'a OutgoingRequest, name: &str) -> &'a str {
    &request.headers.iter().find(|(n, _)| *n == name).unwrap().1
}

struct Rig {
    store: Arc<MemoryWebhookStore>,
    clock: Arc<ManualClock>,
    sub: i64,
}

async fn rig(secrets: &[&str], key: Option<&SecretKey>) -> Rig {
    let store = Arc::new(MemoryWebhookStore::new());
    let sub = store
        .create_subscription(
            NewSubscription {
                name: "hook".into(),
                url: "https://receiver.test/hook".into(),
                event_types: vec![],
                enabled: true,
                created_by: UserId::new("root"),
            },
            t0(),
        )
        .await
        .unwrap();
    for s in secrets {
        store
            .add_secret(sub.id, store_secret(key, s).unwrap(), t0())
            .await
            .unwrap();
    }
    store
        .append_event("repository.created", r#"{"id":"e1"}"#, t0())
        .await
        .unwrap();
    Rig {
        store,
        clock: Arc::new(ManualClock::new(t0())),
        sub: sub.id,
    }
}

fn worker(rig: &Rig, sender: Arc<Scripted>, key: Option<SecretKey>) -> DeliveryWorker {
    DeliveryWorker::new(
        rig.store.clone(),
        rig.clock.clone(),
        sender,
        Arc::new(FixedJitter(0.5)),
        key,
        "https://pyn.example".into(),
        WorkerConfig::default(),
    )
}

async fn only_delivery(rig: &Rig) -> pyn_core::Delivery {
    let mut all = rig
        .store
        .list_deliveries(
            rig.sub,
            &DeliveryQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    all.remove(0)
}

#[tokio::test]
async fn a_delivery_is_signed_with_every_active_secret_and_marked_delivered() {
    let rig = rig(&["whsec_one", "whsec_two"], None).await;
    let sender = Scripted::replying(vec![Ok(204)]);
    let stats = worker(&rig, sender.clone(), None)
        .run_once(t0())
        .await
        .unwrap();
    assert_eq!((stats.claimed, stats.delivered, stats.failed), (1, 1, 0));

    let sent = sender.sent();
    assert_eq!(sent.len(), 1);
    let request = &sent[0];
    assert_eq!(request.url, "https://receiver.test/hook");
    assert_eq!(request.body, br#"{"id":"e1"}"#);
    let keys = [b"whsec_one".to_vec(), b"whsec_two".to_vec()];
    assert_eq!(
        header(request, "Pyn-Signature"),
        signature_header(&keys, t0().timestamp(), &request.body)
    );
    assert_eq!(header(request, "Pyn-Event"), "repository.created");
    assert_eq!(header(request, "Pyn-Server"), "https://pyn.example");
    let delivery = only_delivery(&rig).await;
    assert_eq!(header(request, "Pyn-Delivery"), delivery.id);
    assert_eq!(delivery.status, DeliveryStatus::Delivered);
    assert_eq!(delivery.last_status_code, Some(204));
}

#[tokio::test]
async fn a_failed_attempt_backs_off_then_retries_with_a_fresh_signature_and_the_same_id() {
    let rig = rig(&["whsec_one"], None).await;
    let sender = Scripted::replying(vec![Ok(500), Ok(200)]);
    let w = worker(&rig, sender.clone(), None);
    let first = w.run_once(t0()).await.unwrap();
    assert_eq!((first.delivered, first.failed), (0, 1));
    let after_failure = only_delivery(&rig).await;
    assert_eq!(after_failure.attempt_count, 1);
    assert_eq!(after_failure.last_status_code, Some(500));
    assert_eq!(after_failure.status, DeliveryStatus::Pending);

    assert_eq!(
        w.run_once(t0() + Duration::seconds(30))
            .await
            .unwrap()
            .claimed,
        0
    );
    let later = t0() + Duration::minutes(5);
    assert_eq!(w.run_once(later).await.unwrap().delivered, 1);

    let sent = sender.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(
        header(&sent[0], "Pyn-Delivery"),
        header(&sent[1], "Pyn-Delivery")
    );
    assert_ne!(
        header(&sent[0], "Pyn-Signature"),
        header(&sent[1], "Pyn-Signature")
    );
    assert!(header(&sent[1], "Pyn-Signature").starts_with(&format!("t={}", later.timestamp())));
    assert_eq!(only_delivery(&rig).await.status, DeliveryStatus::Delivered);
}

#[tokio::test]
async fn eight_failed_attempts_mark_the_delivery_failed() {
    let rig = rig(&["whsec_one"], None).await;
    let sender = Scripted::replying(vec![Err("could not connect".into()); 20]);
    let w = worker(&rig, sender.clone(), None);
    let mut now = t0();
    for _ in 0..12 {
        w.run_once(now).await.unwrap();
        now += Duration::hours(13);
    }
    assert_eq!(sender.sent().len(), 8);
    let delivery = only_delivery(&rig).await;
    assert_eq!(delivery.status, DeliveryStatus::Failed);
    assert_eq!(delivery.last_error.as_deref(), Some("could not connect"));
    assert_eq!(delivery.last_status_code, None);
}

#[tokio::test]
async fn a_subscription_without_a_secret_fails_without_sending() {
    let rig = rig(&[], None).await;
    let sender = Scripted::replying(vec![]);
    let stats = worker(&rig, sender.clone(), None)
        .run_once(t0())
        .await
        .unwrap();
    assert_eq!(stats.failed, 1);
    assert!(sender.sent().is_empty());
    assert!(
        only_delivery(&rig)
            .await
            .last_error
            .unwrap()
            .contains("no active signing secret")
    );
}

#[tokio::test]
async fn encrypted_secrets_sign_when_the_key_is_set_and_fail_without_it() {
    let key = SecretKey::from_hex(&"07".repeat(32)).unwrap();
    let rig = rig(&["whsec_sealed"], Some(&key)).await;
    let sender = Scripted::replying(vec![]);
    worker(&rig, sender.clone(), Some(key))
        .run_once(t0())
        .await
        .unwrap();
    let request = &sender.sent()[0];
    assert_eq!(
        header(request, "Pyn-Signature"),
        signature_header(&[b"whsec_sealed".to_vec()], t0().timestamp(), &request.body)
    );

    let rig = self::rig(
        &["whsec_sealed"],
        Some(&SecretKey::from_hex(&"07".repeat(32)).unwrap()),
    )
    .await;
    let sender = Scripted::replying(vec![]);
    worker(&rig, sender.clone(), None)
        .run_once(t0())
        .await
        .unwrap();
    assert!(sender.sent().is_empty());
    assert!(only_delivery(&rig).await.last_error.is_some());
}

#[tokio::test]
async fn workers_sharing_a_store_never_send_the_same_delivery_twice() {
    let rig = rig(&["whsec_one"], None).await;
    let sender = Scripted::replying(vec![]);
    let (a, b) = (
        worker(&rig, sender.clone(), None),
        worker(&rig, sender.clone(), None),
    );
    let (x, y) = tokio::join!(a.run_once(t0()), b.run_once(t0()));
    assert_eq!(x.unwrap().claimed + y.unwrap().claimed, 1);
    assert_eq!(sender.sent().len(), 1);
}

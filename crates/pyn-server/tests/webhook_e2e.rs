//! Webhooks end to end: the admin API, the outbox, the delivery worker and a local receiver that follows the receiver guide.
//! Time is a manual clock; nothing sleeps.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, Clock, FixedJitter, ManualClock, RegistrationMode,
    Repositories, Rules, UserId, WebhookSettings, WebhookStore,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::webhook_delivery::{DeliveryWorker, HttpSender, OutgoingRequest, WorkerConfig};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use sha2::Sha256;
use tower::ServiceExt;

const REPLAY_WINDOW: i64 = 300;
const BODY: &str = r#"{"id":"evt_1","type":"repository.created"}"#;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap()
}

/// What the receiver guide tells a receiver to do: replay window, any matching `v1`, dedupe on the delivery id.
struct Receiver {
    clock: Arc<ManualClock>,
    secrets: Mutex<Vec<String>>,
    answers: Mutex<VecDeque<u16>>,
    seen: Mutex<HashSet<String>>,
    accepted: Mutex<Vec<(String, usize)>>,
    duplicates: Mutex<Vec<String>>,
    rejected: Mutex<Vec<String>>,
}

impl Receiver {
    fn new(clock: Arc<ManualClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            secrets: Mutex::default(),
            answers: Mutex::default(),
            seen: Mutex::default(),
            accepted: Mutex::default(),
            duplicates: Mutex::default(),
            rejected: Mutex::default(),
        })
    }

    fn trust(&self, secret: &str) {
        self.secrets.lock().unwrap().push(secret.to_string());
    }

    fn answer(&self, statuses: &[u16]) {
        self.answers
            .lock()
            .unwrap()
            .extend(statuses.iter().copied());
    }

    fn accepted_ids(&self) -> Vec<String> {
        self.accepted
            .lock()
            .unwrap()
            .iter()
            .map(|a| a.0.clone())
            .collect()
    }
}

fn verify(
    header: &str,
    body: &[u8],
    now: DateTime<Utc>,
    secrets: &[String],
) -> Result<usize, &'static str> {
    let mut t = None;
    let mut tags = Vec::new();
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => tags.push(v.to_string()),
            _ => {}
        }
    }
    let t = t.ok_or("no timestamp")?;
    if (now.timestamp() - t).abs() > REPLAY_WINDOW {
        return Err("outside the replay window");
    }
    let signed = format!("{t}.");
    for secret in secrets {
        for tag in &tags {
            let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
            mac.update(signed.as_bytes());
            mac.update(body);
            if hex::decode(tag).is_ok_and(|raw| mac.verify_slice(&raw).is_ok()) {
                return Ok(tags.len());
            }
        }
    }
    Err("no signature matches")
}

fn header<'a>(request: &'a OutgoingRequest, name: &str) -> &'a str {
    &request.headers.iter().find(|(n, _)| *n == name).unwrap().1
}

#[async_trait]
impl HttpSender for Receiver {
    async fn send(&self, request: OutgoingRequest) -> Result<u16, String> {
        let secrets = self.secrets.lock().unwrap().clone();
        let checked = verify(
            header(&request, "Pyn-Signature"),
            &request.body,
            self.clock.now(),
            &secrets,
        );
        let signatures = match checked {
            Ok(n) => n,
            Err(why) => {
                self.rejected.lock().unwrap().push(why.to_string());
                return Ok(401);
            }
        };
        let id = header(&request, "Pyn-Delivery").to_string();
        if let Some(status) = self.answers.lock().unwrap().pop_front()
            && status >= 300
        {
            return Ok(status);
        }
        if !self.seen.lock().unwrap().insert(id.clone()) {
            self.duplicates.lock().unwrap().push(id);
            return Ok(200);
        }
        self.accepted.lock().unwrap().push((id, signatures));
        Ok(200)
    }
}

struct World {
    app: Router,
    store: Arc<MemoryWebhookStore>,
    clock: Arc<ManualClock>,
    worker: DeliveryWorker,
    receiver: Arc<Receiver>,
}

async fn world() -> World {
    let clock = Arc::new(ManualClock::new(t0()));
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let store = Arc::new(MemoryWebhookStore::new());
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
        .with_webhooks(store.clone(), WebhookSettings::default()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock.clone(),
        Rules::empty(),
    ));
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    });
    let receiver = Receiver::new(clock.clone());
    let worker = DeliveryWorker::new(
        store.clone(),
        clock.clone(),
        receiver.clone(),
        Arc::new(FixedJitter(0.5)),
        None,
        "https://pyn.example".into(),
        WorkerConfig::default(),
    );
    World {
        app,
        store,
        clock,
        worker,
        receiver,
    }
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
}

impl World {
    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Reply {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header(api::DEV_USER_HEADER, "root")
            .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
            .unwrap();
        let r = self.app.clone().oneshot(req).await.unwrap();
        Reply {
            status: r.status(),
            body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
        }
    }

    async fn subscribe(&self) -> (i64, String) {
        let r = self
            .call(
                "POST",
                "/v1/admin/webhooks",
                Some(json!({"name": "ci", "url": "https://receiver.test/hook"})),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED);
        let made: api::CreatedWebhook = r.json();
        self.receiver.trust(&made.secret);
        (made.webhook.id, made.secret)
    }

    async fn raise(&self) {
        self.store
            .append_event("repository.created", BODY, self.clock.now())
            .await
            .unwrap();
    }

    async fn pass(&self) -> pyn_server::webhook_delivery::RunStats {
        self.worker.run_once(self.clock.now()).await.unwrap()
    }

    async fn deliveries(&self, id: i64, query: &str) -> api::WebhookDeliveryPage {
        let r = self
            .call(
                "GET",
                &format!("/v1/admin/webhooks/{id}/deliveries{query}"),
                None,
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        r.json()
    }
}

#[tokio::test]
async fn a_delivery_verifies_and_shows_in_the_delivery_list() {
    let w = world().await;
    let (id, _) = w.subscribe().await;
    w.raise().await;
    assert_eq!(w.pass().await.delivered, 1);

    assert!(w.receiver.rejected.lock().unwrap().is_empty());
    let page = w.deliveries(id, "").await;
    assert_eq!(page.deliveries.len(), 1);
    let d = &page.deliveries[0];
    assert_eq!(d.status, api::WebhookDeliveryStatus::Delivered);
    assert_eq!((d.attempts, d.last_status_code), (1, Some(200)));
    assert_eq!(w.receiver.accepted_ids(), std::slice::from_ref(&d.id));
}

#[tokio::test]
async fn a_signature_outside_the_replay_window_is_refused() {
    let w = world().await;
    let (_, secret) = w.subscribe().await;
    let signature = pyn_server::webhook_delivery::signature_header(
        &[secret.clone().into_bytes()],
        t0().timestamp(),
        BODY.as_bytes(),
    );
    let secrets = [secret];
    let inside = t0() + Duration::seconds(REPLAY_WINDOW);
    assert!(verify(&signature, BODY.as_bytes(), inside, &secrets).is_ok());
    let outside = inside + Duration::seconds(1);
    assert_eq!(
        verify(&signature, BODY.as_bytes(), outside, &secrets),
        Err("outside the replay window")
    );
    let early = t0() - Duration::seconds(REPLAY_WINDOW + 1);
    assert!(verify(&signature, BODY.as_bytes(), early, &secrets).is_err());
    assert_eq!(
        verify(&signature, b"{}", t0(), &secrets),
        Err("no signature matches")
    );
}

#[tokio::test]
async fn a_failed_attempt_retries_after_the_backoff_with_a_fresh_signature_and_the_same_id() {
    let w = world().await;
    let (id, _) = w.subscribe().await;
    w.receiver.answer(&[503]);
    w.raise().await;
    assert_eq!(w.pass().await.failed, 1);

    let page = w.deliveries(id, "?status=pending").await;
    let d = &page.deliveries[0];
    assert_eq!((d.attempts, d.last_status_code), (1, Some(503)));
    assert_eq!(d.next_attempt_at, t0() + Duration::seconds(60));
    assert!(d.last_error.as_deref().unwrap().contains("503"));

    w.clock.advance(Duration::seconds(59));
    assert_eq!(w.pass().await.claimed, 0);
    w.clock.advance(Duration::seconds(1));
    assert_eq!(w.pass().await.delivered, 1);

    let page = w.deliveries(id, "").await;
    assert_eq!(page.deliveries[0].id, d.id);
    assert_eq!(
        page.deliveries[0].status,
        api::WebhookDeliveryStatus::Delivered
    );
    assert_eq!(page.deliveries[0].attempts, 2);
    assert_eq!(w.receiver.accepted_ids(), std::slice::from_ref(&d.id));
    assert!(w.receiver.rejected.lock().unwrap().is_empty());
}

#[tokio::test]
async fn during_a_rotation_deliveries_carry_both_signatures_until_one_is_retired() {
    let w = world().await;
    let (id, old) = w.subscribe().await;
    let r = w
        .call("POST", &format!("/v1/admin/webhooks/{id}/secrets"), None)
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let rotated: api::RotatedWebhookSecret = r.json();

    // A receiver that knows only the old secret, then only the new one, verifies the same delivery stream.
    w.raise().await;
    assert_eq!(w.pass().await.delivered, 1);
    w.receiver.secrets.lock().unwrap().clear();
    w.receiver.trust(&rotated.secret);
    w.raise().await;
    assert_eq!(w.pass().await.delivered, 1);
    assert!(w.receiver.rejected.lock().unwrap().is_empty());
    let signatures: Vec<usize> = w
        .receiver
        .accepted
        .lock()
        .unwrap()
        .iter()
        .map(|a| a.1)
        .collect();
    assert_eq!(signatures, [2, 2]);

    let old_id = w
        .call("GET", &format!("/v1/admin/webhooks/{id}"), None)
        .await
        .json::<api::Webhook>()
        .secrets[0]
        .id;
    let r = w
        .call(
            "DELETE",
            &format!("/v1/admin/webhooks/{id}/secrets/{old_id}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    w.raise().await;
    assert_eq!(w.pass().await.delivered, 1);
    assert_eq!(w.receiver.accepted.lock().unwrap()[2].1, 1);

    // The retired secret no longer verifies anything.
    w.receiver.secrets.lock().unwrap().clear();
    w.receiver.trust(&old);
    w.raise().await;
    assert_eq!(w.pass().await.failed, 1);
    assert_eq!(w.receiver.rejected.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_manual_redeliver_keeps_the_delivery_id_and_is_audited() {
    let w = world().await;
    let (id, _) = w.subscribe().await;
    w.receiver.answer(&[500; 8]);
    w.raise().await;
    for _ in 0..8 {
        w.pass().await;
        w.clock.advance(Duration::hours(13));
    }
    let page = w.deliveries(id, "?status=failed").await;
    assert_eq!(page.deliveries.len(), 1);
    let failed = page.deliveries[0].clone();
    assert_eq!(failed.attempts, 8);
    assert!(failed.failed_at.is_some());

    let uri = format!("/v1/admin/webhooks/{id}/deliveries/{}/redeliver", failed.id);
    let r = w.call("POST", &uri, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let queued: api::WebhookDelivery = r.json();
    assert_eq!(queued.id, failed.id);
    assert_eq!(queued.status, api::WebhookDeliveryStatus::Pending);
    assert_eq!((queued.attempts, queued.last_error), (0, None));
    assert_eq!(queued.next_attempt_at, w.clock.now());

    assert_eq!(w.pass().await.delivered, 1);
    assert_eq!(w.receiver.accepted_ids(), std::slice::from_ref(&failed.id));

    // Redelivering a delivered one reaches the receiver again with the same id, so it dedupes.
    let r = w.call("POST", &uri, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(w.pass().await.delivered, 1);
    assert_eq!(
        *w.receiver.duplicates.lock().unwrap(),
        std::slice::from_ref(&failed.id)
    );
    assert_eq!(w.receiver.accepted_ids().len(), 1);

    let audit = w.call("GET", "/v1/admin/audit?limit=100", None).await;
    let entries = audit.json::<api::AuditPage>().entries;
    let redelivered: Vec<_> = entries
        .iter()
        .filter(|e| e.action == "webhook_redelivered")
        .collect();
    assert_eq!(redelivered.len(), 2);
    assert!(redelivered[0].detail.contains(&failed.id));
    assert_eq!(redelivered[0].actor, "root");
}

#[tokio::test]
async fn deliveries_page_filter_and_refuse_foreign_or_unknown_ids() {
    let w = world().await;
    let (id, _) = w.subscribe().await;
    let (other, _) = w.subscribe().await;
    for _ in 0..3 {
        w.raise().await;
    }
    w.receiver.answer(&[500]);
    w.pass().await;

    let first = w.deliveries(id, "?limit=2").await;
    assert_eq!(first.deliveries.len(), 2);
    let cursor = first.next_before.expect("a second page");
    let second = w.deliveries(id, &format!("?limit=2&before={cursor}")).await;
    assert_eq!(second.deliveries.len(), 1);
    assert_eq!(second.next_before, None);
    assert!(first.deliveries[0].event_id > first.deliveries[1].event_id);

    let delivered = w.deliveries(id, "?status=delivered").await;
    assert!(
        delivered
            .deliveries
            .iter()
            .all(|d| d.status == api::WebhookDeliveryStatus::Delivered)
    );

    let r = w
        .call(
            "GET",
            &format!("/v1/admin/webhooks/{id}/deliveries?status=bogus"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = w
        .call("GET", "/v1/admin/webhooks/99/deliveries", None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let foreign = first.deliveries[0].id.clone();
    let r = w
        .call(
            "POST",
            &format!("/v1/admin/webhooks/{other}/deliveries/{foreign}/redeliver"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(
        r.json::<api::ErrorBody>().code,
        "webhook_delivery_not_found"
    );
    let r = w
        .call(
            "POST",
            &format!("/v1/admin/webhooks/{id}/deliveries/not-a-uuid/redeliver"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

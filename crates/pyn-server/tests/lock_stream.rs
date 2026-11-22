//! The lock change stream: every lock transition appears once and in order, a cursor resumes without a gap, and the
//! route is authorised like the lock list.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuthProvider, Credential, Identity, ManualClock,
    Permission, RegistrationMode, RepoPath, Repositories, Rules, UserId, Visibility,
    WebhookSettings,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::events::WebhookLimitEvents;
use pyn_server::{AppState, router};
use tower::ServiceExt;

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap()
}

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

fn path(p: &str) -> RepoPath {
    RepoPath::try_from(p.to_string()).unwrap()
}

struct World {
    app: Router,
    access: Arc<AccessService>,
    repos: Arc<Repositories>,
    clock: Arc<ManualClock>,
}

async fn world() -> World {
    let clock = Arc::new(ManualClock::new(t0()));
    let hooks = Arc::new(MemoryWebhookStore::new());
    let meta = Arc::new(MemoryMetadataStore::new().with_outbox(hooks.clone()));
    let store = Arc::new(MemoryAccessStore::new().with_outbox(hooks.clone()));
    let audit = Arc::new(MemoryAuditStore::new());
    let objects = Arc::new(MemoryObjectStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
            clock.clone(),
            Arc::new(WebhookLimitEvents::new(
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
        store.ensure_user(&UserId::new(user), t0()).await.unwrap();
    }
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock.clone(),
        Rules::empty(),
    ));
    let alice = who("alice");
    for (name, visibility) in [
        ("game", Visibility::Private),
        ("other", Visibility::Private),
        ("open", Visibility::Public),
    ] {
        repos
            .create(&alice, &alice.user, name, Some(visibility), None)
            .await
            .unwrap();
    }
    let app = router(AppState {
        repos: repos.clone(),
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth {
            access: access.clone(),
        }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        certificate: Default::default(),
        ready: Default::default(),
    });
    World {
        app,
        access,
        repos,
        clock,
    }
}

impl World {
    async fn get(&self, cred: &Cred, uri: &str) -> (StatusCode, Vec<u8>) {
        let req = Request::builder()
            .uri(uri)
            .cred(cred)
            .body(Body::empty())
            .unwrap();
        let r = self.app.clone().oneshot(req).await.unwrap();
        (
            r.status(),
            r.into_body().collect().await.unwrap().to_bytes().to_vec(),
        )
    }

    async fn page(&self, repo: &str, query: &str) -> api::LockStreamPage {
        let (status, body) = self
            .get(
                &Cred::dev("alice"),
                &format!("/v1/repos/alice/{repo}/locks/events{query}"),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        serde_json::from_slice(&body).unwrap()
    }

    async fn service(&self, repo: &str) -> Arc<pyn_core::RepoService> {
        self.repos.open("alice", repo).await.unwrap().service
    }
}

fn kinds(page: &api::LockStreamPage) -> Vec<&str> {
    page.events
        .iter()
        .map(|e| e.event.event_type.as_str())
        .collect()
}

#[tokio::test]
async fn every_lock_transition_is_in_the_stream_in_order() {
    let w = world().await;
    let head = w.page("game", "").await;
    assert!(head.events.is_empty());
    let (alice, bob) = (who("alice"), who("bob"));
    let svc = w.service("game").await;
    let a = path("Content/a.umap");

    svc.checkout(&a, &alice.user, None).await.unwrap();
    svc.checkout(&a, &alice.user, None).await.unwrap();
    svc.release(&a, &alice.user).await.unwrap();
    svc.checkout(&a, &alice.user, None).await.unwrap();
    svc.force_unlock(&a, &bob.user, "stuck").await.unwrap();
    svc.checkout(&a, &alice.user, None).await.unwrap();
    let record = w.repos.open("alice", "game").await.unwrap().record;
    let principal = w.access.principal(&record.id, &alice.user).await.unwrap();
    let content = svc.upload_blob(b"v1".to_vec()).await.unwrap();
    let rev = svc
        .checkin(&principal, &a, content, None, "v1".into())
        .await
        .unwrap();
    let held = svc.checkout(&a, &alice.user, Some(rev.id)).await.unwrap();
    w.clock.advance(Duration::days(30));
    assert_eq!(w.repos.sweep_expired_locks().await.unwrap(), 1);
    assert_eq!(w.repos.sweep_expired_locks().await.unwrap(), 0);
    svc.checkout(&a, &bob.user, Some(rev.id)).await.unwrap();

    let page = w.page("game", &format!("?after={}", head.cursor)).await;
    assert_eq!(
        kinds(&page),
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
    let data: Vec<_> = page.events.iter().map(|e| &e.event.data).collect();
    assert!(matches!(data[0], api::EventData::LockAcquired(d)
        if d.owner == "alice" && d.repo == "game" && d.path == "Content/a.umap" && d.holder == "alice"));
    assert!(matches!(data[3], api::EventData::LockForced(d)
        if d.holder == "alice" && d.actor == "bob" && d.reason.as_deref() == Some("stuck")));
    assert!(matches!(data[5], api::EventData::LockReleased(d)
        if d.reason.as_deref() == Some("checkin")));
    assert!(matches!(data[7], api::EventData::LockExpired(d)
        if d.holder == "alice" && d.expires_at == held.expires_at));
    assert!(matches!(data[8], api::EventData::LockAcquired(d) if d.holder == "bob"));
    let cursors: Vec<i64> = page
        .events
        .iter()
        .map(|e| e.cursor.parse().unwrap())
        .collect();
    assert!(cursors.windows(2).all(|p| p[0] < p[1]));
    assert_eq!(page.cursor, page.events.last().unwrap().cursor);
}

#[tokio::test]
async fn resuming_from_a_cursor_misses_and_repeats_nothing() {
    let w = world().await;
    let start = w.page("game", "").await.cursor;
    let alice = who("alice");
    let svc = w.service("game").await;
    for n in 0..5 {
        svc.checkout(&path(&format!("Content/{n}.umap")), &alice.user, None)
            .await
            .unwrap();
    }

    let mut seen = Vec::new();
    let mut cursor = start.clone();
    loop {
        let page = w.page("game", &format!("?after={cursor}&limit=2")).await;
        if page.events.is_empty() {
            assert_eq!(page.cursor, cursor);
            break;
        }
        assert!(page.events.len() <= 2);
        seen.extend(page.events.iter().map(|e| e.cursor.clone()));
        cursor = page.cursor;
    }
    let all = w.page("game", &format!("?after={start}")).await;
    let all: Vec<String> = all.events.iter().map(|e| e.cursor.clone()).collect();
    assert_eq!(all.len(), 5);
    assert_eq!(seen, all);

    svc.release(&path("Content/0.umap"), &alice.user)
        .await
        .unwrap();
    let next = w.page("game", &format!("?after={cursor}")).await;
    assert_eq!(kinds(&next), ["lock.released"]);
}

#[tokio::test]
async fn a_repository_sees_only_its_own_lock_events() {
    let w = world().await;
    let alice = who("alice");
    let game = w.service("game").await;
    let other = w.service("other").await;
    game.checkout(&path("a.bin"), &alice.user, None)
        .await
        .unwrap();
    other
        .checkout(&path("b.bin"), &alice.user, None)
        .await
        .unwrap();

    let page = w.page("game", "?after=0").await;
    assert_eq!(page.events.len(), 1);
    assert!(
        matches!(&page.events[0].event.data, api::EventData::LockAcquired(d) if d.path == "a.bin")
    );
}

#[tokio::test]
async fn a_held_request_returns_when_a_change_arrives_and_empty_when_none_does() {
    let w = world().await;
    let head = w.page("game", "").await.cursor;
    let quiet = w.page("game", &format!("?after={head}&wait=1")).await;
    assert!(quiet.events.is_empty());
    assert_eq!(quiet.cursor, head);

    let svc = w.service("game").await;
    let alice = who("alice");
    let writer = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        svc.checkout(&path("a.bin"), &alice.user, None)
            .await
            .unwrap();
    });
    let held = w.page("game", &format!("?after={head}&wait=10")).await;
    writer.await.unwrap();
    assert_eq!(kinds(&held), ["lock.acquired"]);
}

#[tokio::test]
async fn reading_the_stream_needs_what_reading_the_lock_list_needs() {
    let w = world().await;
    let alice = who("alice");
    w.service("game")
        .await
        .checkout(&path("a.bin"), &alice.user, None)
        .await
        .unwrap();
    w.service("open")
        .await
        .checkout(&path("a.bin"), &alice.user, None)
        .await
        .unwrap();

    let bob = who("bob");
    let mine = w
        .repos
        .create(&bob, &bob.user, "mine", None, None)
        .await
        .unwrap()
        .id;
    let (_, secret) = w
        .access
        .create_token(
            &bob,
            pyn_core::fixtures::new_token_for(&bob, "reader", [Permission::Read], vec![mine], None),
        )
        .await
        .unwrap();
    let bob = Cred::token(&secret);
    for (cred, repo) in [
        (bob.clone(), "game"),
        (Cred::SignedOut, "game"),
        (bob.clone(), "missing"),
    ] {
        let locks = w
            .get(&cred, &format!("/v1/repos/alice/{repo}/locks"))
            .await
            .0;
        let stream = w
            .get(
                &cred,
                &format!("/v1/repos/alice/{repo}/locks/events?after=0"),
            )
            .await
            .0;
        assert_ne!(locks, StatusCode::OK, "{repo} as {cred:?}");
        assert_eq!(stream, locks, "{repo} as {cred:?}");
    }
    let (status, body) = w
        .get(
            &Cred::dev("bob"),
            "/v1/repos/alice/open/locks/events?after=0",
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let page: api::LockStreamPage = serde_json::from_slice(&body).unwrap();
    assert_eq!(kinds(&page), ["lock.acquired"]);
    let (status, body) = w
        .get(
            &Cred::dev("alice"),
            "/v1/repos/alice/game/locks/events?after=abc",
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(String::from_utf8_lossy(&body).contains("invalid_request"));
}

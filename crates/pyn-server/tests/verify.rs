use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, MetadataStore, Principal, RepoId, RepoPath,
    RepoRecord, RepoSettings, Repositories, RevisionId, Rules, SystemClock, UserId, Visibility,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use tower::ServiceExt;

struct World {
    app: axum::Router,
    meta: Arc<MemoryMetadataStore>,
}

async fn world() -> World {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    meta.create_repo(RepoRecord {
        id: RepoId::new("t"),
        owner: UserId::new("owner"),
        name: "game".into(),
        visibility: Visibility::Private,
        settings: RepoSettings::default(),
        created_at: chrono::Utc::now(),
    })
    .await
    .unwrap();
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig::default())
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Arc::new(Repositories::new(
        meta.clone(),
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    ));
    let service = repos.open("owner", "game").await.unwrap().service;
    let who = Principal::unrestricted(UserId::new("alice"));
    let path = RepoPath::new("a.txt").unwrap();
    for (i, text) in ["one", "two"].into_iter().enumerate() {
        let content = service.upload_blob(text.as_bytes().to_vec()).await.unwrap();
        let base = (i > 0).then_some(RevisionId(i as u64));
        service
            .checkin(&who, &path, content, base, text.into())
            .await
            .unwrap();
    }
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: true,
    });
    World { app, meta }
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let r = app
        .clone()
        .oneshot(
            Request::get(uri)
                .header(api::DEV_USER_HEADER, "alice")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

#[tokio::test]
async fn revisions_and_audit_entries_carry_their_hashes() {
    let w = world().await;
    let (_, body) = get(&w.app, "/v1/repos/owner/game/history?path=a.txt").await;
    let page: api::HistoryPage = serde_json::from_slice(&body).unwrap();
    assert_eq!(page.revisions[0].previous, None);
    assert_eq!(
        page.revisions[1].previous.as_deref(),
        Some(page.revisions[0].hash.as_str())
    );
    assert_eq!(page.revisions[0].hash.len(), 64);

    let (_, body) = get(&w.app, "/v1/repos/owner/game/audit").await;
    let audit: api::AuditPage = serde_json::from_slice(&body).unwrap();
    assert_eq!(audit.entries.len(), 2);
    assert_eq!(
        audit.entries[0].previous.as_deref(),
        Some(audit.entries[1].hash.as_str())
    );
}

#[tokio::test]
async fn verify_reports_a_clean_repository_and_every_break() {
    let w = world().await;
    let (status, body) = get(&w.app, "/v1/repos/owner/game/verify").await;
    assert_eq!(status, StatusCode::OK);
    let report: api::VerifyReport = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (report.paths, report.revisions, report.audit_events),
        (1, 2, Some(2))
    );
    assert!(report.problems.is_empty());

    w.meta.tamper(
        &RepoId::new("t"),
        &RepoPath::new("a.txt").unwrap(),
        RevisionId(1),
        |r| r.message = "rewritten".into(),
    );
    let (_, body) = get(&w.app, "/v1/repos/owner/game/verify?path=a.txt").await;
    let report: api::VerifyReport = serde_json::from_slice(&body).unwrap();
    assert_eq!(report.audit_events, None);
    assert_eq!(report.problems.len(), 1);
    assert_eq!(report.problems[0].kind, "hash_mismatch");
    assert_eq!(report.problems[0].revision, Some(1));
}

#[tokio::test]
async fn a_tampered_revision_is_a_500_with_a_stable_code_and_a_missing_path_a_404() {
    let w = world().await;
    w.meta.tamper(
        &RepoId::new("t"),
        &RepoPath::new("a.txt").unwrap(),
        RevisionId(2),
        |r| r.message = "rewritten".into(),
    );
    let (status, body) = get(&w.app, "/v1/repos/owner/game/history?path=a.txt").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let err: api::ErrorBody = serde_json::from_slice(&body).unwrap();
    assert_eq!(err.code, "history_corrupt");
    assert!(!err.message.contains("a.txt"), "{}", err.message);

    let (status, _) = get(&w.app, "/v1/repos/owner/game/verify?path=nope.txt").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn checking_the_whole_repository_needs_a_signed_in_caller_even_when_it_is_public() {
    let w = world().await;
    w.meta
        .update_repo(
            &RepoId::new("t"),
            pyn_core::RepoUpdate {
                visibility: Some(Visibility::Public),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let anonymous = |uri: &'static str| {
        let app = w.app.clone();
        async move {
            let r = app
                .oneshot(Request::get(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            r.status()
        }
    };
    assert_eq!(
        anonymous("/v1/repos/owner/game/verify").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anonymous("/v1/repos/owner/game/verify?path=a.txt").await,
        StatusCode::OK
    );
    assert_eq!(
        anonymous("/v1/repos/owner/game/history?path=a.txt").await,
        StatusCode::OK
    );
    let (status, _) = get(&w.app, "/v1/repos/owner/game/verify").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn verify_takes_a_cursor_and_refuses_one_that_does_not_parse_or_fit() {
    let w = world().await;
    let (status, body) = get(&w.app, "/v1/repos/owner/game/verify?paged=true").await;
    assert_eq!(status, StatusCode::OK);
    let whole: api::VerifyReport = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        (whole.next_cursor, whole.paths, whole.revisions),
        (None, 1, 2)
    );

    let (status, body) = get(&w.app, "/v1/repos/owner/game/verify?paged=true&cursor=c:").await;
    assert_eq!(status, StatusCode::OK);
    let rest: api::VerifyReport = serde_json::from_slice(&body).unwrap();
    assert_eq!((rest.paths, rest.revisions, rest.commits), (0, 0, 2));
    assert_eq!(rest.audit_events, Some(2));

    for uri in [
        "/v1/repos/owner/game/verify?cursor=",
        "/v1/repos/owner/game/verify?cursor=garbage",
        "/v1/repos/owner/game/verify?cursor=r:0:a.txt",
        "/v1/repos/owner/game/verify?path=a.txt&cursor=h:",
        "/v1/repos/owner/game/verify?path=a.txt&cursor=r:1:b.txt",
    ] {
        let (status, body) = get(&w.app, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        let err: api::ErrorBody = serde_json::from_slice(&body).unwrap();
        assert_eq!(err.code, "invalid_request", "{uri}");
    }

    let (status, body) = get(
        &w.app,
        "/v1/repos/owner/game/verify?path=a.txt&cursor=r:2:a.txt",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let tail: api::VerifyReport = serde_json::from_slice(&body).unwrap();
    assert_eq!((tail.paths, tail.revisions, tail.next_cursor), (0, 1, None));
}

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{
    AuditAction, AuditQuery, BreakKind, ManualClock, ObjectStore, Principal, PynError, RepoId,
    RepoPath, RepoService, RevisionHash, RevisionId, Rules, ServiceConfig, UserId,
};

struct World {
    svc: RepoService,
    meta: Arc<MemoryMetadataStore>,
    audit: Arc<MemoryAuditStore>,
    objects: Arc<MemoryObjectStore>,
    repo: RepoId,
}

fn world() -> World {
    let meta = Arc::new(MemoryMetadataStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let objects = Arc::new(MemoryObjectStore::new());
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let repo = RepoId::new("game");
    let svc = RepoService::new(
        repo.clone(),
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
        meta.clone(),
        objects.clone(),
        audit.clone(),
        clock,
        ServiceConfig::default(),
    );
    World {
        svc,
        meta,
        audit,
        objects,
        repo,
    }
}

fn path(s: &str) -> RepoPath {
    RepoPath::new(s).unwrap()
}

fn alice() -> Principal {
    Principal::unrestricted(UserId::new("alice"))
}

async fn commit(w: &World, p: &RepoPath, text: &str) {
    let base = w.svc.head(p).await.unwrap().map(|r| r.id);
    let c = w.objects.put(text.as_bytes().to_vec()).await.unwrap();
    w.svc
        .checkin(&alice(), p, c, base, text.into())
        .await
        .unwrap();
}

async fn three_revisions(w: &World) -> RepoPath {
    let a = path("Source/a.cpp");
    for t in ["one", "two", "three"] {
        commit(w, &a, t).await;
    }
    a
}

fn is_corrupt(e: &PynError) -> bool {
    matches!(e, PynError::HistoryCorrupt(_))
}

#[tokio::test]
async fn an_untouched_repository_verifies_clean() {
    let w = world();
    three_revisions(&w).await;
    let report = w.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
    assert_eq!((report.paths, report.revisions), (1, 3));
    assert_eq!(report.audit_events, Some(3));
}

#[tokio::test]
async fn an_altered_message_is_found_and_never_served() {
    let w = world();
    let a = three_revisions(&w).await;
    w.meta.tamper(&w.repo, &a, RevisionId(2), |r| {
        r.message = "rewritten".into()
    });

    let report = w.svc.verify(None, false).await.unwrap();
    assert_eq!(report.problems.len(), 1);
    let p = &report.problems[0];
    assert_eq!(
        (p.kind, p.revision, p.path.as_ref()),
        (BreakKind::Hash, Some(RevisionId(2)), Some(&a))
    );
    assert!(is_corrupt(&w.svc.history(&a).await.unwrap_err()));
    assert!(is_corrupt(
        &w.svc.read(&a, Some(RevisionId(2))).await.unwrap_err()
    ));
    assert!(w.svc.read(&a, Some(RevisionId(1))).await.is_ok());
}

#[tokio::test]
async fn a_rewritten_chain_is_found_where_it_stops_matching() {
    let w = world();
    let a = three_revisions(&w).await;
    // Rewriting r2 and re-sealing it leaves r3 pointing at the old hash.
    w.meta.tamper(&w.repo, &a, RevisionId(2), |r| {
        r.author = UserId::new("mallory");
        r.hash = r.expected_hash();
    });
    let report = w.svc.verify(Some(&a), false).await.unwrap();
    let found: Vec<_> = report
        .problems
        .iter()
        .map(|p| (p.kind, p.revision))
        .collect();
    assert_eq!(found, vec![(BreakKind::Link, Some(RevisionId(3)))]);
}

#[tokio::test]
async fn a_removed_or_reordered_revision_breaks_the_link() {
    let w = world();
    let a = three_revisions(&w).await;
    w.meta.tamper(&w.repo, &a, RevisionId(3), |r| {
        r.previous = Some(RevisionHash::new("0".repeat(64)));
        r.hash = r.expected_hash();
    });
    let report = w.svc.verify(None, false).await.unwrap();
    assert_eq!(report.problems.len(), 1);
    assert_eq!(report.problems[0].kind, BreakKind::Link);
}

#[tokio::test]
async fn a_checkin_cannot_extend_a_tampered_head() {
    let w = world();
    let a = three_revisions(&w).await;
    w.meta
        .tamper(&w.repo, &a, RevisionId(3), |r| r.message = "x".into());
    let c = w.objects.put(b"four".to_vec()).await.unwrap();
    let err = w
        .svc
        .checkin(&alice(), &a, c, Some(RevisionId(3)), "four".into())
        .await
        .unwrap_err();
    assert!(is_corrupt(&err), "{err}");
    assert!(w.svc.head(&a).await.is_err());
}

#[tokio::test]
async fn an_altered_audit_event_is_found_and_never_served() {
    let w = world();
    three_revisions(&w).await;
    let events = w
        .svc
        .audit(&AuditQuery {
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].action, AuditAction::Checkin);

    w.audit.tamper(2, |e| e.actor = UserId::new("mallory"));
    let report = w.svc.verify(None, true).await.unwrap();
    assert_eq!(report.audit_events, Some(3));
    assert_eq!(report.problems.len(), 1);
    assert_eq!(
        (report.problems[0].kind, report.problems[0].event),
        (BreakKind::Hash, Some(2))
    );
    let err = w
        .svc
        .audit(&AuditQuery {
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(is_corrupt(&err));
}

#[tokio::test]
async fn a_deleted_audit_event_breaks_the_chain() {
    let w = world();
    three_revisions(&w).await;
    w.audit.remove(2);
    let report = w.svc.verify(None, true).await.unwrap();
    assert_eq!(report.audit_events, Some(2));
    assert_eq!(
        report
            .problems
            .iter()
            .map(|p| (p.kind, p.event))
            .collect::<Vec<_>>(),
        vec![(BreakKind::Link, Some(3))]
    );
}

#[tokio::test]
async fn verifying_an_unknown_path_is_not_found() {
    let w = world();
    let err = w.svc.verify(Some(&path("nope")), false).await.unwrap_err();
    assert!(matches!(err, PynError::PathNotFound(_)), "{err}");
}

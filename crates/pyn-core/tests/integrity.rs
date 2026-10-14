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
async fn a_base_that_is_not_an_older_revision_of_the_path_is_found() {
    let w = world();
    let a = three_revisions(&w).await;
    let report = w.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
    for bad in [RevisionId(3), RevisionId(9)] {
        w.meta
            .tamper(&w.repo, &a, RevisionId(3), |r| r.base = Some(bad));
        let report = w.svc.verify(Some(&a), false).await.unwrap();
        let found: Vec<_> = report
            .problems
            .iter()
            .map(|p| (p.kind, p.revision))
            .collect();
        assert_eq!(found, vec![(BreakKind::Base, Some(RevisionId(3)))], "{bad}");
    }
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

#[test]
fn a_migrated_revision_still_verifies_against_its_frozen_v1_hash() {
    let mut rev = v1_revision();
    assert!(rev.verify().is_ok());
    rev.legacy_content = None;
    assert!(
        rev.verify().is_err(),
        "without the stored input the hash does not reproduce"
    );
}

#[tokio::test]
async fn a_tampered_commit_or_tree_is_reported_by_verify() {
    let w = world();
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    commit(&w, &a, "one").await;
    commit(&w, &b, "two").await;
    let head = w.svc.head_commit().await.unwrap().unwrap();
    let first = w.svc.commits(None, None).await.unwrap().commits[1].id;

    w.meta
        .tamper_commit(&w.repo, &first, |c| c.message = "forged".into());
    let report = w.svc.verify(None, false).await.unwrap();
    let kinds: Vec<_> = report.problems.iter().map(|p| (p.kind, p.commit)).collect();
    assert_eq!(kinds, vec![(BreakKind::Hash, Some(first))]);
    assert!(w.svc.get_commit(&first).await.is_err());

    let w = world();
    commit(&w, &a, "one").await;
    let only = w.svc.head_commit().await.unwrap().unwrap();
    w.meta.tamper_commit(&w.repo, &only, |c| c.tree = only);
    let report = w.svc.verify(None, false).await.unwrap();
    assert!(
        report.problems.iter().any(|p| p.kind == BreakKind::Hash),
        "{report:?}"
    );
    assert_ne!(head, only);
}

#[tokio::test]
async fn a_v2_revision_covers_its_file_mode_and_commit() {
    let w = world();
    let a = path("Source/a.cpp");
    commit(&w, &a, "one").await;
    let rev = w.svc.head(&a).await.unwrap().unwrap();
    assert!(rev.person.is_some());
    w.meta.tamper(&w.repo, &a, rev.id, |r| {
        r.file_mode = pyn_core::FileMode::Executable;
    });
    assert!(w.svc.head(&a).await.is_err());
    w.meta.tamper(&w.repo, &a, rev.id, |r| {
        r.file_mode = pyn_core::FileMode::Regular;
        r.commit = r.content;
    });
    assert!(w.svc.head(&a).await.is_err());
}

#[tokio::test]
async fn a_flipped_hash_version_is_a_version_problem_not_a_silent_reencode() {
    let w = world();
    let a = three_revisions(&w).await;
    w.meta.tamper(&w.repo, &a, RevisionId(2), |r| {
        assert_eq!(r.hash_version, 2);
        r.hash_version = 1;
    });
    let report = w.svc.verify(Some(&a), false).await.unwrap();
    let found: Vec<_> = report
        .problems
        .iter()
        .map(|p| (p.kind, p.revision))
        .collect();
    assert_eq!(found, vec![(BreakKind::Version, Some(RevisionId(2)))]);
    let err = w.svc.read(&a, Some(RevisionId(2))).await.unwrap_err();
    assert!(
        is_corrupt(&err)
            && err
                .to_string()
                .contains("version 1 but has a structured author"),
        "{err}"
    );
    assert!(is_corrupt(&w.svc.history(&a).await.unwrap_err()));

    w.meta.tamper(&w.repo, &a, RevisionId(2), |r| {
        r.hash_version = 2;
        r.person = None;
    });
    let report = w.svc.verify(Some(&a), false).await.unwrap();
    assert_eq!(report.problems[0].kind, BreakKind::Version);
    let err = w.svc.head(&a).await;
    assert!(err.is_ok(), "r3 is the head and is intact");
    let err = w.svc.read(&a, Some(RevisionId(2))).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("version 2 but has no structured author"),
        "{err}"
    );

    w.meta
        .tamper(&w.repo, &a, RevisionId(2), |r| r.hash_version = 3);
    let err = w.svc.read(&a, Some(RevisionId(2))).await.unwrap_err();
    assert!(err.to_string().contains("unknown version 3"), "{err}");
}

#[test]
fn a_v1_revision_with_a_structured_author_or_v2_without_is_inconsistent() {
    let mut rev = v1_revision();
    assert!(rev.version_problem().is_none());
    rev.hash_version = 2;
    assert!(rev.version_problem().is_some() && rev.verify().is_err());
    rev.hash_version = 1;
    rev.person = Some(pyn_core::Person::new("alice", "a@example.com", 0));
    assert!(rev.version_problem().is_some() && rev.verify().is_err());
}

fn v1_revision() -> pyn_core::Revision {
    let empty_blob = pyn_core::ObjectId::from_hex(
        pyn_core::REPO_ALGORITHM,
        "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813",
    )
    .unwrap();
    pyn_core::Revision {
        id: RevisionId(1),
        path: RepoPath::new("Content/a.umap").unwrap(),
        content: empty_blob,
        legacy_content: Some(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
        ),
        author: UserId::new("alice"),
        person: None,
        hash_version: 1,
        message: "first".into(),
        created_at: Utc.timestamp_micros(1_700_000_000_123_456).unwrap(),
        restored_from: None,
        mode: None,
        file_mode: pyn_core::FileMode::Regular,
        changeset: None,
        commit: empty_blob,
        hash: RevisionHash::new("9c4caa53e94d5fa8fb261cb765f73725506641df99e225605e83f179ae0d9b52"),
        previous: None,
        base: None,
    }
}

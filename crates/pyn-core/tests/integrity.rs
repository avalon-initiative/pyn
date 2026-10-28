use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{
    AuditAction, AuditQuery, BreakKind, ManualClock, Principal, PynError, RepoId, RepoPath,
    RepoService, RevisionHash, RevisionId, Rules, ServiceConfig, UserId,
};

struct World {
    svc: RepoService,
    meta: Arc<MemoryMetadataStore>,
    audit: Arc<MemoryAuditStore>,
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
        Rules::from_toml("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n")
            .unwrap(),
        meta.clone(),
        objects.clone(),
        audit.clone(),
        pyn_core::RepoServiceContext {
            owner: pyn_core::UserId::new("owner"),
            clock,
            limit_events: Arc::new(pyn_core::MemoryLimitEvents::new()),
        },
        ServiceConfig::default(),
    );
    World {
        svc,
        meta,
        audit,
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
    let c = w.svc.upload_blob(text.as_bytes().to_vec()).await.unwrap();
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
async fn a_base_that_is_not_the_branchs_pointer_is_found_on_any_branch() {
    let w = world();
    let map = path("Content/a.umap");
    let put = |branch: &'static str, text: &'static str, base: Option<u64>| {
        let (svc, map) = (&w.svc, map.clone());
        async move {
            let who = alice();
            svc.checkout_on(branch, &map, &who.user, base.map(RevisionId))
                .await
                .unwrap();
            let c = svc.upload_blob(text.as_bytes().to_vec()).await.unwrap();
            svc.checkin_on(
                branch,
                &who,
                &map,
                c,
                pyn_core::FileMode::Regular,
                base.map(RevisionId),
                text.into(),
            )
            .await
            .unwrap()
        }
    };
    put("main", "one", None).await;
    w.svc
        .create_branch(&alice(), "feature", None)
        .await
        .unwrap();
    put("feature", "two", Some(1)).await;
    put("main", "three", Some(1)).await;
    put("feature", "four", Some(2)).await;
    let report = w.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");

    // r3 and r4 each name an older revision of the path, but not their own branch's pointer.
    for (id, wrong) in [(3, 2), (4, 1)] {
        let original = w.svc.history(&map).await.unwrap()[id as usize - 1].base;
        w.meta.tamper(&w.repo, &map, RevisionId(id), |r| {
            r.base = Some(RevisionId(wrong))
        });
        let report = w.svc.verify(Some(&map), false).await.unwrap();
        let found: Vec<_> = report
            .problems
            .iter()
            .map(|p| (p.kind, p.revision))
            .collect();
        assert_eq!(
            found,
            vec![(BreakKind::Base, Some(RevisionId(id)))],
            "r{id}"
        );
        w.meta
            .tamper(&w.repo, &map, RevisionId(id), |r| r.base = original);
    }
    let report = w.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
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
    let c = w.svc.upload_blob(b"four".to_vec()).await.unwrap();
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
    w.meta.tamper_commit(&w.repo, &only, |c| c.raw.push(b'x'));
    let report = w.svc.verify(None, false).await.unwrap();
    assert_eq!(
        report.problems.iter().map(|p| p.kind).collect::<Vec<_>>(),
        vec![BreakKind::Hash]
    );
    assert!(w.svc.get_commit(&only).await.is_err());

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
async fn a_commit_pointer_cache_that_differs_from_what_the_commit_made_is_reported() {
    let w = world();
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    commit(&w, &a, "one").await;
    commit(&w, &b, "two").await;
    let second = w.svc.head_commit().await.unwrap().unwrap();
    let report = w.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");

    let kinds = |r: &pyn_core::VerifyReport| {
        r.problems
            .iter()
            .map(|p| (p.kind, p.commit))
            .collect::<Vec<_>>()
    };
    let expected = (BreakKind::Pointer, Some(second));
    // a pointer the commit did not make, one missing, and one twice
    for case in 0..3 {
        let mut original = Vec::new();
        w.meta
            .tamper_pointers(&w.repo, &second, |v| original = v.clone());
        w.meta.tamper_pointers(&w.repo, &second, |v| match case {
            0 => v.push((path("Source/a.cpp"), RevisionId(1))),
            1 => v.clear(),
            _ => v.push(v[0].clone()),
        });
        let report = w.svc.verify(None, false).await.unwrap();
        assert!(
            kinds(&report).contains(&expected),
            "case {case}: {report:?}"
        );
        w.meta.tamper_pointers(&w.repo, &second, |v| *v = original);
    }
    let report = w.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
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
        .tamper(&w.repo, &a, RevisionId(2), |r| r.hash_version = 4);
    let err = w.svc.read(&a, Some(RevisionId(2))).await.unwrap_err();
    assert!(err.to_string().contains("unknown version 4"), "{err}");
}

#[tokio::test]
async fn a_deletion_revision_is_v3_and_its_flag_cannot_be_flipped() {
    let w = world();
    let a = three_revisions(&w).await;
    let removed = w
        .svc
        .checkin_changes_on(
            "main",
            &alice(),
            Vec::new(),
            vec![pyn_core::Deletion {
                path: a.clone(),
                base: Some(RevisionId(3)),
            }],
            "remove".into(),
        )
        .await
        .unwrap();
    assert_eq!((removed[0].hash_version, removed[0].deleted), (3, true));
    let report = w.svc.verify(Some(&a), false).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);

    w.meta
        .tamper(&w.repo, &a, RevisionId(4), |r| r.deleted = false);
    let report = w.svc.verify(Some(&a), false).await.unwrap();
    assert_eq!(report.problems[0].kind, BreakKind::Version);
    w.meta.tamper(&w.repo, &a, RevisionId(4), |r| {
        r.deleted = true;
        r.hash_version = 2;
    });
    let report = w.svc.verify(Some(&a), false).await.unwrap();
    assert_eq!(report.problems[0].kind, BreakKind::Version);
    w.meta.tamper(&w.repo, &a, RevisionId(4), |r| {
        r.hash_version = 3;
        r.content = removed[0].content;
        r.message = "other".into();
    });
    let report = w.svc.verify(Some(&a), false).await.unwrap();
    assert_eq!(report.problems[0].kind, BreakKind::Hash);
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
        deleted: false,
        changeset: None,
        commit: empty_blob,
        hash: RevisionHash::new("9c4caa53e94d5fa8fb261cb765f73725506641df99e225605e83f179ae0d9b52"),
        previous: None,
        base: None,
    }
}

#[tokio::test]
async fn a_path_history_page_checks_its_revisions_and_the_link_to_the_page_before() {
    let w = world();
    let a = three_revisions(&w).await;
    w.meta.tamper(&w.repo, &a, RevisionId(3), |r| {
        r.previous = Some(RevisionHash::new("00"));
    });
    let page = |after: Option<u64>| {
        w.svc
            .history_page_on("main", &a, after.map(RevisionId), Some(1))
    };
    assert_eq!(page(None).await.unwrap().next, Some(RevisionId(1)));
    assert_eq!(page(Some(1)).await.unwrap().next, Some(RevisionId(2)));
    let err = page(Some(2)).await.unwrap_err();
    assert!(is_corrupt(&err) && err.to_string().contains("r3"), "{err}");

    w.meta.tamper(&w.repo, &a, RevisionId(3), |r| {
        r.previous = Some(RevisionHash::new("00"));
    });
    w.meta.tamper(&w.repo, &a, RevisionId(2), |r| {
        r.message = "rewritten".into()
    });
    assert!(page(None).await.is_ok());
    assert!(is_corrupt(&page(Some(1)).await.unwrap_err()));
}

async fn stepped(
    w: &World,
    only: Option<&RepoPath>,
    audit: bool,
    budget: usize,
) -> (pyn_core::VerifyReport, usize) {
    let mut total = pyn_core::VerifyReport::default();
    let (mut cursor, mut steps) = (None, 0);
    loop {
        let step = w
            .svc
            .verify_step(only, audit, cursor.as_deref(), budget)
            .await
            .unwrap();
        steps += 1;
        assert!(steps < 1000, "no progress at budget {budget}");
        total.paths += step.paths;
        total.revisions += step.revisions;
        total.commits += step.commits;
        total.audit_events = match (total.audit_events, step.audit_events) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        total.problems.extend(step.problems);
        match step.next {
            Some(next) => cursor = Some(next),
            None => return (total, steps),
        }
    }
}

fn summary(r: &pyn_core::VerifyReport) -> (Vec<u64>, Vec<String>) {
    let mut problems: Vec<_> = r.problems.iter().map(|p| format!("{p:?}")).collect();
    problems.sort();
    (
        vec![
            r.paths,
            r.revisions,
            r.commits,
            r.audit_events.unwrap_or(u64::MAX),
        ],
        problems,
    )
}

#[tokio::test]
async fn a_stepped_verify_finds_what_one_pass_finds_at_every_budget() {
    let w = world();
    let a = three_revisions(&w).await;
    let (b, c) = (path("Source/b.cpp"), path("Docs/c.md"));
    for t in ["x", "y"] {
        commit(&w, &b, t).await;
        commit(&w, &c, t).await;
    }
    let clean = w.svc.verify(None, true).await.unwrap();
    assert!(clean.problems.is_empty(), "{clean:?}");
    assert_eq!((clean.paths, clean.revisions), (3, 7));
    for budget in 1..=40 {
        let (got, _) = stepped(&w, None, true, budget).await;
        assert_eq!(summary(&got), summary(&clean), "clean, budget {budget}");
    }

    let first = w.svc.commits(None, None).await.unwrap().commits[3].id;
    w.meta
        .tamper(&w.repo, &a, RevisionId(2), |r| r.message = "x".into());
    w.meta.tamper(&w.repo, &b, RevisionId(2), |r| {
        r.previous = Some(RevisionHash::new("0".repeat(64)));
        r.hash = r.expected_hash();
    });
    w.audit.remove(4);
    w.meta
        .tamper_commit(&w.repo, &first, |c| c.message = "forged".into());
    let whole = w.svc.verify(None, true).await.unwrap();
    assert!(whole.problems.len() >= 4, "{whole:?}");
    let (single, steps) = stepped(&w, None, true, 100_000).await;
    assert_eq!((steps, summary(&single)), (1, summary(&whole)));
    let mut most = 0;
    for budget in 1..=40 {
        let (got, steps) = stepped(&w, None, true, budget).await;
        assert_eq!(summary(&got), summary(&whole), "budget {budget}");
        most = most.max(steps);
    }
    assert!(most >= 10, "a tiny budget takes many steps, took {most}");

    let (no_audit, _) = stepped(&w, None, false, 3).await;
    assert_eq!(no_audit.audit_events, None);
    assert!(no_audit.problems.iter().all(|p| p.event.is_none()));
}

#[tokio::test]
async fn a_path_check_resumes_inside_the_path() {
    let w = world();
    let a = path("Source/a.cpp");
    for t in ["1", "2", "3", "4", "5"] {
        commit(&w, &a, t).await;
    }
    w.meta
        .tamper(&w.repo, &a, RevisionId(4), |r| r.message = "x".into());
    for budget in 1..=7 {
        let (got, steps) = stepped(&w, Some(&a), false, budget).await;
        assert_eq!((got.paths, got.revisions), (1, 5), "budget {budget}");
        let found: Vec<_> = got.problems.iter().map(|p| (p.kind, p.revision)).collect();
        assert_eq!(found, vec![(BreakKind::Hash, Some(RevisionId(4)))]);
        assert_eq!(steps, 5usize.div_ceil(budget).max(1), "budget {budget}");
    }
}

#[tokio::test]
async fn verify_cursors_must_parse_and_belong_to_the_request() {
    let w = world();
    let a = three_revisions(&w).await;
    for bad in [
        "",
        "x",
        "h",
        "z:1",
        "r:0:Source/a.cpp",
        "r:x:Source/a.cpp",
        "r:1",
        "r:1:/abs",
        "c:zz",
        "t:x",
        "t:-1",
        "a:0",
        "a:-3",
        "a:x",
    ] {
        let err = w
            .svc
            .verify_step(None, true, Some(bad), 10)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad:?}: {err}");
    }
    for foreign in ["h:", "c:", "r:1:Source/other.cpp"] {
        let err = w
            .svc
            .verify_step(Some(&a), false, Some(foreign), 10)
            .await
            .unwrap_err();
        assert!(
            matches!(err, PynError::InvalidRequest(_)),
            "{foreign:?}: {err}"
        );
    }
    let ok = w
        .svc
        .verify_step(Some(&a), false, Some("r:3:Source/a.cpp"), 10)
        .await
        .unwrap();
    assert_eq!((ok.revisions, ok.next), (1, None));
    let past = w
        .svc
        .verify_step(None, false, Some("h:Zzz"), 10)
        .await
        .unwrap();
    assert_eq!((past.paths, past.next), (0, None), "after the last path");
}

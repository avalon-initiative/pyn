//! Limit refusals keep their errors and queue owner events through the shared webhook store.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, Clock, Credential, DeliveryQuery, Identity, Limits, ManualClock,
    NewSubscription, OrgRole, PynError, Registration, RegistrationMode, RepoPath, Repositories,
    Rules, UserId, WebhookSettings, WebhookStore,
};

use pyn_proto::{Event, EventData, LimitKind, OwnerLimitReachedData};

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    hooks: Arc<MemoryWebhookStore>,
    clock: Arc<ManualClock>,
}

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

fn world(limits: Limits, upload_cap: u64) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
    ));
    let hooks = Arc::new(MemoryWebhookStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_server::events::WebhookLimitEvents::new(
                hooks.clone(),
                "https://pyn.example".into(),
            )),
        )
        .with_config(AccessConfig {
            default_limits: limits,
            public_url: "https://pyn.example".into(),
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone())
        .with_webhooks(hooks.clone(), WebhookSettings::default()),
    );
    let repos = Repositories::new(
        meta,
        Arc::new(pyn_core::ChunkedStore::new(Arc::new(
            MemoryObjectStore::new(),
        ))),
        audit,
        access.clone(),
        clock.clone(),
        Rules::from_toml("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n")
            .unwrap(),
    )
    .with_default_max_locks(1)
    .unwrap()
    .with_unreferenced_upload_cap(upload_cap);
    World {
        repos,
        access,
        hooks,
        clock,
    }
}

async fn subscribe(w: &World) -> i64 {
    w.hooks
        .create_subscription(
            NewSubscription {
                name: "limits".into(),
                url: "https://example.test/hook".into(),
                event_types: vec!["owner.limit_reached".into()],
                enabled: true,
                created_by: UserId::new("root"),
            },
            w.clock.now(),
        )
        .await
        .unwrap()
        .id
}

async fn deliveries(w: &World, id: i64) -> usize {
    w.hooks
        .list_deliveries(
            id,
            &DeliveryQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn repository_refusals_are_throttled_using_the_injected_clock() {
    let w = world(
        Limits {
            repositories: Some(1),
            ..Default::default()
        },
        100,
    );
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "existing", None, None)
        .await
        .unwrap();
    let sub = subscribe(&w).await;
    for (advance, expected) in [
        (Duration::zero(), 1),
        (Duration::zero(), 1),
        (Duration::hours(1) - Duration::microseconds(1), 1),
        (Duration::microseconds(1), 2),
        (Duration::hours(1) + Duration::microseconds(1), 3),
    ] {
        w.clock.advance(advance);
        let err = w
            .repos
            .create(&who("alice"), &UserId::new("alice"), "no", None, None)
            .await
            .unwrap_err();
        assert!(matches!(&err, PynError::RepoLimitReached { owner, limit: 1 } if owner == "alice"));
        assert_eq!(err.code(), "repo_limit_reached");
        assert_eq!(
            err.to_string(),
            "repository limit reached: alice may own at most 1 repositories on this server"
        );
        assert_eq!(deliveries(&w, sub).await, expected);
    }
}

#[tokio::test]
async fn members_storage_and_non_owner_locks_keep_their_errors_and_attribute_the_resource_owner() {
    let w = world(
        Limits {
            members: Some(1),
            storage_bytes: Some(3),
            ..Default::default()
        },
        100,
    );
    let sub = subscribe(&w).await;
    w.access
        .register(Registration::new("bob", "correct horse battery"))
        .await
        .unwrap();
    w.access.create_org(&who("alice"), "studio").await.unwrap();
    for _ in 0..2 {
        let err = w
            .access
            .add_org_member(
                &who("alice"),
                &UserId::new("studio"),
                &UserId::new("bob"),
                OrgRole::Member,
            )
            .await
            .unwrap_err();
        assert!(matches!(&err, PynError::MemberLimitReached { org, limit: 1 } if org == "studio"));
        assert_eq!(err.code(), "member_limit_reached");
        assert_eq!(
            err.to_string(),
            "member limit reached: studio may have at most 1 members on this server"
        );
    }
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    open.service
        .checkout(
            &RepoPath::new("Content/one").unwrap(),
            &UserId::new("bob"),
            None,
        )
        .await
        .unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    let content = open.service.upload_blob(b"four".to_vec()).await.unwrap();
    for _ in 0..2 {
        let err = open
            .service
            .checkout(
                &RepoPath::new("Content/two").unwrap(),
                &UserId::new("bob"),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::LockLimitReached { limit: 1 }));
        assert_eq!(err.code(), "lock_limit_reached");
        assert_eq!(
            err.to_string(),
            "lock limit reached: you already hold 1 lock(s) in this repository; unlock one before locking another"
        );
        let err = open
            .service
            .checkin(
                &principal,
                &RepoPath::new("file").unwrap(),
                content,
                None,
                "m".into(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, PynError::StorageLimitReached { owner, limit: 3, used: 0 } if owner == "alice")
        );
        assert_eq!(err.code(), "storage_limit_reached");
        assert_eq!(
            err.to_string(),
            "storage limit reached: alice may store at most 3 bytes on this server and uses 0"
        );
    }
    assert_eq!(deliveries(&w, sub).await, 3);
    let due = w
        .hooks
        .claim_due(100, Duration::minutes(1), w.clock.now())
        .await
        .unwrap();
    assert_eq!(due.len(), 3);
    let mut data = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for item in due {
        let event: Event = serde_json::from_str(&item.event.payload).unwrap();
        assert_eq!(item.event.event_type, "owner.limit_reached");
        assert_eq!(event.event_type, item.event.event_type);
        assert_eq!(event.created_at, w.clock.now());
        assert_eq!(event.server, "https://pyn.example");
        assert_eq!(event.version, 1);
        assert!(!event.id.is_empty());
        assert!(ids.insert(event.id));
        let mut body =
            serde_json::from_str::<serde_json::Value>(&item.event.payload).unwrap()["data"].clone();
        let owner_id = body.as_object_mut().unwrap().remove("owner_id").unwrap();
        assert!(owner_id.as_str().unwrap().len() == 25);
        data.push(body);
    }
    for (owner, limit, value) in [
        ("studio", "members", 1),
        ("alice", "locks", 1),
        ("alice", "storage_bytes", 3),
    ] {
        assert!(
            data.contains(&serde_json::json!({ "owner": owner, "limit": limit, "value": value }))
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn independent_services_racing_a_limit_record_exactly_one_event() {
    let hooks = Arc::new(MemoryWebhookStore::new());
    pyn_core::contract::racing_limit_services_record_one_event(
        Arc::new(MemoryMetadataStore::new()),
        Arc::new(MemoryAccessStore::new()),
        [hooks.clone(), hooks],
    )
    .await;
}

fn assert_storage_error(err: &PynError, owner: &str, limit: u64, used: u64) {
    assert!(
        matches!(err, PynError::StorageLimitReached { owner: actual_owner, limit: actual_limit, used: actual_used }
        if actual_owner == owner && *actual_limit == limit && *actual_used == used),
        "{err}"
    );
    assert_eq!(err.code(), "storage_limit_reached");
    assert_eq!(
        err.to_string(),
        format!(
            "storage limit reached: {owner} may store at most {limit} bytes on this server and uses {used}"
        )
    );
}

async fn assert_one_storage_event(w: &World, subscription: i64, limit: u64) {
    assert_eq!(
        deliveries(w, subscription).await,
        1,
        "storage refusal must queue one limit event"
    );
    let due = w
        .hooks
        .claim_due(10, Duration::minutes(1), w.clock.now())
        .await
        .unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].subscription.id, subscription);
    assert_eq!(due[0].event.event_type, "owner.limit_reached");
    let payload: serde_json::Value = serde_json::from_str(&due[0].event.payload).unwrap();
    assert_eq!(payload["type"], "owner.limit_reached");
    let mut data = payload["data"].clone();
    assert_eq!(
        data.as_object_mut()
            .unwrap()
            .remove("owner_id")
            .unwrap()
            .as_str()
            .unwrap()
            .len(),
        25
    );
    assert_eq!(
        data,
        serde_json::json!({"owner": "alice", "limit": "storage_bytes", "value": limit})
    );
}

#[tokio::test]
async fn push_storage_refusal_records_the_owner_limit_event() {
    use pyn_core::commit_record::{TreeChange, tree_of};
    use pyn_core::{FileMode, PushObjects};
    use pyn_object::{Algorithm, Kind, Signature, hash_object};

    let w = world(
        Limits {
            storage_bytes: Some(3),
            ..Default::default()
        },
        100,
    );
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    let blob = open.service.upload_blob(b"four".to_vec()).await.unwrap();
    let tree = tree_of(vec![TreeChange {
        path: RepoPath::new("file").unwrap(),
        mode: FileMode::Regular,
        id: blob,
        removed: false,
    }])
    .unwrap();
    let signature = Signature {
        name: b"alice".to_vec(),
        email: b"alice@example.test".to_vec(),
        time: w.clock.now().timestamp(),
        offset_minutes: 0,
    };
    let raw = pyn_object::Commit {
        tree: tree.root,
        parents: vec![],
        author: signature.clone(),
        committer: signature,
        extra_headers: vec![],
        message: b"push\n".to_vec(),
    }
    .encode(Algorithm::Sha256)
    .unwrap();
    let id = hash_object(Algorithm::Sha256, Kind::Commit, &raw).unwrap();
    let sub = subscribe(&w).await;
    for _ in 0..2 {
        let err = open
            .service
            .push_commits(
                &principal,
                "main",
                PushObjects {
                    commits: vec![(id, raw.clone())],
                    trees: tree.trees.clone(),
                },
            )
            .await
            .unwrap_err();
        assert_storage_error(&err, "alice", 3, 0);
    }
    assert_one_storage_event(&w, sub, 3).await;
}

#[tokio::test]
async fn changeset_storage_refusal_records_the_owner_limit_event() {
    use pyn_core::{ChangeEntry, FileMode};

    let w = world(
        Limits {
            storage_bytes: Some(3),
            ..Default::default()
        },
        100,
    );
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    let a = open.service.upload_blob(b"ab".to_vec()).await.unwrap();
    let b = open.service.upload_blob(b"cd".to_vec()).await.unwrap();
    let sub = subscribe(&w).await;
    for _ in 0..2 {
        let entries = [("a", a), ("b", b)]
            .into_iter()
            .map(|(path, content)| ChangeEntry {
                path: RepoPath::new(path).unwrap(),
                content,
                file_mode: FileMode::Regular,
                base: None,
            })
            .collect();
        let err = open
            .service
            .checkin_changeset(&principal, entries, "write two paths".into())
            .await
            .unwrap_err();
        assert_storage_error(&err, "alice", 3, 2);
    }
    assert_one_storage_event(&w, sub, 3).await;
}

#[tokio::test]
async fn merge_storage_refusal_records_the_owner_limit_event() {
    use pyn_core::{FastForward, FileMode, LimitsChange, MergeRequest};

    let w = world(Limits::default(), 100);
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    let path = RepoPath::new("file").unwrap();
    let content = open
        .service
        .upload_blob(b"a\nb\nc\n".to_vec())
        .await
        .unwrap();
    let base = open
        .service
        .checkin(&principal, &path, content, None, "base".into())
        .await
        .unwrap();
    open.service
        .create_branch(&principal, "feature", Some("main"))
        .await
        .unwrap();
    for (branch, text) in [("feature", "A\nb\nc\n"), ("main", "a\nb\nC\n")] {
        let content = open
            .service
            .upload_blob(text.as_bytes().to_vec())
            .await
            .unwrap();
        open.service
            .checkin_on(
                branch,
                &principal,
                &path,
                content,
                FileMode::Regular,
                Some(base.id),
                "change".into(),
            )
            .await
            .unwrap();
    }
    w.access
        .admin_for_tests(&UserId::new("root"))
        .await
        .unwrap();
    w.access
        .set_owner_limits(
            &who("root"),
            &UserId::new("alice"),
            LimitsChange {
                repositories: None,
                members: None,
                storage_bytes: Some(Some(18)),
            },
        )
        .await
        .unwrap();
    let sub = subscribe(&w).await;
    for _ in 0..2 {
        let err = open
            .service
            .merge(
                &principal,
                MergeRequest {
                    source: "feature".into(),
                    target: Some("main".into()),
                    fast_forward: FastForward::Never,
                    message: None,
                    expect_target: None,
                    resolutions: vec![],
                },
            )
            .await
            .unwrap_err();
        assert_storage_error(&err, "alice", 18, 18);
    }
    assert_one_storage_event(&w, sub, 18).await;
}

#[tokio::test]
async fn an_upload_refused_at_reservation_queues_nothing_and_leaves_the_storage_bytes_slot_free() {
    use pyn_core::LimitsChange;

    let w = world(Limits::default(), 2);
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    let kept = open.service.upload_blob(b"ab".to_vec()).await.unwrap();
    let sub = subscribe(&w).await;
    let err = open.service.upload_blob(b"c".to_vec()).await.unwrap_err();
    assert_storage_error(&err, &open.record.id.to_string(), 2, 2);
    assert_eq!(deliveries(&w, sub).await, 0);

    w.access
        .admin_for_tests(&UserId::new("root"))
        .await
        .unwrap();
    w.access
        .set_owner_limits(
            &who("root"),
            &UserId::new("alice"),
            LimitsChange {
                repositories: None,
                members: None,
                storage_bytes: Some(Some(1)),
            },
        )
        .await
        .unwrap();
    let err = open
        .service
        .checkin(
            &principal,
            &RepoPath::new("file").unwrap(),
            kept,
            None,
            "m".into(),
        )
        .await
        .unwrap_err();
    assert_storage_error(&err, "alice", 1, 0);
    assert_one_storage_event(&w, sub, 1).await;
}

#[tokio::test]
async fn an_upload_refused_at_recording_queues_nothing_and_leaves_the_storage_bytes_slot_free() {
    use pyn_core::LimitsChange;
    use pyn_core::object::Keying;
    use std::future::{Future, poll_fn};
    use std::task::Poll;
    use tokio::io::AsyncWriteExt;

    let w = world(Limits::default(), 3);
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let (mut writer, reader) = tokio::io::duplex(2);
    let mut uploading =
        Box::pin(
            open.service
                .upload(Box::pin(reader), Keying::Blob { len: 2 }, None, 2),
        );
    poll_fn(|cx| {
        assert!(uploading.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;

    w.clock.advance(Duration::hours(6) + Duration::seconds(1));
    let kept = open.service.upload_blob(b"cd".to_vec()).await.unwrap();
    let sub = subscribe(&w).await;
    writer.write_all(b"ab").await.unwrap();
    drop(writer);
    let err = uploading.await.unwrap_err();
    assert_storage_error(&err, &open.record.id.to_string(), 3, 2);
    assert_eq!(deliveries(&w, sub).await, 0);

    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    w.access
        .admin_for_tests(&UserId::new("root"))
        .await
        .unwrap();
    w.access
        .set_owner_limits(
            &who("root"),
            &UserId::new("alice"),
            LimitsChange {
                repositories: None,
                members: None,
                storage_bytes: Some(Some(1)),
            },
        )
        .await
        .unwrap();
    let err = open
        .service
        .checkin(
            &principal,
            &RepoPath::new("file").unwrap(),
            kept,
            None,
            "m".into(),
        )
        .await
        .unwrap_err();
    assert_storage_error(&err, "alice", 1, 0);
    assert_one_storage_event(&w, sub, 1).await;
}

#[tokio::test]
async fn an_assembly_refused_at_recording_queues_nothing_and_leaves_the_storage_bytes_slot_free() {
    use pyn_core::LimitsChange;
    use pyn_core::object::Keying;

    let w = world(Limits::default(), 3);
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    let chunk = open
        .service
        .upload(
            Box::pin(std::io::Cursor::new(b"ab".to_vec())),
            Keying::Raw,
            None,
            2,
        )
        .await
        .unwrap();
    let sub = subscribe(&w).await;
    let err = open
        .service
        .assemble(&[chunk, chunk], None)
        .await
        .unwrap_err();
    assert_storage_error(&err, &open.record.id.to_string(), 3, 0);
    assert_eq!(deliveries(&w, sub).await, 0);

    w.access
        .admin_for_tests(&UserId::new("root"))
        .await
        .unwrap();
    w.access
        .set_owner_limits(
            &who("root"),
            &UserId::new("alice"),
            LimitsChange {
                repositories: None,
                members: None,
                storage_bytes: Some(Some(1)),
            },
        )
        .await
        .unwrap();
    let err = open
        .service
        .checkin(
            &principal,
            &RepoPath::new("file").unwrap(),
            chunk,
            None,
            "m".into(),
        )
        .await
        .unwrap_err();
    assert_storage_error(&err, "alice", 1, 0);
    assert_one_storage_event(&w, sub, 1).await;
}

#[path = "common/failing_webhooks.rs"]
mod failing_webhooks;

#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn a_failed_webhook_append_preserves_the_limit_error_and_logs_the_loss() {
    use std::sync::atomic::Ordering;

    let logs = LogBuffer::default();
    let capture = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_writer(move || capture.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let hooks = Arc::new(failing_webhooks::FailingWebhooks::default());
    let meta = Arc::new(MemoryMetadataStore::new());
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
    ));
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_server::events::WebhookLimitEvents::new(
                hooks.clone(),
                "https://pyn.example".into(),
            )),
        )
        .with_config(AccessConfig {
            default_limits: Limits {
                repositories: Some(0),
                ..Default::default()
            },
            ..AccessConfig::default()
        })
        .with_registry(meta.clone()),
    );
    let repos = Repositories::new(
        meta,
        Arc::new(MemoryObjectStore::new()),
        Arc::new(MemoryAuditStore::new()),
        access,
        clock,
        Rules::empty(),
    );
    let err = repos
        .create(&who("alice"), &UserId::new("alice"), "refused", None, None)
        .await
        .unwrap_err();
    assert!(matches!(&err, PynError::RepoLimitReached { owner, limit: 0 } if owner == "alice"));
    assert_eq!(err.code(), "repo_limit_reached");
    assert_eq!(
        err.to_string(),
        "repository limit reached: alice may own at most 0 repositories on this server"
    );
    assert_eq!(hooks.attempts.load(Ordering::SeqCst), 1);
    let output = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(output.contains("WARN"), "{output}");
    assert!(output.contains("outbox unavailable"), "{output}");
    assert!(output.contains("alice"), "{output}");
    assert!(output.contains("Repositories"), "{output}");
}

#[tokio::test]
async fn domain_kinds_use_the_matching_wire_kind_and_admission_key() {
    use pyn_core::{LimitEvents, LimitKind as DomainKind, LimitReached};
    use pyn_server::events::WebhookLimitEvents;

    let now = Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap();
    for (domain, wire, key) in [
        (
            DomainKind::Repositories,
            LimitKind::Repositories,
            "repositories",
        ),
        (DomainKind::Members, LimitKind::Members, "members"),
        (
            DomainKind::StorageBytes,
            LimitKind::StorageBytes,
            "storage_bytes",
        ),
        (DomainKind::Locks, LimitKind::Locks, "locks"),
    ] {
        let hooks = Arc::new(MemoryWebhookStore::new());
        let subscription = hooks
            .create_subscription(
                NewSubscription {
                    name: "limits".into(),
                    url: "https://example.test/hook".into(),
                    event_types: vec!["owner.limit_reached".into()],
                    enabled: true,
                    created_by: UserId::new("root"),
                },
                now,
            )
            .await
            .unwrap();
        let owner = UserId::new("alice");
        hooks
            .append_limit_reached(&owner, key, "{}", now)
            .await
            .unwrap()
            .unwrap();
        let recorder = WebhookLimitEvents::new(hooks.clone(), "https://pyn.example".into());
        let fact = LimitReached {
            owner: owner.clone(),
            owner_id: pyn_core::AccountId::new("ualice"),
            kind: domain,
            value: 7,
        };
        recorder.record(fact.clone(), now).await.unwrap();
        assert_eq!(
            hooks
                .list_deliveries(
                    subscription.id,
                    &DeliveryQuery {
                        limit: 10,
                        ..Default::default()
                    }
                )
                .await
                .unwrap()
                .len(),
            1
        );
        let later = now + Duration::hours(1);
        recorder.record(fact, later).await.unwrap();
        let due = hooks
            .claim_due(10, Duration::minutes(1), later)
            .await
            .unwrap();
        assert_eq!(due.len(), 2);
        let event: Event = serde_json::from_str(
            &due.iter()
                .find(|d| d.event.payload != "{}")
                .unwrap()
                .event
                .payload,
        )
        .unwrap();
        assert_eq!(
            event.data,
            EventData::OwnerLimitReached(OwnerLimitReachedData {
                owner: "alice".into(),
                owner_id: "ualice".into(),
                limit: wire,
                value: 7,
            })
        );
        assert_eq!(event.created_at, later);
        assert_eq!(event.server, "https://pyn.example");
    }
}

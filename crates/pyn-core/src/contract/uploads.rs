//! Uploaded objects are scoped to the repository that uploaded them, while the object store stays shared.

use std::sync::Arc;

use super::push::{Files, build, objects, set};
use super::{RULES, Store, blob, harness, harness_on, other_repo, path, who};
use crate::memory::MemoryObjectStore;
use crate::merge::{FastForward, MergeRequest, Resolution};
use crate::{
    ChunkedStore, FileMode, Keying, ObjectId, ObjectStore, PynError, RepoId, RepoService, Rules,
    ServiceConfig,
};

async fn chunk(svc: &RepoService, bytes: &'static [u8]) -> crate::Result<ObjectId> {
    svc.upload(
        Box::pin(std::io::Cursor::new(bytes)),
        Keying::Raw,
        None,
        bytes.len() as u64,
    )
    .await
}

pub async fn an_object_is_usable_only_by_the_repository_that_uploaded_it(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let a = path("Source/a.cpp");
    let c = blob(&h, "shared bytes").await;

    for err in [
        other
            .svc
            .checkin(&who("alice"), &a, c, None, "x".into())
            .await
            .unwrap_err(),
        other
            .svc
            .checkin_changeset(
                &who("alice"),
                vec![crate::ChangeEntry {
                    path: a.clone(),
                    content: c,
                    file_mode: FileMode::Regular,
                    base: None,
                }],
                "x".into(),
            )
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
    }
    assert!(other.svc.head(&a).await.unwrap().is_none());

    h.svc
        .checkin(&who("alice"), &a, c, None, "x".into())
        .await
        .unwrap();
    let read = other.svc.read_blob(&c, None).await.err().unwrap();
    assert!(matches!(read, PynError::ObjectNotFound(_)), "{read}");
}

pub async fn uploads_are_reported_missing_per_repository_and_dedup_in_the_store(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let c = blob(&h, "same bytes").await;
    let ghost = ObjectId::from_hex(pyn_object::Algorithm::Sha256, &"00".repeat(32)).unwrap();

    assert!(h.svc.missing_objects(&[c]).await.unwrap().is_empty());
    let from_other = other.svc.missing_objects(&[c, ghost]).await.unwrap();
    assert_eq!(from_other, vec![c, ghost]);
    assert_eq!(h.svc.missing_objects(&[ghost]).await.unwrap(), vec![ghost]);

    let again = other.svc.upload_blob(b"same bytes".to_vec()).await.unwrap();
    assert_eq!(again, c);
    assert!(other.svc.missing_objects(&[c]).await.unwrap().is_empty());
    other
        .svc
        .checkin(&who("alice"), &path("Source/a.cpp"), c, None, "x".into())
        .await
        .unwrap();

    let third = harness_on(
        h.store.clone(),
        h.objects.clone(),
        RepoId::new("third"),
        RULES,
    );
    assert_eq!(third.svc.missing_objects(&[c]).await.unwrap(), vec![c]);
}

pub async fn a_push_needs_its_blobs_uploaded_to_the_repository(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let c = blob(&h, "pushed").await;
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", c);
    let commit = build(&files, None, "one", 1);

    let err = other
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
    assert!(other.svc.head_of("main").await.unwrap().is_none());

    other.svc.upload_blob(b"pushed".to_vec()).await.unwrap();
    other
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap();
}

pub async fn a_merge_resolution_needs_content_the_repository_knows(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let bin = path("Source/img.bin");
    let put = |branch: &'static str, bytes: Vec<u8>, base: Option<u64>| {
        let (svc, bin) = (h.svc.clone(), bin.clone());
        async move {
            let c = svc.upload_blob(bytes).await.unwrap();
            let base = base.map(crate::RevisionId);
            svc.checkin_on(
                branch,
                &who("alice"),
                &bin,
                c,
                FileMode::Regular,
                base,
                "b".into(),
            )
            .await
            .unwrap()
        }
    };
    put("main", vec![0, 1, 2, 3], None).await;
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();
    put("feature", vec![0, 1, 2, 9], Some(1)).await;
    put("main", vec![0, 7, 2, 3], Some(1)).await;

    let request = |content| MergeRequest {
        source: "feature".into(),
        target: Some("main".into()),
        fast_forward: FastForward::Allow,
        message: None,
        expect_target: None,
        resolutions: vec![Resolution {
            path: bin.clone(),
            content,
            file_mode: FileMode::Regular,
        }],
    };
    let foreign = other.svc.upload_blob(vec![5, 5, 5]).await.unwrap();
    let err = h
        .svc
        .merge(&who("alice"), request(foreign))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");

    let own = h.svc.upload_blob(vec![5, 5, 5]).await.unwrap();
    assert_eq!(own, foreign);
    assert!(h.svc.merge(&who("alice"), request(own)).await.is_ok());
}

pub async fn assembling_needs_chunks_the_repository_uploaded(store: Store) {
    let shared: Arc<dyn ObjectStore> =
        Arc::new(ChunkedStore::new(Arc::new(MemoryObjectStore::new())));
    let make = |repo: &str| {
        let h = harness(store.clone());
        RepoService::new(
            RepoId::new(repo),
            Rules::from_toml(RULES).unwrap(),
            store.clone(),
            shared.clone(),
            h.audit.clone(),
            h.clock.clone(),
            ServiceConfig::default(),
        )
    };
    let (mine, theirs) = (make("game"), make("other"));
    let c1 = chunk(&mine, b"first half ").await.unwrap();
    let c2 = chunk(&mine, b"second half").await.unwrap();

    let err = theirs.assemble(&[c1, c2], None).await.unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
    let id = mine.assemble(&[c1, c2], None).await.unwrap();
    assert!(mine.missing_objects(&[id]).await.unwrap().is_empty());
    assert_eq!(
        theirs.missing_objects(&[id, c1]).await.unwrap(),
        vec![id, c1]
    );

    chunk(&theirs, b"first half ").await.unwrap();
    chunk(&theirs, b"second half").await.unwrap();
    assert_eq!(theirs.assemble(&[c1, c2], None).await.unwrap(), id);
}

pub async fn unreferenced_uploads_are_capped_per_repository(store: Store) {
    let h = harness(store.clone());
    let svc = RepoService::new(
        RepoId::new("capped"),
        Rules::from_toml(RULES).unwrap(),
        store.clone(),
        h.objects.clone(),
        h.audit.clone(),
        h.clock.clone(),
        ServiceConfig::default(),
    )
    .with_unreferenced_cap(10);
    let a = svc.upload_blob(b"abcdef".to_vec()).await.unwrap();
    assert_eq!(
        store
            .unreferenced_bytes(&RepoId::new("capped"))
            .await
            .unwrap(),
        6
    );

    let err = svc.upload_blob(b"ghijkl".to_vec()).await.unwrap_err();
    assert!(
        matches!(err, PynError::StorageLimitReached { limit: 10, .. }),
        "{err}"
    );
    assert_eq!(svc.upload_blob(b"abcdef".to_vec()).await.unwrap(), a);
    assert!(svc.upload_blob(b"gh".to_vec()).await.is_ok());

    svc.checkin(&who("alice"), &path("Source/a.cpp"), a, None, "x".into())
        .await
        .unwrap();
    assert_eq!(
        store
            .unreferenced_bytes(&RepoId::new("capped"))
            .await
            .unwrap(),
        2
    );
    svc.upload_blob(b"ghijkl".to_vec()).await.unwrap();
}

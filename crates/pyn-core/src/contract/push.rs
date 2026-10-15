//! Client-built commits pushed to a branch (decision #300), through `RepoService::push_commits`: every store must
//! take the whole push or none of it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use chrono::Duration;
use pyn_object::{Algorithm, Kind, Signature, hash_object};

use super::{
    Harness, Store, blob, capped_service, harness, harness_with, path, repo_record, user, who,
};
use crate::commit_record::{TreeChange, tree_of};
use crate::{FileMode, Mode, ObjectId, Permission, Principal, PushObjects, PynError, RevisionId};

type Files = BTreeMap<String, (FileMode, ObjectId)>;

/// A commit and the trees of its whole snapshot, as a client would build them.
struct Built {
    id: ObjectId,
    raw: Vec<u8>,
    trees: Vec<(ObjectId, Vec<u8>)>,
}

const T0: i64 = 1_790_000_000;

fn who_signed(at: i64) -> Signature {
    Signature {
        name: b"carol".to_vec(),
        email: b"carol@example.test".to_vec(),
        time: at,
        offset_minutes: 120,
    }
}

fn commit_raw(
    tree: ObjectId,
    parents: Vec<ObjectId>,
    message: &[u8],
    author: i64,
    committer: i64,
) -> Vec<u8> {
    pyn_object::Commit {
        tree,
        parents,
        author: who_signed(author),
        committer: who_signed(committer),
        extra_headers: vec![],
        message: message.to_vec(),
    }
    .encode(Algorithm::Sha256)
    .unwrap()
}

fn built_from(raw: Vec<u8>, trees: Vec<(ObjectId, Vec<u8>)>) -> Built {
    let id = hash_object(Algorithm::Sha256, Kind::Commit, &raw).unwrap();
    Built { id, raw, trees }
}

fn snapshot_tree(files: &Files) -> crate::commit_record::TreeBuild {
    tree_of(
        files
            .iter()
            .map(|(p, (mode, id))| TreeChange {
                path: path(p),
                mode: *mode,
                id: *id,
            })
            .collect(),
    )
    .unwrap()
}

fn build(files: &Files, parent: Option<ObjectId>, message: &str, n: i64) -> Built {
    let tree = snapshot_tree(files);
    let raw = commit_raw(
        tree.root,
        parent.into_iter().collect(),
        format!("{message}\n").as_bytes(),
        T0 + n,
        T0 + n,
    );
    built_from(raw, tree.trees)
}

fn objects(parts: &[&Built]) -> PushObjects {
    PushObjects {
        commits: parts.iter().map(|b| (b.id, b.raw.clone())).collect(),
        trees: parts.iter().flat_map(|b| b.trees.clone()).collect(),
    }
}

fn set(files: &mut Files, p: &str, id: ObjectId) {
    files.insert(p.to_string(), (FileMode::Regular, id));
}

async fn head(h: &Harness, branch: &str) -> Option<ObjectId> {
    h.svc.head_of(branch).await.unwrap()
}

async fn absent(h: &Harness, ids: &[ObjectId]) {
    for id in ids {
        let err = h.svc.get_commit(id).await.unwrap_err();
        assert!(matches!(err, PynError::CommitNotFound(_)), "{err}");
    }
}

pub async fn a_push_lands_its_commits_under_their_own_ids(store: Store) {
    let h = harness(store);
    let (a, b) = (blob(&h, "a1").await, blob(&h, "b1").await);
    let a2 = blob(&h, "a2").await;
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", a);
    set(&mut files, "Source/b.cpp", b);
    let first = build(&files, None, "first", 1);
    set(&mut files, "Source/a.cpp", a2);
    files.insert("Tools/run.sh".into(), (FileMode::Executable, b));
    let second = build(&files, Some(first.id), "second", 2);

    let revs = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&first, &second]))
        .await
        .unwrap();
    assert_eq!(head(&h, "main").await, Some(second.id));

    let stored = h.svc.get_commit(&first.id).await.unwrap();
    assert_eq!(stored.raw, first.raw);
    assert_eq!(stored.revisions.len(), 2);
    assert_eq!(stored.author.name, "carol");
    assert_eq!(stored.author.offset_minutes, 120);
    let stored = h.svc.get_commit(&second.id).await.unwrap();
    assert_eq!(stored.parents, vec![first.id]);
    let paths: Vec<_> = stored.revisions.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["Source/a.cpp", "Tools/run.sh"]);

    assert_eq!(revs.len(), 4);
    let a_hist = h.svc.history(&path("Source/a.cpp")).await.unwrap();
    assert_eq!(a_hist.iter().map(|r| r.id.0).collect::<Vec<_>>(), [1, 2]);
    assert_eq!(a_hist[1].base, Some(RevisionId(1)));
    assert_eq!(a_hist[1].content, a2);
    assert_eq!(a_hist[1].author, user("alice"));
    let person = a_hist[1].person.as_ref().unwrap();
    assert_eq!(
        (person.name.as_str(), person.offset_minutes),
        ("carol", 120)
    );
    assert_eq!(a_hist[1].created_at.timestamp(), T0 + 2);
    assert_eq!(a_hist[1].message, "second");
    let run = h.svc.head(&path("Tools/run.sh")).await.unwrap().unwrap();
    assert_eq!(run.file_mode, FileMode::Executable);
    assert_eq!(run.mode, Some(Mode::Shared));

    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
    assert_eq!(report.commits, 2);
    let snap = h.svc.snapshot_on("main").await.unwrap();
    assert_eq!(snap.entries.len(), 3);
}

pub async fn a_push_must_fast_forward_the_branch(store: Store) {
    let h = harness(store);
    let (x, y) = (blob(&h, "x").await, blob(&h, "y").await);
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", x);
    let root = build(&files, None, "root", 1);
    h.svc
        .push_commits(&who("alice"), "main", objects(&[&root]))
        .await
        .unwrap();

    // Another root, as if built without the server's history.
    let mut other = Files::new();
    set(&mut other, "Source/z.cpp", y);
    let stray = build(&other, None, "stray", 2);
    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&stray]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotFastForward(_)), "{err}");
    assert_eq!(err.code(), "not_fast_forward");

    // A commit based on an older head once the branch has moved on.
    set(&mut files, "Source/b.cpp", y);
    let next = build(&files, Some(root.id), "next", 3);
    h.svc
        .push_commits(&who("alice"), "main", objects(&[&next]))
        .await
        .unwrap();
    set(&mut files, "Source/c.cpp", y);
    let late = build(&files, Some(root.id), "late", 4);
    let err = h
        .svc
        .push_commits(&who("bob"), "main", objects(&[&late]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotFastForward(_)), "{err}");

    // Pushing what is already there is no fast-forward either.
    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&next]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotFastForward(_)), "{err}");
    assert_eq!(head(&h, "main").await, Some(next.id));
    absent(&h, &[stray.id, late.id]).await;
    assert!(h.svc.head(&path("Source/z.cpp")).await.unwrap().is_none());
}

pub async fn an_exclusive_path_needs_the_pushers_live_lock_at_push_time(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    let c = blob(&h, "map").await;
    let mut files = Files::new();
    set(&mut files, "Content/Dungeon.umap", c);
    let commit = build(&files, None, "map", 1);

    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");

    h.svc.checkout(&map, &user("bob"), None).await.unwrap();
    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::LockHeld { ref owner, .. } if owner == &user("bob")),
        "{err}"
    );
    assert_eq!(h.svc.locks().await.unwrap().len(), 1, "bob keeps his lock");

    h.svc.release(&map, &user("bob")).await.unwrap();
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.clock.advance(Duration::hours(9));
    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::LockRequired(_)),
        "a lapsed lease is refused: {err}"
    );
    absent(&h, &[commit.id]).await;

    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let revs = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap();
    assert_eq!(revs[0].mode, Some(Mode::Exclusive));
    assert!(h.svc.locks().await.unwrap().is_empty());
}

pub async fn a_lock_is_consumed_after_the_paths_last_revision_in_the_push(store: Store) {
    let h = harness(store);
    let (map, other) = (path("Content/a.umap"), path("Content/b.umap"));
    let (one, two, three) = (
        blob(&h, "one").await,
        blob(&h, "two").await,
        blob(&h, "three").await,
    );
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.svc.checkout(&other, &user("alice"), None).await.unwrap();
    let mut files = Files::new();
    set(&mut files, "Content/a.umap", one);
    let first = build(&files, None, "one", 1);
    set(&mut files, "Content/a.umap", two);
    set(&mut files, "Content/b.umap", one);
    let second = build(&files, Some(first.id), "two", 2);
    set(&mut files, "Content/a.umap", three);
    let third = build(&files, Some(second.id), "three", 3);

    h.svc
        .push_commits(&who("alice"), "main", objects(&[&first, &second, &third]))
        .await
        .unwrap();
    let hist = h.svc.history(&map).await.unwrap();
    assert_eq!(hist.iter().map(|r| r.id.0).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(
        hist.iter().map(|r| r.base.map(|b| b.0)).collect::<Vec<_>>(),
        [None, Some(1), Some(2)]
    );
    assert_eq!(h.svc.history(&other).await.unwrap().len(), 1);
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "both locks used up"
    );

    // The next push needs a fresh lock.
    set(&mut files, "Content/a.umap", one);
    let fourth = build(&files, Some(third.id), "four", 4);
    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&fourth]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
}

pub async fn a_pushed_exclusive_revision_is_based_on_the_branchs_pointer(store: Store) {
    let h = harness(store);
    let map = path("Content/a.umap");
    let (v1, v2, v3) = (
        blob(&h, "v1").await,
        blob(&h, "v2").await,
        blob(&h, "v3").await,
    );
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.svc
        .checkin(&who("alice"), &map, v1, None, "one".into())
        .await
        .unwrap();
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();
    h.svc
        .checkout_on("feature", &map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    h.svc
        .checkin_on(
            "feature",
            &who("alice"),
            &map,
            v2,
            FileMode::Regular,
            Some(RevisionId(1)),
            "feature".into(),
        )
        .await
        .unwrap();

    // main's pointer is still r1, so a push there is based on r1, not on the feature's r2.
    let main_head = head(&h, "main").await.unwrap();
    let mut files = Files::new();
    set(&mut files, "Content/a.umap", v3);
    let commit = build(&files, Some(main_head), "main edit", 5);
    h.svc
        .checkout_on("main", &map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    let revs = h
        .svc
        .push_commits(&who("bob"), "main", objects(&[&commit]))
        .await
        .unwrap();
    assert_eq!(
        (revs[0].id, revs[0].base),
        (RevisionId(3), Some(RevisionId(1)))
    );
    assert_eq!(
        h.svc.revision_on("main", &map).await.unwrap().unwrap().id,
        RevisionId(3)
    );
    assert_eq!(
        h.svc
            .revision_on("feature", &map)
            .await
            .unwrap()
            .unwrap()
            .id,
        RevisionId(2)
    );
}

pub async fn a_rejected_push_leaves_nothing_behind(store: Store) {
    let h = harness(store);
    let (map, held) = (path("Content/a.umap"), path("Content/held.umap"));
    let (x, y) = (blob(&h, "x").await, blob(&h, "y").await);
    h.svc.checkout(&held, &user("alice"), None).await.unwrap();
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", x);
    let first = build(&files, None, "shared part", 1);
    set(&mut files, "Content/a.umap", y);
    let second = build(&files, Some(first.id), "needs a lock", 2);

    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&first, &second]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    assert_eq!(head(&h, "main").await, None);
    absent(&h, &[first.id, second.id]).await;
    assert!(h.svc.head(&path("Source/a.cpp")).await.unwrap().is_none());
    assert!(h.svc.head(&map).await.unwrap().is_none());
    assert_eq!(h.svc.locks().await.unwrap().len(), 1, "unrelated lock kept");
    assert_eq!(h.svc.commits(None, None).await.unwrap().commits.len(), 0);
    let report = h.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");

    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let revs = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&first, &second]))
        .await
        .unwrap();
    assert_eq!(revs.len(), 2, "the same push lands once it is valid");
    assert_eq!(h.svc.locks().await.unwrap().len(), 1);
}

async fn refused(h: &Harness, what: &str, parts: &[&Built]) {
    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(parts))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::PushUnsupported(_)), "{what}: {err}");
    assert_eq!(err.code(), "push_unsupported");
    assert_eq!(head(h, "main").await, Some(parts[0].parents_of()));
}
impl Built {
    fn parents_of(&self) -> ObjectId {
        pyn_object::Commit::parse(Algorithm::Sha256, &self.raw)
            .unwrap()
            .parents[0]
    }
}

pub async fn a_push_refuses_what_it_cannot_record(store: Store) {
    let h = harness(store);
    let (x, y) = (blob(&h, "x").await, blob(&h, "y").await);
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", x);
    set(&mut files, "Source/b.cpp", x);
    let base = build(&files, None, "base", 1);
    h.svc
        .push_commits(&who("alice"), "main", objects(&[&base]))
        .await
        .unwrap();

    let mut with_policy = files.clone();
    set(&mut with_policy, ".pyn/pyn.toml", y);
    refused(
        &h,
        "policy file",
        &[&build(&with_policy, Some(base.id), "policy", 2)],
    )
    .await;

    let mut deleted = files.clone();
    deleted.remove("Source/b.cpp");
    set(&mut deleted, "Source/c.cpp", y);
    refused(
        &h,
        "deletion",
        &[&build(&deleted, Some(base.id), "delete", 2)],
    )
    .await;

    let mut folder = files.clone();
    folder.remove("Source/b.cpp");
    set(&mut folder, "Source/b.cpp/inner", y);
    refused(
        &h,
        "a file turned folder",
        &[&build(&folder, Some(base.id), "folder", 2)],
    )
    .await;

    refused(
        &h,
        "empty commit",
        &[&build(&files, Some(base.id), "empty", 2)],
    )
    .await;

    let mut changed = files.clone();
    set(&mut changed, "Source/a.cpp", y);
    let tree = snapshot_tree(&changed);
    let other_second = commit_raw(tree.root, vec![base.id], b"two times\n", T0 + 2, T0 + 3);
    refused(
        &h,
        "author and committer times differ",
        &[&built_from(other_second, tree.trees.clone())],
    )
    .await;
    let not_utf8 = commit_raw(tree.root, vec![base.id], b"caf\xe9\n", T0 + 2, T0 + 2);
    refused(
        &h,
        "message that is not UTF-8",
        &[&built_from(not_utf8, tree.trees.clone())],
    )
    .await;
    let no_newline = commit_raw(tree.root, vec![base.id], b"no newline", T0 + 2, T0 + 2);
    refused(
        &h,
        "message without a final newline",
        &[&built_from(no_newline, tree.trees.clone())],
    )
    .await;
    let merge = commit_raw(
        tree.root,
        vec![base.id, base.id],
        b"merge\n",
        T0 + 2,
        T0 + 2,
    );
    refused(
        &h,
        "merge commit",
        &[&built_from(merge, tree.trees.clone())],
    )
    .await;

    let mut gitlink = files.clone();
    gitlink.insert("Source/sub".into(), (FileMode::Gitlink, x));
    refused(
        &h,
        "submodule",
        &[&build(&gitlink, Some(base.id), "submodule", 2)],
    )
    .await;

    assert_eq!(h.svc.commits(None, None).await.unwrap().commits.len(), 1);

    let ok = build(&changed, Some(base.id), "fine", 2);
    h.svc
        .push_commits(&who("alice"), "main", objects(&[&ok]))
        .await
        .unwrap();
}

pub async fn a_push_is_checked_against_its_hashes_trees_and_objects(store: Store) {
    let h = harness(store);
    let (x, y) = (blob(&h, "x").await, blob(&h, "y").await);
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", x);
    let root = build(&files, None, "root", 1);
    h.svc
        .push_commits(&who("alice"), "main", objects(&[&root]))
        .await
        .unwrap();
    set(&mut files, "Source/b.cpp", y);
    let next = build(&files, Some(root.id), "next", 2);

    let invalid = |err: PynError, what: &str| {
        assert!(matches!(err, PynError::PushInvalid(_)), "{what}: {err}");
        assert_eq!(err.code(), "push_invalid");
    };
    let push = |o: PushObjects| {
        let svc = h.svc.clone();
        async move { svc.push_commits(&who("alice"), "main", o).await }
    };

    let mut wrong_id = objects(&[&next]);
    wrong_id.commits[0].0 = root.id;
    invalid(push(wrong_id).await.unwrap_err(), "commit id");

    let mut wrong_tree = objects(&[&next]);
    wrong_tree.trees[0].1.push(0);
    invalid(push(wrong_tree).await.unwrap_err(), "tree hash");

    let mut no_trees = objects(&[&next]);
    no_trees.trees.clear();
    invalid(push(no_trees).await.unwrap_err(), "missing tree");

    let mut garbage = objects(&[&next]);
    garbage.commits[0].1 = b"not a commit".to_vec();
    garbage.commits[0].0 = hash_object(Algorithm::Sha256, Kind::Commit, b"not a commit").unwrap();
    invalid(push(garbage).await.unwrap_err(), "not a commit");

    let mut sha1_tree = objects(&[&next]);
    let sha1 = ObjectId::from_hex(Algorithm::Sha1, &"1".repeat(40)).unwrap();
    sha1_tree.commits[0].0 = sha1;
    invalid(push(sha1_tree).await.unwrap_err(), "wrong hash algorithm");

    // Commits out of order, or not a chain.
    set(&mut files, "Source/c.cpp", y);
    let third = build(&files, Some(next.id), "third", 3);
    let err = push(objects(&[&third, &next])).await.unwrap_err();
    assert!(
        matches!(err, PynError::CommitNotFound(_)),
        "a child before its parent: {err}"
    );
    let skipping = build(&files, Some(root.id), "skips next", 4);
    invalid(
        push(objects(&[&next, &skipping])).await.unwrap_err(),
        "not a chain",
    );

    // A blob the server never received.
    let mut missing = files.clone();
    let unknown = ObjectId::from_hex(crate::REPO_ALGORITHM, &"7".repeat(64)).unwrap();
    set(&mut missing, "Source/d.cpp", unknown);
    let err = push(objects(&[&build(&missing, Some(root.id), "no blob", 5)]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");

    // A parent the server does not have.
    let ghost = ObjectId::from_hex(crate::REPO_ALGORITHM, &"9".repeat(64)).unwrap();
    let err = push(objects(&[&build(&files, Some(ghost), "orphan", 6)]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::CommitNotFound(_)), "{err}");

    let err = push(PushObjects::default()).await.unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");

    assert_eq!(head(&h, "main").await, Some(root.id));
    assert_eq!(h.svc.commits(None, None).await.unwrap().commits.len(), 1);
    assert!(h.svc.head(&path("Source/b.cpp")).await.unwrap().is_none());
    push(objects(&[&next, &third])).await.unwrap();
}

pub async fn an_unlisted_path_is_exclusive_in_a_push(store: Store) {
    let h = harness_with(store, "");
    let c = blob(&h, "x").await;
    let mut files = Files::new();
    set(&mut files, "Notes/todo.txt", c);
    let commit = build(&files, None, "todo", 1);
    let err = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");

    h.svc
        .checkout(&path("Notes/todo.txt"), &user("alice"), None)
        .await
        .unwrap();
    let revs = h
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap();
    assert_eq!(revs[0].mode, Some(Mode::Exclusive));
}

pub async fn a_push_needs_the_checkin_permission_and_an_existing_branch(store: Store) {
    let h = harness(store);
    let c = blob(&h, "x").await;
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", c);
    let commit = build(&files, None, "a", 1);
    let reader = Principal {
        user: user("rita"),
        permissions: BTreeSet::from([Permission::Read]),
        email: None,
    };
    let err = h
        .svc
        .push_commits(&reader, "main", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::Checkin)),
        "{err}"
    );

    let err = h
        .svc
        .push_commits(&who("alice"), "feature", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::BranchNotFound(_)), "{err}");
    absent(&h, &[commit.id]).await;
}

pub async fn a_push_past_the_storage_cap_applies_nothing(store: Store) {
    let h = harness(store.clone());
    let svc = Arc::new(capped_service(&store, &h, "game", Some(10)));
    store
        .create_repo(repo_record("game", "alice", "game"))
        .await
        .unwrap();
    let (six, other) = (blob(&h, "123456").await, blob(&h, "abcdef").await);
    let mut files = Files::new();
    set(&mut files, "Source/a", six);
    let first = build(&files, None, "fits", 1);
    set(&mut files, "Source/b", other);
    let second = build(&files, Some(first.id), "past the cap", 2);

    let err = svc
        .push_commits(&who("alice"), "main", objects(&[&first, &second]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::StorageLimitReached { .. }), "{err}");
    assert!(svc.head(&path("Source/a")).await.unwrap().is_none());
    assert_eq!(svc.head_of("main").await.unwrap(), None);
    svc.push_commits(&who("alice"), "main", objects(&[&first]))
        .await
        .unwrap();
}

pub async fn racing_pushes_to_one_branch_have_exactly_one_winner(store: Store) {
    let h = harness(store);
    let mut pushes = Vec::new();
    for i in 0..8 {
        let mut files = Files::new();
        set(
            &mut files,
            &format!("Source/f{i}.cpp"),
            blob(&h, &format!("v{i}")).await,
        );
        pushes.push(objects(&[&build(&files, None, &format!("c{i}"), i)]));
    }
    let tasks: Vec<_> = pushes
        .into_iter()
        .map(|o| {
            let svc = h.svc.clone();
            tokio::spawn(async move { svc.push_commits(&who("alice"), "main", o).await })
        })
        .collect();
    let mut wins = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => wins += 1,
            Err(e) => assert!(matches!(e, PynError::NotFastForward(_)), "{e}"),
        }
    }
    assert_eq!(wins, 1);
    assert_eq!(h.svc.commits(None, None).await.unwrap().commits.len(), 1);
}

//! Deleting refs, checked updates and reflogs, on a scratch repository.

use std::fs;

use pyn_git::{Error, Expect, RefStore, ReflogEntry};
use pyn_object::{Algorithm, ObjectId, Signature};

fn id(n: u8) -> ObjectId {
    ObjectId::from_bytes(Algorithm::Sha1, &[n; 20]).unwrap()
}

fn store() -> (tempfile::TempDir, RefStore) {
    let tmp = tempfile::tempdir().unwrap();
    let git = tmp.path().join(".git");
    fs::create_dir_all(git.join("refs/heads")).unwrap();
    fs::create_dir_all(git.join("refs/tags")).unwrap();
    fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let refs = RefStore::new(git.clone(), git, Algorithm::Sha1);
    (tmp, refs)
}

fn who() -> Signature {
    Signature {
        name: b"Log Writer".to_vec(),
        email: b"log@example.com".to_vec(),
        time: 1_700_000_000,
        offset_minutes: -330,
    }
}

#[test]
fn an_update_that_expects_another_value_changes_nothing() {
    let (tmp, refs) = store();
    refs.update_checked("refs/heads/a", Expect::Absent, &id(1))
        .unwrap();
    let err = refs
        .update_checked("refs/heads/a", Expect::Absent, &id(2))
        .unwrap_err();
    assert!(matches!(err, Error::RefChanged(_)));
    let err = refs
        .update_checked("refs/heads/a", Expect::Id(id(9)), &id(2))
        .unwrap_err();
    assert!(matches!(err, Error::RefChanged(_)));
    refs.update_checked("refs/heads/a", Expect::Id(id(1)), &id(2))
        .unwrap();
    assert_eq!(refs.resolve("refs/heads/a").unwrap(), Some(id(2)));
    assert!(!tmp.path().join(".git/refs/heads/a.lock").exists());
}

#[test]
fn delete_removes_loose_and_packed_refs_and_empty_folders() {
    let (tmp, refs) = store();
    let git = tmp.path().join(".git");
    refs.update("refs/heads/topic/deep", &id(1)).unwrap();
    refs.delete("refs/heads/topic/deep", Expect::Id(id(1)))
        .unwrap();
    assert!(!git.join("refs/heads/topic").exists());
    assert!(git.join("refs/heads").is_dir());

    let (a, b) = (id(3).to_hex(), id(4).to_hex());
    fs::write(
        git.join("packed-refs"),
        format!("# pack-refs with: peeled fully-peeled sorted \n{a} refs/heads/old\n{b} refs/tags/v1\n^{a}\n"),
    )
    .unwrap();
    refs.delete("refs/heads/old", Expect::Any).unwrap();
    assert_eq!(refs.resolve("refs/heads/old").unwrap(), None);
    assert_eq!(refs.resolve("refs/tags/v1").unwrap(), Some(id(4)));
    assert_eq!(refs.packed().unwrap()[0].peeled, Some(id(3)));
    refs.delete("refs/tags/v1", Expect::Any).unwrap();
    assert!(refs.packed().unwrap().is_empty());
    assert!(refs.delete("refs/heads/x", Expect::Id(id(1))).is_err());
}

#[test]
fn reflogs_round_trip_and_are_only_created_on_request() {
    let (tmp, refs) = store();
    let entry = |old, new, msg: &str| ReflogEntry {
        old: id(old),
        new: id(new),
        who: who(),
        message: msg.into(),
    };
    assert!(
        !refs
            .append_reflog(
                "refs/heads/a",
                &entry(0, 1, "branch: Created from HEAD"),
                false
            )
            .unwrap()
    );
    assert!(!tmp.path().join(".git/logs").exists());
    refs.append_reflog(
        "refs/heads/a",
        &entry(0, 1, "branch: Created from HEAD"),
        true,
    )
    .unwrap();
    refs.append_reflog("refs/heads/a", &entry(1, 2, "commit: second\nline"), false)
        .unwrap();
    let text = fs::read_to_string(tmp.path().join(".git/logs/refs/heads/a")).unwrap();
    assert_eq!(
        text,
        format!(
            "{} {} Log Writer <log@example.com> 1700000000 -0530\tbranch: Created from HEAD\n{} {} Log Writer <log@example.com> 1700000000 -0530\tcommit: second line\n",
            id(0),
            id(1),
            id(1),
            id(2)
        )
    );
    let read = refs.read_reflog("refs/heads/a").unwrap();
    assert_eq!(read.len(), 2);
    assert_eq!(read[1].new, id(2));
    assert_eq!(read[0].who, who());
    refs.rename_reflog("refs/heads/a", "refs/heads/b").unwrap();
    assert!(refs.has_reflog("refs/heads/b") && !refs.has_reflog("refs/heads/a"));
    refs.write_reflog("refs/heads/b", &[]).unwrap();
    assert!(!refs.has_reflog("refs/heads/b"));
}

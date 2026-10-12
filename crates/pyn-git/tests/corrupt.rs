mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{copy_git_dir, expected, id};
use pyn_git::{Error, Index, ObjectDb, Pack, Repository};
use pyn_object::{Algorithm, Kind, hash_object};

fn pack_files(git_dir: &Path) -> (PathBuf, PathBuf) {
    let dir = git_dir.join("objects/pack");
    let mut idx = None;
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "idx") {
            idx = Some(p);
        }
    }
    let idx = idx.unwrap();
    (idx.clone(), idx.with_extension("pack"))
}

fn flip(path: &Path, at: impl Fn(usize) -> usize) {
    let mut bytes = fs::read(path).unwrap();
    let i = at(bytes.len());
    bytes[i] ^= 0xff;
    fs::write(path, bytes).unwrap();
}

#[test]
fn truncated_pack_is_rejected() {
    for (name, alg) in [
        ("sha1-packed", Algorithm::Sha1),
        ("sha256-packed", Algorithm::Sha256),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = copy_git_dir(name, tmp.path(), "r/.git");
        let (idx, pack) = pack_files(&git_dir);
        let len = fs::metadata(&pack).unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(&pack)
            .unwrap()
            .set_len(len - 10)
            .unwrap();
        assert!(
            matches!(Pack::open(&idx, alg), Err(Error::Corrupt { .. })),
            "{name}"
        );
        assert!(
            Repository::open(&tmp.path().join("r"))
                .unwrap()
                .objects()
                .is_err()
        );
    }
}

#[test]
fn pack_shorter_than_its_header_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-packed", tmp.path(), "r/.git");
    let (idx, pack) = pack_files(&git_dir);
    fs::write(&pack, b"PACK").unwrap();
    assert!(Pack::open(&idx, Algorithm::Sha1).is_err());
}

#[test]
fn flipped_pack_byte_fails_verify_and_reads_stay_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-packed", tmp.path(), "r/.git");
    let (idx, pack) = pack_files(&git_dir);
    flip(&pack, |len| len / 2);
    let pack = Pack::open(&idx, Algorithm::Sha1).unwrap();
    assert!(matches!(pack.verify(), Err(Error::Checksum { .. })));
    for oid in pack.ids().collect::<Vec<_>>() {
        // Every read either succeeds with a verified hash or fails with an error; none may panic.
        let _ = pack.read(&oid);
    }
}

#[test]
fn bad_pack_trailer_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-packed", tmp.path(), "r/.git");
    let (idx, pack) = pack_files(&git_dir);
    flip(&pack, |len| len - 1);
    assert!(matches!(
        Pack::open(&idx, Algorithm::Sha1),
        Err(Error::Corrupt { .. })
    ));
}

#[test]
fn bad_index_checksum_fails_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha256-packed", tmp.path(), "r/.git");
    let (idx, _) = pack_files(&git_dir);
    flip(&idx, |len| len - 1);
    let pack = Pack::open(&idx, Algorithm::Sha256).unwrap();
    assert!(matches!(
        pack.verify(),
        Err(Error::Checksum { what: "pack index" })
    ));
}

#[test]
fn corrupt_pack_index_tables_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-packed", tmp.path(), "r/.git");
    let (idx, _) = pack_files(&git_dir);
    let good = fs::read(&idx).unwrap();
    fs::write(&idx, &good[..good.len() / 2]).unwrap();
    assert!(Pack::open(&idx, Algorithm::Sha1).is_err());
    let mut bad = good.clone();
    bad[7] = 1;
    fs::write(&idx, &bad).unwrap();
    assert!(matches!(
        Pack::open(&idx, Algorithm::Sha1),
        Err(Error::Unsupported(_))
    ));
    let mut bad = good;
    bad[8 + 4 * 10] = 0xff;
    fs::write(&idx, &bad).unwrap();
    assert!(Pack::open(&idx, Algorithm::Sha1).is_err());
}

#[test]
fn wrong_algorithm_for_a_pack_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-packed", tmp.path(), "r/.git");
    let (idx, _) = pack_files(&git_dir);
    assert!(Pack::open(&idx, Algorithm::Sha256).is_err());
}

fn loose_path(db_dir: &Path, hex: &str) -> PathBuf {
    db_dir.join(&hex[..2]).join(&hex[2..])
}

#[test]
fn corrupt_loose_objects_fail_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-loose", tmp.path(), "r/.git");
    let objects = git_dir.join("objects");
    let db = ObjectDb::open(&objects, Algorithm::Sha1).unwrap();
    let rows = expected("sha1-loose", "objects.txt");
    let ids: Vec<_> = rows.iter().map(|r| id(Algorithm::Sha1, &r[0])).collect();

    let writable = common::make_writable;

    // truncated zlib stream
    let p = loose_path(&objects, &ids[0].to_hex());
    let bytes = fs::read(&p).unwrap();
    writable(&p);
    fs::write(&p, &bytes[..bytes.len() - 6]).unwrap();
    assert!(matches!(db.read(&ids[0]), Err(Error::Corrupt { .. })));

    // not zlib at all
    let p = loose_path(&objects, &ids[1].to_hex());
    writable(&p);
    fs::write(&p, b"this is not an object").unwrap();
    assert!(matches!(db.read(&ids[1]), Err(Error::Corrupt { .. })));

    // valid object stored under the wrong id
    let (a, b) = (
        loose_path(&objects, &ids[2].to_hex()),
        loose_path(&objects, &ids[3].to_hex()),
    );
    writable(&b);
    fs::copy(&a, &b).unwrap();
    assert!(matches!(db.read(&ids[3]), Err(Error::Corrupt { .. })));
}

#[test]
fn missing_object_and_foreign_algorithm() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-loose", tmp.path(), "r/.git");
    let db = ObjectDb::open(&git_dir.join("objects"), Algorithm::Sha1).unwrap();
    let absent = hash_object(Algorithm::Sha1, Kind::Blob, b"not stored").unwrap();
    assert!(matches!(db.read(&absent), Err(Error::ObjectNotFound(_))));
    assert_eq!(db.read_opt(&absent).unwrap(), None);
    let foreign = hash_object(Algorithm::Sha256, Kind::Blob, b"x").unwrap();
    assert!(db.read(&foreign).is_err());
    assert!(!db.contains(&foreign).unwrap());
}

#[test]
fn corrupt_index_files_fail_cleanly() {
    for (name, alg) in [
        ("sha1-loose", Algorithm::Sha1),
        ("sha1-packed", Algorithm::Sha1),
        ("sha256-loose", Algorithm::Sha256),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = copy_git_dir(name, tmp.path(), "r/.git");
        let path = git_dir.join("index");
        let good = fs::read(&path).unwrap();
        assert!(Index::parse(&good, alg).is_ok());

        for cut in [0, 5, 11, 40, good.len() / 2, good.len() - 1] {
            assert!(Index::parse(&good[..cut], alg).is_err(), "{name} cut {cut}");
        }
        let mut bad = good.clone();
        let mid = bad.len() / 2;
        bad[mid] ^= 1;
        assert!(matches!(
            Index::parse(&bad, alg),
            Err(Error::Checksum { .. })
        ));
        let mut bad = good.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(matches!(
            Index::parse(&bad, alg),
            Err(Error::Checksum { .. })
        ));

        // Wrong algorithm reads garbage of the wrong width and must still fail rather than panic.
        let other = if alg == Algorithm::Sha1 {
            Algorithm::Sha256
        } else {
            Algorithm::Sha1
        };
        assert!(Index::parse(&good, other).is_err());
    }
}

#[test]
fn index_with_valid_checksum_but_bad_structure_is_rejected() {
    use pyn_object::ObjectId;
    // A hand-built v2 index whose single entry has no path terminator, with a correct checksum.
    let alg = Algorithm::Sha1;
    let mut body = Vec::new();
    body.extend_from_slice(b"DIRC");
    body.extend_from_slice(&2u32.to_be_bytes());
    body.extend_from_slice(&1u32.to_be_bytes());
    body.extend_from_slice(&[0; 40]);
    body.extend_from_slice(ObjectId::from_bytes(alg, &[1; 20]).unwrap().as_bytes());
    body.extend_from_slice(&3u16.to_be_bytes());
    body.extend_from_slice(b"abc");
    let sum = {
        use sha1_checked::{Digest as _, Sha1};
        Sha1::digest(&body).to_vec()
    };
    body.extend_from_slice(&sum);
    assert!(Index::parse(&body, alg).is_err());
}

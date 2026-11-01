//! Short-id enumeration over pack indexes, loose objects, multiple packs and alternates.

mod common;

use std::fs;

use common::{copy_dir, fixture_dir};
use pyn_git::ObjectDb;
use pyn_object::{Algorithm, ObjectId};

fn expected(ids: &[ObjectId], prefix: &str) -> Vec<ObjectId> {
    let mut found: Vec<_> = ids
        .iter()
        .filter(|id| id.to_hex().starts_with(prefix))
        .copied()
        .collect();
    found.sort_by_key(ObjectId::to_hex);
    found.dedup();
    found
}

fn prefixes(ids: &[ObjectId]) -> Vec<String> {
    let mut all: Vec<String> = ["", "0", "f", "00", "ff", "abc", "0000000", "fffffff", "g1"]
        .map(String::from)
        .to_vec();
    for id in ids {
        let hex = id.to_hex();
        all.extend((1..=hex.len()).map(|n| hex[..n].to_string()));
    }
    all
}

#[test]
fn pack_and_database_prefixes_match_a_hex_filter() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        for boundary in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = if boundary {
                common::write_boundary_pack(tmp.path(), algorithm);
                tmp.path().to_path_buf()
            } else {
                fixture_dir(&format!("{}-packed", algorithm.name())).join("dot-git/objects")
            };
            let db = ObjectDb::open(&dir, algorithm).unwrap();
            let pack = db.packs().remove(0);
            let ids: Vec<_> = pack.ids().collect();
            for prefix in prefixes(&ids) {
                assert_eq!(
                    pack.ids_with_prefix(&prefix),
                    expected(&ids, &prefix),
                    "{prefix}"
                );
                assert_eq!(
                    db.ids_with_prefix(&prefix).unwrap(),
                    expected(&ids, &prefix),
                    "{prefix}"
                );
            }
            let too_long = format!("{}0", ids[0].to_hex());
            assert!(pack.ids_with_prefix(&too_long).is_empty());
        }
    }
}

#[test]
fn duplicates_across_packs_loose_objects_and_alternates_are_one_id() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("objects");
        let other = tmp.path().join("alternate");
        let source = fixture_dir(&format!("{}-packed", algorithm.name())).join("dot-git/objects");
        copy_dir(&source, &local);
        copy_dir(&source, &other);
        fs::create_dir_all(local.join("info")).unwrap();
        fs::write(
            local.join("info/alternates"),
            format!("{}\n", other.display()),
        )
        .unwrap();
        let pack_dir = local.join("pack");
        for entry in fs::read_dir(&pack_dir)
            .unwrap()
            .flatten()
            .collect::<Vec<_>>()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(ext) = name.strip_prefix("pack-").and_then(|n| n.split_once('.')) {
                fs::copy(
                    entry.path(),
                    pack_dir.join(format!("pack-second.{}", ext.1)),
                )
                .unwrap();
            }
        }
        let db = ObjectDb::open(&local, algorithm).unwrap();
        let ids: Vec<_> = db.packs()[0].ids().collect();
        assert_eq!(db.packs().len(), 2);
        let hex = ids[0].to_hex();
        fs::create_dir_all(local.join(&hex[..2])).unwrap();
        fs::write(local.join(&hex[..2]).join(&hex[2..]), b"").unwrap();
        for prefix in prefixes(&ids) {
            assert_eq!(
                db.ids_with_prefix(&prefix).unwrap(),
                expected(&ids, &prefix),
                "{prefix}"
            );
        }
        assert_eq!(db.ids_with_prefix("").unwrap().len(), ids.len());
    }
}

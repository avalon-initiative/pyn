//! Prefix uniqueness over pack indexes, loose fan-out directories and alternates.

mod common;

use std::fs;
use std::path::Path;

use common::{copy_dir, fixture_dir};
use pyn_git::ObjectDb;
use pyn_object::{Algorithm, ObjectId};

fn reference(id: &ObjectId, minimum: usize, ids: &[ObjectId]) -> usize {
    let hex = id.to_hex();
    ids.iter()
        .filter(|other| *other != id)
        .fold(minimum, |len, other| {
            len.max(
                hex.bytes()
                    .zip(other.to_hex().bytes())
                    .take_while(|(a, b)| a == b)
                    .count()
                    + 1,
            )
        })
        .min(hex.len())
}

#[test]
fn packed_hits_misses_neighbors_and_exhausted_prefixes() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let db = ObjectDb::open(
            &fixture_dir(&format!("{}-packed", algorithm.name())).join("dot-git/objects"),
            algorithm,
        )
        .unwrap();
        let packs = db.packs();
        assert_eq!(packs.len(), 1);
        let ids: Vec<_> = packs[0].ids().collect();
        assert!(!ids.is_empty());
        let mut probes = ids.clone();
        for id in &ids {
            for offset in [2, algorithm.digest_len() - 1] {
                for mask in [0x01, 0x10] {
                    let mut bytes = id.as_bytes().to_vec();
                    bytes[offset] ^= mask;
                    probes.push(ObjectId::from_bytes(algorithm, &bytes).unwrap());
                }
            }
        }
        probes.push(ObjectId::from_bytes(algorithm, &vec![0; algorithm.digest_len()]).unwrap());
        probes.push(ObjectId::from_bytes(algorithm, &vec![255; algorithm.digest_len()]).unwrap());
        for id in probes {
            for minimum in [2, 4, 7, algorithm.digest_len() * 2] {
                let expected = reference(&id, minimum, &ids);
                assert_eq!(
                    packs[0].abbreviation_len(&id, minimum),
                    expected,
                    "pack {id} minimum {minimum}"
                );
                assert_eq!(
                    db.abbreviation_len(&id, minimum),
                    expected,
                    "database {id} minimum {minimum}"
                );
            }
        }
    }
}

fn loose(dir: &Path, id: &ObjectId) {
    // Uniqueness reads directory names, without opening or decoding object bodies.
    let hex = id.to_hex();
    fs::create_dir_all(dir.join(&hex[..2])).unwrap();
    fs::write(dir.join(&hex[..2]).join(&hex[2..]), b"").unwrap();
}

#[test]
fn loose_and_alternate_hits_misses_duplicates_and_exhausted_prefixes() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        for alternate in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let local = tmp.path().join("objects");
            let other = tmp.path().join("alternate");
            fs::create_dir_all(local.join("info")).unwrap();
            fs::create_dir_all(&other).unwrap();
            if alternate {
                fs::write(
                    local.join("info/alternates"),
                    format!("{}\n", other.display()),
                )
                .unwrap();
            }
            let store = if alternate { &other } else { &local };
            let width = algorithm.digest_len() * 2;
            let id = ObjectId::from_hex(algorithm, &format!("abcd1234{}", "0".repeat(width - 8)))
                .unwrap();
            let collision =
                ObjectId::from_hex(algorithm, &format!("abcd1235{}", "0".repeat(width - 8)))
                    .unwrap();
            loose(store, &id);
            loose(&local, &id);
            let db = ObjectDb::open(&local, algorithm).unwrap();
            assert_eq!(db.abbreviation_len(&id, 4), 4, "duplicates must not extend");
            assert_eq!(
                db.abbreviation_len(&collision, 4),
                8,
                "a miss still compares its neighbor"
            );
            loose(store, &collision);
            assert_eq!(
                db.abbreviation_len(&id, 4),
                8,
                "{alternate}: loose collision must extend"
            );
            assert_eq!(
                db.abbreviation_len(&id, 9),
                9,
                "a longer minimum must survive"
            );
            let null = ObjectId::from_bytes(algorithm, &vec![0; algorithm.digest_len()]).unwrap();
            assert_eq!(db.abbreviation_len(&null, 4), 4, "absent fan-out directory");
            let last =
                ObjectId::from_hex(algorithm, &format!("{}1", "0".repeat(width - 1))).unwrap();
            loose(store, &last);
            assert_eq!(
                db.abbreviation_len(&null, 4),
                width,
                "ambiguity can exhaust the hash width"
            );
            assert_eq!(db.abbreviation_len(&id, width), width, "full width");
        }
    }
}

#[test]
fn packed_alternates_extend_without_counting_duplicate_ids() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let tmp = tempfile::tempdir().unwrap();
        let local = tmp.path().join("objects");
        let other = tmp.path().join("alternate");
        fs::create_dir_all(local.join("info")).unwrap();
        copy_dir(
            &fixture_dir(&format!("{}-packed", algorithm.name())).join("dot-git/objects"),
            &other,
        );
        fs::write(
            local.join("info/alternates"),
            format!("{}\n", other.display()),
        )
        .unwrap();
        let alt = ObjectDb::open(&other, algorithm).unwrap();
        let ids: Vec<_> = alt.packs()[0].ids().collect();
        let id = ids[0];
        loose(&local, &id);
        let db = ObjectDb::open(&local, algorithm).unwrap();
        assert_eq!(db.abbreviation_len(&id, 4), reference(&id, 4, &ids));
        let mut bytes = id.as_bytes().to_vec();
        bytes[algorithm.digest_len() - 1] ^= 1;
        let absent = ObjectId::from_bytes(algorithm, &bytes).unwrap();
        assert_eq!(
            db.abbreviation_len(&absent, 4),
            algorithm.digest_len() * 2,
            "packed alternate must extend an absent id"
        );
    }
}

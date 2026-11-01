//! Count binary id accesses inside the real pack search, including internal enumeration.

mod common;

use common::fixture_dir;
use pyn_git::{ObjectDb, counters};
use pyn_object::{Algorithm, ObjectId};

fn take_id_accesses() -> usize {
    counters::take().id_accesses
}

#[test]
fn pack_abbreviation_bounds_binary_id_accesses() {
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
            let packs = db.packs();
            let pack = &packs[0];
            let ids: Vec<_> = pack.ids().collect();
            assert!(!ids.is_empty());
            assert_eq!(
                take_id_accesses(),
                ids.len(),
                "the id-access counter must observe actual pack enumeration"
            );
            let mut probes = ids.clone();
            for id in &ids {
                let mut bytes = id.as_bytes().to_vec();
                bytes[algorithm.digest_len() - 1] ^= 1;
                probes.push(ObjectId::from_bytes(algorithm, &bytes).unwrap());
            }
            probes
                .push(ObjectId::from_bytes(algorithm, &[0; 32][..algorithm.digest_len()]).unwrap());
            probes.push(
                ObjectId::from_bytes(algorithm, &[255; 32][..algorithm.digest_len()]).unwrap(),
            );
            for id in probes {
                let bucket = ids
                    .iter()
                    .filter(|other| other.as_bytes()[0] == id.as_bytes()[0])
                    .count();
                // Binary lower bound plus predecessor, exact-hit check and successor.
                let bound = (usize::BITS - bucket.leading_zeros()) as usize + 3;
                assert!(
                    bound < ids.len(),
                    "the fixture must distinguish binary search from a full scan"
                );
                for minimum in [4, 7, algorithm.digest_len() * 2] {
                    let hex = id.to_hex();
                    let expected = ids
                        .iter()
                        .filter(|other| *other != &id)
                        .fold(minimum, |len, other| {
                            len.max(
                                hex.bytes()
                                    .zip(other.to_hex().bytes())
                                    .take_while(|(a, b)| a == b)
                                    .count()
                                    + 1,
                            )
                        })
                        .min(hex.len());
                    assert_eq!(
                        pack.abbreviation_len(&id, minimum),
                        expected,
                        "positive rendering control for {id}"
                    );
                    let accesses = take_id_accesses();
                    assert!(
                        accesses <= bound,
                        "pack abbreviation must bound binary id accesses: {id}, minimum {minimum}, got {accesses}, allowed {bound}"
                    );
                    if bucket > 0 && minimum < hex.len() {
                        assert!(
                            accesses > 0,
                            "a nonempty bucket must exercise the counted id-access boundary"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn pack_prefix_lookup_reads_only_the_matching_range() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let db = ObjectDb::open(
            &fixture_dir(&format!("{}-packed", algorithm.name())).join("dot-git/objects"),
            algorithm,
        )
        .unwrap();
        let packs = db.packs();
        let pack = &packs[0];
        let ids: Vec<_> = pack.ids().collect();
        take_id_accesses();
        for id in &ids {
            let hex = id.to_hex();
            let found = pack.ids_with_prefix(&hex[..8]);
            let accesses = take_id_accesses();
            assert!(found.contains(id));
            let bound = 2 * ((usize::BITS - ids.len().leading_zeros()) as usize + 1) + found.len();
            assert!(
                accesses <= bound,
                "{id}: {accesses} accesses, allowed {bound}"
            );
            assert!(accesses < ids.len(), "a full scan reads every id");
        }
    }
}

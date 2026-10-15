use std::collections::HashMap;

use pyn_object::{
    Algorithm, Commit, Error, FileMode, Kind, ObjectHasher, ObjectId, Signature, Tag, TreeEntry,
    encode_tree, hash_object, parse_tree,
};

struct Row {
    kind: Kind,
    id: ObjectId,
    body: Vec<u8>,
}

fn fixtures(algo: Algorithm) -> HashMap<String, Row> {
    let text = include_str!("fixtures/objects.tsv");
    let mut map = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if Algorithm::from_name(f[1]) != Some(algo) {
            continue;
        }
        let kind = Kind::from_name(f[2].as_bytes()).unwrap();
        let row = Row {
            kind,
            id: ObjectId::from_hex(algo, f[3]).unwrap(),
            body: hex::decode(f[4]).unwrap(),
        };
        map.insert(f[0].to_string(), row);
    }
    map
}

const ALGOS: [Algorithm; 2] = [Algorithm::Sha1, Algorithm::Sha256];

fn sig(name: &[u8], email: &str, time: i64, offset_minutes: i32) -> Signature {
    Signature {
        name: name.to_vec(),
        email: email.as_bytes().to_vec(),
        time,
        offset_minutes,
    }
}

fn alice(time: i64, off: i32) -> Signature {
    sig(b"Alice", "alice@example.com", time, off)
}

fn bob(time: i64, off: i32) -> Signature {
    sig(b"Bob", "bob@example.com", time, off)
}

fn entry(mode: FileMode, name: &[u8], id: ObjectId) -> TreeEntry {
    TreeEntry {
        mode,
        name: name.to_vec(),
        id,
    }
}

#[test]
fn every_fixture_hashes_and_round_trips() {
    for algo in ALGOS {
        let rows = fixtures(algo);
        assert_eq!(rows.len(), 17);
        for (name, row) in &rows {
            assert_eq!(
                hash_object(algo, row.kind, &row.body).unwrap(),
                row.id,
                "{name}"
            );
            let again = match row.kind {
                Kind::Blob => row.body.clone(),
                Kind::Tree => encode_tree(algo, &parse_tree(algo, &row.body).unwrap()).unwrap(),
                Kind::Commit => Commit::parse(algo, &row.body)
                    .unwrap()
                    .encode(algo)
                    .unwrap(),
                Kind::Tag => Tag::parse(algo, &row.body).unwrap().encode(algo).unwrap(),
            };
            assert_eq!(again, row.body, "{name} round trip");
        }
    }
}

#[test]
fn blobs_match_git() {
    let binary: Vec<u8> = (0..=255).collect();
    let cases: [(&str, &[u8]); 5] = [
        ("blob_empty", b""),
        ("blob_hello", b"hello\n"),
        ("blob_script", b"#!/bin/sh\n"),
        ("blob_link", b"target"),
        ("blob_binary", &binary),
    ];
    for algo in ALGOS {
        let rows = fixtures(algo);
        for (name, content) in cases {
            assert_eq!(
                hash_object(algo, Kind::Blob, content).unwrap(),
                rows[name].id,
                "{name}"
            );
        }
    }
    let empty = hash_object(Algorithm::Sha1, Kind::Blob, b"").unwrap();
    assert_eq!(empty.to_hex(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
}

#[test]
fn streaming_matches_one_shot_for_any_chunking() {
    let data: Vec<u8> = (0..5000u32).map(|i| (i * 7 % 251) as u8).collect();
    for algo in ALGOS {
        let whole = hash_object(algo, Kind::Blob, &data).unwrap();
        for chunk in [1, 3, 64, 4999, 5000] {
            let mut h = ObjectHasher::blob(algo, data.len() as u64);
            for c in data.chunks(chunk) {
                h.update(c).unwrap();
            }
            assert_eq!(h.finish().unwrap(), whole, "chunk {chunk}");
        }
    }
}

#[test]
fn streaming_enforces_the_declared_length() {
    let mut h = ObjectHasher::blob(Algorithm::Sha256, 4);
    h.update(b"abc").unwrap();
    assert_eq!(
        h.update(b"de"),
        Err(Error::LengthMismatch {
            declared: 4,
            actual: 5
        })
    );
    let mut h = ObjectHasher::blob(Algorithm::Sha1, 4);
    h.update(b"abc").unwrap();
    assert_eq!(
        h.finish(),
        Err(Error::LengthMismatch {
            declared: 4,
            actual: 3
        })
    );
}

#[test]
fn trees_match_git() {
    for algo in ALGOS {
        let rows = fixtures(algo);
        let id = |n: &str| rows[n].id;
        // Deliberately unsorted: encoding must apply git's order.
        let mixed = [
            entry(FileMode::Regular, b"zzz", id("blob_empty")),
            entry(FileMode::Gitlink, b"mod", id("commit_root")),
            entry(FileMode::Symlink, b"foo0", id("blob_link")),
            entry(FileMode::Executable, b"foo.c", id("blob_script")),
            entry(FileMode::Regular, b"foo-bar", id("blob_hello")),
            entry(FileMode::Tree, b"foo", id("tree_empty")),
        ];
        assert_eq!(encode_tree(algo, &mixed).unwrap(), rows["tree_mixed"].body);
        let parsed = parse_tree(algo, &rows["tree_mixed"].body).unwrap();
        let names: Vec<&[u8]> = parsed.iter().map(|e| e.name.as_slice()).collect();
        // The directory `foo` sorts as `foo/`: after `foo.c`, before `foo0`.
        assert_eq!(
            names,
            [&b"foo-bar"[..], b"foo.c", b"foo", b"foo0", b"mod", b"zzz"]
        );

        let nonutf = [
            entry(FileMode::Regular, "café".as_bytes(), id("blob_hello")),
            entry(FileMode::Regular, b"\xff\xfe", id("blob_hello")),
            entry(FileMode::Regular, b"a", id("blob_binary")),
        ];
        assert_eq!(
            encode_tree(algo, &nonutf).unwrap(),
            rows["tree_nonutf8"].body
        );
        assert_eq!(encode_tree(algo, &[]).unwrap(), rows["tree_empty"].body);
        assert!(rows["tree_empty"].body.is_empty());
    }
    let empty = hash_object(Algorithm::Sha1, Kind::Tree, b"").unwrap();
    assert_eq!(empty.to_hex(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
}

fn commit(
    rows: &HashMap<String, Row>,
    parents: &[&str],
    tree: &str,
    a: Signature,
    c: Signature,
    msg: &[u8],
) -> Commit {
    Commit {
        tree: rows[tree].id,
        parents: parents.iter().map(|p| rows[*p].id).collect(),
        author: a,
        committer: c,
        extra_headers: vec![],
        message: msg.to_vec(),
    }
}

#[test]
fn commits_match_git() {
    for algo in ALGOS {
        let rows = fixtures(algo);
        let cases = [
            (
                "commit_root",
                commit(
                    &rows,
                    &[],
                    "tree_empty",
                    alice(1700000000, 0),
                    bob(1700000000, 0),
                    b"root\n",
                ),
            ),
            (
                "commit_neg_tz",
                commit(
                    &rows,
                    &["commit_root"],
                    "tree_mixed",
                    alice(1700000100, -480),
                    bob(1700000200, -480),
                    b"second\n\nbody line\n",
                ),
            ),
            (
                "commit_half_hour",
                commit(
                    &rows,
                    &["commit_neg_tz"],
                    "tree_nonutf8",
                    alice(1700000300, 330),
                    bob(1700000400, -210),
                    b"half hour",
                ),
            ),
            (
                "commit_merge",
                commit(
                    &rows,
                    &["commit_root", "commit_neg_tz", "commit_half_hour"],
                    "tree_mixed",
                    alice(1700000500, 765),
                    bob(1700000500, 765),
                    b"Merge\n",
                ),
            ),
            (
                "commit_nonutf8_name",
                commit(
                    &rows,
                    &[],
                    "tree_empty",
                    sig(b"\xff\xfe Z", "alice@example.com", 100000000, 0),
                    bob(100000000, 0),
                    b"",
                ),
            ),
        ];
        for (name, c) in cases {
            assert_eq!(c.encode(algo).unwrap(), rows[name].body, "{name}");
            assert_eq!(c.id(algo).unwrap(), rows[name].id, "{name}");
            assert_eq!(Commit::parse(algo, &rows[name].body).unwrap(), c, "{name}");
        }
    }
}

#[test]
fn signed_commit_with_encoding_round_trips() {
    for algo in ALGOS {
        let rows = fixtures(algo);
        let c = Commit::parse(algo, &rows["commit_gpgsig"].body).unwrap();
        assert_eq!(
            c.extra_headers[0],
            (b"encoding".to_vec(), b"ISO-8859-1".to_vec())
        );
        let sig = c.gpgsig().unwrap();
        assert!(sig.starts_with(b"-----BEGIN PGP SIGNATURE-----\n\n"));
        assert!(sig.ends_with(b"-----END PGP SIGNATURE-----"));
        assert_eq!(c.committer.offset_minutes, -420);
        assert_eq!(c.id(algo).unwrap(), rows["commit_gpgsig"].id);

        let mut built = c.clone();
        built.extra_headers.truncate(1);
        built.extra_headers.push((b"gpgsig".to_vec(), sig.to_vec()));
        assert_eq!(built.encode(algo).unwrap(), rows["commit_gpgsig"].body);
    }
}

#[test]
fn tags_match_git() {
    for algo in ALGOS {
        let rows = fixtures(algo);
        let cases = [
            (
                "tag_annotated",
                Tag {
                    object: rows["commit_root"].id,
                    kind: Kind::Commit,
                    name: b"v1.0".to_vec(),
                    tagger: Some(alice(1700000800, 60)),
                    message: b"release\n".to_vec(),
                },
            ),
            (
                "tag_no_tagger",
                Tag {
                    object: rows["blob_hello"].id,
                    kind: Kind::Blob,
                    name: b"old".to_vec(),
                    tagger: None,
                    message: b"legacy tag\n".to_vec(),
                },
            ),
        ];
        for (name, t) in cases {
            assert_eq!(t.encode(algo).unwrap(), rows[name].body, "{name}");
            assert_eq!(t.id(algo).unwrap(), rows[name].id, "{name}");
            assert_eq!(Tag::parse(algo, &rows[name].body).unwrap(), t, "{name}");
        }
        let signed = Tag::parse(algo, &rows["tag_signed"].body).unwrap();
        assert_eq!(signed.kind, Kind::Tree);
        assert!(signed.message.ends_with(b"-----END PGP SIGNATURE-----\n"));
    }
}

#[test]
fn tree_rejects_bad_input() {
    let id = ObjectId::from_bytes(Algorithm::Sha1, &[1; 20]).unwrap();
    let file = |n: &[u8]| entry(FileMode::Regular, n, id);
    for bad in [&b""[..], b".", b"..", b"a/b", b"a\0b"] {
        assert_eq!(
            encode_tree(Algorithm::Sha1, &[file(bad)]),
            Err(Error::InvalidName)
        );
    }
    assert_eq!(
        encode_tree(Algorithm::Sha1, &[file(b"a"), file(b"a")]),
        Err(Error::DuplicateName)
    );
    assert_eq!(
        encode_tree(
            Algorithm::Sha1,
            &[file(b"a"), entry(FileMode::Tree, b"a", id)]
        ),
        Err(Error::DuplicateName)
    );

    let mut body = b"100644 a\0".to_vec();
    body.extend_from_slice(&[1; 20]);
    assert!(parse_tree(Algorithm::Sha1, &body).is_ok());
    assert!(parse_tree(Algorithm::Sha256, &body).is_err());
    assert!(parse_tree(Algorithm::Sha1, &body[..body.len() - 1]).is_err());
    assert!(parse_tree(Algorithm::Sha1, b"100664 a\0aaaaaaaaaaaaaaaaaaaa").is_err());
    assert!(parse_tree(Algorithm::Sha1, b"040000 a\0aaaaaaaaaaaaaaaaaaaa").is_err());
    let mut unsorted = Vec::new();
    for n in [b"b", b"a"] {
        unsorted.extend_from_slice(b"100644 ");
        unsorted.extend_from_slice(n);
        unsorted.push(0);
        unsorted.extend_from_slice(&[1; 20]);
    }
    assert!(parse_tree(Algorithm::Sha1, &unsorted).is_err());
}

#[test]
fn commit_parse_rejects_malformed() {
    let a = Algorithm::Sha1;
    let good = fixtures(a)["commit_root"].body.clone();
    assert!(Commit::parse(a, &good).is_ok());
    assert!(Commit::parse(a, b"").is_err());
    assert!(Commit::parse(a, b"tree abc\n\nmsg").is_err());
    let swapped = String::from_utf8(good.clone())
        .unwrap()
        .replace("author", "xauthor");
    assert!(Commit::parse(a, swapped.as_bytes()).is_err());
    let bad_tz = String::from_utf8(good).unwrap().replace("+0000", "+00:0");
    assert!(Commit::parse(a, bad_tz.as_bytes()).is_err());
    assert!(Commit::parse(Algorithm::Sha256, &fixtures(a)["commit_root"].body).is_err());
}

#[test]
fn object_id_parsing() {
    let h1 = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391";
    let id = ObjectId::parse(h1).unwrap();
    assert_eq!(id.algorithm(), Algorithm::Sha1);
    assert_eq!(id.as_bytes().len(), 20);
    assert_eq!(id.to_string(), h1);
    assert_eq!(format!("{id:?}"), format!("sha1:{h1}"));
    let h2 = "473a0f4c3be8a93681a267e3b1e9a7dcda1185436fe141f7749120a303721813";
    assert_eq!(ObjectId::parse(h2).unwrap().algorithm(), Algorithm::Sha256);
    assert_ne!(
        ObjectId::parse(h1).unwrap(),
        ObjectId::from_hex(Algorithm::Sha1, &h1.replace('e', "d")).unwrap()
    );
    assert!(ObjectId::parse("abc").is_err());
    assert!(ObjectId::parse(&"g".repeat(40)).is_err());
    assert!(ObjectId::from_hex(Algorithm::Sha256, h1).is_err());
    assert_eq!(Algorithm::from_name("sha256"), Some(Algorithm::Sha256));
    assert_eq!(Algorithm::from_name("md5"), None);
}

#[test]
fn signature_offsets_encode_both_signs() {
    for (minutes, text) in [
        (0, "+0000"),
        (330, "+0530"),
        (-210, "-0330"),
        (-720, "-1200"),
        (840, "+1400"),
    ] {
        let mut c = commit(
            &fixtures(Algorithm::Sha1),
            &[],
            "tree_empty",
            alice(5, minutes),
            alice(5, minutes),
            b"m",
        );
        let body = String::from_utf8(c.encode(Algorithm::Sha1).unwrap()).unwrap();
        assert!(body.contains(&format!("> 5 {text}\n")), "{text}");
        assert_eq!(Commit::parse(Algorithm::Sha1, body.as_bytes()).unwrap(), c);
        c.parents.clear();
    }
}

fn mismatched_ids() -> (ObjectId, ObjectId) {
    let sha1 = ObjectId::from_bytes(Algorithm::Sha1, &[1; 20]).unwrap();
    let sha256 = ObjectId::from_bytes(Algorithm::Sha256, &[1; 32]).unwrap();
    (sha1, sha256)
}

fn mismatch(expected: Algorithm, actual: Algorithm) -> Error {
    Error::AlgorithmMismatch { expected, actual }
}

#[test]
fn tree_rejects_other_algorithm_id() {
    let (sha1, sha256) = mismatched_ids();
    let entries = [
        entry(FileMode::Regular, b"a", sha256),
        entry(FileMode::Regular, b"b", sha1),
    ];
    assert_eq!(
        encode_tree(Algorithm::Sha256, &entries),
        Err(mismatch(Algorithm::Sha256, Algorithm::Sha1))
    );
    assert_eq!(
        encode_tree(Algorithm::Sha1, &entries),
        Err(mismatch(Algorithm::Sha1, Algorithm::Sha256))
    );
}

fn commit_with(tree: ObjectId, parents: Vec<ObjectId>) -> Commit {
    Commit {
        tree,
        parents,
        author: alice(1, 0),
        committer: alice(1, 0),
        extra_headers: vec![],
        message: b"m\n".to_vec(),
    }
}

#[test]
fn commit_rejects_other_algorithm_id() {
    let (sha1, sha256) = mismatched_ids();
    let bad_tree = commit_with(sha1, vec![]);
    assert_eq!(
        bad_tree.encode(Algorithm::Sha256),
        Err(mismatch(Algorithm::Sha256, Algorithm::Sha1))
    );
    assert_eq!(
        bad_tree.id(Algorithm::Sha256),
        Err(mismatch(Algorithm::Sha256, Algorithm::Sha1))
    );
    let bad_parent = commit_with(sha256, vec![sha256, sha1]);
    assert_eq!(
        bad_parent.encode(Algorithm::Sha256),
        Err(mismatch(Algorithm::Sha256, Algorithm::Sha1))
    );
    assert!(
        commit_with(sha256, vec![sha256])
            .encode(Algorithm::Sha256)
            .is_ok()
    );
}

#[test]
fn tag_rejects_other_algorithm_id() {
    let (sha1, sha256) = mismatched_ids();
    let tag = |object| Tag {
        object,
        kind: Kind::Commit,
        name: b"v1".to_vec(),
        tagger: None,
        message: b"m\n".to_vec(),
    };
    assert_eq!(
        tag(sha1).encode(Algorithm::Sha256),
        Err(mismatch(Algorithm::Sha256, Algorithm::Sha1))
    );
    assert_eq!(
        tag(sha1).id(Algorithm::Sha256),
        Err(mismatch(Algorithm::Sha256, Algorithm::Sha1))
    );
    assert!(tag(sha256).encode(Algorithm::Sha256).is_ok());
}

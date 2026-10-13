mod common;

use std::fs;
use std::io::Read;
use std::path::Path;

use common::git::{ALGORITHMS, Git, build_history};
use pyn_git::{
    EntryKind, Error, IngestLimits, ObjectDb, PackEntry, PackWriter, index_pack, reachable_objects,
    write_pack,
};
use pyn_object::{Algorithm, Kind, ObjectId, hash_object};

/// Hands out one byte per read to exercise the streaming path.
struct Trickle<'a>(&'a [u8]);

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0.is_empty() || buf.is_empty() {
            return Ok(0);
        }
        buf[0] = self.0[0];
        self.0 = &self.0[1..];
        Ok(1)
    }
}

fn pack_dir_entries(g: &Git) -> Vec<String> {
    let dir = g.objects_dir().join("pack");
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn all_objects(g: &Git) -> Vec<ObjectId> {
    let out = g.run(&[
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objectname)",
    ]);
    String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| ObjectId::from_hex(g.algorithm, l).unwrap())
        .collect()
}

fn assert_same_objects(src: &Git, db: &ObjectDb, ids: Vec<ObjectId>) {
    for id in ids {
        let kind = src.text(&["cat-file", "-t", &id.to_hex()]);
        let want = src.run(&["cat-file", &kind, &id.to_hex()]);
        let got = db.read(&id).unwrap();
        assert_eq!(got.kind.name(), kind, "{id}");
        assert_eq!(got.data, want, "{id}");
    }
}

fn pack_bytes(g: &Git, args: &[&str], stdin: &str) -> Vec<u8> {
    g.run_stdin(args, stdin.as_bytes(), 1_700_000_000)
}

/// Ingests `pack` into a fresh bare repository and compares with `git index-pack` and the source objects.
fn check_ingest(src: &Git, pack: &[u8], expect: Option<EntryKind>) {
    let tmp = tempfile::tempdir().unwrap();
    let dst = Git::init(&tmp.path().join("dst"), src.algorithm, true);
    let db = dst.db();
    let got = index_pack(&db, Trickle(pack), &IngestLimits::default()).unwrap();
    assert_eq!(got.thin_bases, 0);
    got.pack.verify().unwrap();

    let pack_file = tmp.path().join("in.pack");
    fs::write(&pack_file, pack).unwrap();
    let ref_idx = tmp.path().join("ref.idx");
    dst.run(&[
        "index-pack",
        "-o",
        ref_idx.to_str().unwrap(),
        pack_file.to_str().unwrap(),
    ]);
    assert_eq!(
        fs::read(got.pack.idx_path()).unwrap(),
        fs::read(&ref_idx).unwrap()
    );
    assert_eq!(fs::read(got.pack.pack_path()).unwrap(), pack);

    if let Some(kind) = expect {
        let found = got
            .pack
            .ids()
            .any(|id| got.pack.entry_kind(&id).unwrap() == Some(kind));
        assert!(found, "no {kind:?} entry in the pack");
    }
    assert_same_objects(src, &dst.db(), all_objects(src));
    let history_tip = src.rev("main");
    dst.run(&["update-ref", "refs/heads/main", &history_tip.to_hex()]);
    dst.run(&["verify-pack", got.pack.idx_path().to_str().unwrap()]);
    dst.fsck();
    let names = pack_dir_entries(&dst);
    assert!(names.iter().all(|n| n.starts_with("pack-")), "{names:?}");
}

#[test]
fn ingests_packs_made_by_git() {
    for alg in ALGORITHMS {
        let tmp = tempfile::tempdir().unwrap();
        let src = Git::init(&tmp.path().join("src"), alg, false);
        build_history(&src);

        let args = [
            "pack-objects",
            "--stdout",
            "--revs",
            "--all",
            "--window=50",
            "--depth=50",
        ];
        let refd = pack_bytes(&src, &args, "");
        check_ingest(&src, &refd, Some(EntryKind::RefDelta));

        let mut ofs_args = args.to_vec();
        ofs_args.push("--delta-base-offset");
        let ofs = pack_bytes(&src, &ofs_args, "");
        check_ingest(&src, &ofs, Some(EntryKind::OfsDelta));

        let nodelta = pack_bytes(
            &src,
            &["pack-objects", "--stdout", "--revs", "--all", "--window=0"],
            "",
        );
        check_ingest(&src, &nodelta, None);

        src.run(&[
            "repack",
            "-a",
            "-d",
            "-f",
            "-q",
            "--window=50",
            "--depth=50",
        ]);
        let packs: Vec<_> = pack_dir_entries(&src)
            .into_iter()
            .filter(|n| n.ends_with(".pack"))
            .collect();
        assert_eq!(packs.len(), 1);
        let repacked = fs::read(src.objects_dir().join("pack").join(&packs[0])).unwrap();
        check_ingest(&src, &repacked, Some(EntryKind::OfsDelta));
    }
}

#[test]
fn completes_thin_packs() {
    for alg in ALGORITHMS {
        let tmp = tempfile::tempdir().unwrap();
        let src = Git::init(&tmp.path().join("src"), alg, false);
        let h = build_history(&src);
        let c1 = h.c[0].to_hex();
        let c5 = h.c[4].to_hex();

        let dst = Git::init(&tmp.path().join("dst"), alg, true);
        let db = dst.db();
        let base = pack_bytes(
            &src,
            &["pack-objects", "--stdout", "--revs"],
            &format!("{c1}\n"),
        );
        index_pack(&db, &base[..], &IngestLimits::default()).unwrap();

        let thin = pack_bytes(
            &src,
            &[
                "pack-objects",
                "--stdout",
                "--revs",
                "--thin",
                "--window=50",
                "--depth=50",
            ],
            &format!("{c5}\n^{c1}\n"),
        );
        let got = index_pack(&db, Trickle(&thin), &IngestLimits::default()).unwrap();
        assert!(got.thin_bases > 0, "git sent no thin delta");
        got.pack.verify().unwrap();
        dst.run(&["update-ref", "refs/heads/main", &c5]);
        dst.fsck();
        assert_same_objects(
            &src,
            &dst.db(),
            src.id_set(&src.run(&["rev-list", "--objects", &c5]), false)
                .into_iter()
                .collect(),
        );

        // The stored pack stands alone: git accepts it without the first pack.
        let lone = tmp.path().join("lone");
        let other = Git::init(&lone, alg, true);
        let pack_path = got.pack.pack_path();
        let copy = other
            .objects_dir()
            .join("pack")
            .join(pack_path.file_name().unwrap());
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::copy(pack_path, &copy).unwrap();
        other.run(&["index-pack", copy.to_str().unwrap()]);
        other.run(&["verify-pack", copy.with_extension("idx").to_str().unwrap()]);

        // Without the base, the same input is refused with a typed error and leaves no pack.
        let empty = Git::init(&tmp.path().join("empty"), alg, true);
        let err = index_pack(&empty.db(), &thin[..], &IngestLimits::default()).unwrap_err();
        assert!(matches!(err, Error::ObjectNotFound(_)), "{err:?}");
        assert!(pack_dir_entries(&empty).is_empty());
    }
}

#[test]
fn written_packs_are_valid() {
    for alg in ALGORITHMS {
        let tmp = tempfile::tempdir().unwrap();
        let src = Git::init(&tmp.path().join("src"), alg, false);
        let h = build_history(&src);
        let db = src.db();
        let want = [h.c[4], h.tag_of_tag];
        let ids = reachable_objects(&db, &want, &[]).unwrap().objects;

        let mut bytes = Vec::new();
        let summary = write_pack(&db, &ids, &mut bytes).unwrap();
        assert_eq!(summary.entries.len(), ids.len());

        let dst = Git::init(&tmp.path().join("dst"), alg, true);
        let pack = tmp.path().join("w.pack");
        fs::write(&pack, &bytes).unwrap();
        let idx = tmp.path().join("w.idx");
        dst.run(&[
            "index-pack",
            "--strict",
            "-o",
            idx.to_str().unwrap(),
            pack.to_str().unwrap(),
        ]);
        assert_eq!(fs::read(&idx).unwrap(), summary.index());

        let target = dst.objects_dir().join("pack");
        fs::create_dir_all(&target).unwrap();
        let name = format!("pack-{}", summary.checksum.to_hex());
        fs::copy(&pack, target.join(format!("{name}.pack"))).unwrap();
        fs::copy(&idx, target.join(format!("{name}.idx"))).unwrap();
        dst.run(&[
            "verify-pack",
            target.join(format!("{name}.idx")).to_str().unwrap(),
        ]);
        dst.run(&["update-ref", "refs/heads/main", &h.c[4].to_hex()]);
        dst.run(&["update-ref", "refs/tags/v1.1", &h.tag_of_tag.to_hex()]);
        dst.fsck();
        assert_same_objects(&src, &dst.db(), all_objects(&src));

        // Written packs ingest back to the same index.
        let again = Git::init(&tmp.path().join("again"), alg, true);
        let got = index_pack(&again.db(), &bytes[..], &IngestLimits::default()).unwrap();
        assert_eq!(fs::read(got.pack.idx_path()).unwrap(), summary.index());
    }
}

#[test]
fn writer_handles_edge_cases() {
    let tmp = tempfile::tempdir().unwrap();
    let g = Git::init(&tmp.path().join("g"), Algorithm::Sha1, true);
    let db = g.db();
    let blob = db.write(Kind::Blob, b"x").unwrap();
    let big = db.write(Kind::Blob, &vec![b'a'; 100_000]).unwrap();

    let mut out = Vec::new();
    let s = write_pack(&db, &[blob, big, blob], &mut out).unwrap();
    assert_eq!(s.entries.len(), 2);

    let mut empty = Vec::new();
    let s = write_pack(&db, &[], &mut empty).unwrap();
    assert_eq!(s.entries.len(), 0);
    let f = tmp.path().join("e.pack");
    fs::write(&f, &empty).unwrap();
    g.run(&["index-pack", f.to_str().unwrap()]);

    let missing = ObjectId::from_hex(Algorithm::Sha1, &"ab".repeat(20)).unwrap();
    assert!(matches!(
        write_pack(&db, &[missing], &mut Vec::new()),
        Err(Error::ObjectNotFound(_))
    ));

    let mut w = PackWriter::new(Vec::new(), Algorithm::Sha1, 2).unwrap();
    w.add(
        blob,
        PackEntry::Whole {
            kind: Kind::Blob,
            data: b"x",
        },
    )
    .unwrap();
    assert!(w.finish().is_err());
}

/// A tiny pack of two objects, for byte-by-byte corruption.
fn tiny(alg: Algorithm, tmp: &Path) -> (Git, Vec<u8>) {
    let src = Git::init(&tmp.join("tiny"), alg, false);
    src.write_file("f", "hello\n");
    src.commit_all("one", 1_700_000_000);
    let pack = pack_bytes(&src, &["pack-objects", "--stdout", "--revs"], "HEAD\n");
    (src, pack)
}

#[test]
fn rejects_every_truncation_and_bit_flip() {
    for alg in ALGORITHMS {
        let tmp = tempfile::tempdir().unwrap();
        let (_src, pack) = tiny(alg, tmp.path());
        let dst = Git::init(&tmp.path().join("dst"), alg, true);
        let db = dst.db();
        for len in 0..pack.len() {
            let err = index_pack(&db, &pack[..len], &IngestLimits::default());
            assert!(err.is_err(), "truncated to {len} was accepted");
            assert!(pack_dir_entries(&dst).is_empty(), "left files at {len}");
        }
        for at in 0..pack.len() {
            let mut bad = pack.clone();
            bad[at] ^= 0x55;
            let err = index_pack(&db, &bad[..], &IngestLimits::default());
            assert!(err.is_err(), "flip at {at} was accepted");
            assert!(pack_dir_entries(&dst).is_empty(), "left files at {at}");
        }
        assert!(index_pack(&db, &pack[..], &IngestLimits::default()).is_ok());
    }
}

#[test]
fn error_kinds_are_typed() {
    let alg = Algorithm::Sha1;
    let tmp = tempfile::tempdir().unwrap();
    let (_src, pack) = tiny(alg, tmp.path());
    let dst = Git::init(&tmp.path().join("dst"), alg, true);
    let db = dst.db();
    let limits = IngestLimits::default();

    assert!(matches!(
        index_pack(&db, &pack[..5], &limits),
        Err(Error::Truncated(_))
    ));
    assert!(matches!(
        index_pack(&db, &pack[..pack.len() - 3], &limits),
        Err(Error::Truncated(_))
    ));
    let mut bad = pack.clone();
    bad[0] = b'X';
    assert!(matches!(
        index_pack(&db, &bad[..], &limits),
        Err(Error::Corrupt { .. })
    ));
    let mut bad = pack.clone();
    bad[7] = 9;
    assert!(matches!(
        index_pack(&db, &bad[..], &limits),
        Err(Error::Unsupported(_))
    ));
    let mut bad = pack.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(matches!(
        index_pack(&db, &bad[..], &limits),
        Err(Error::Checksum { .. })
    ));
    let small = IngestLimits {
        max_objects: 1,
        ..limits
    };
    assert!(matches!(
        index_pack(&db, &pack[..], &small),
        Err(Error::Limit { .. })
    ));
    let small = IngestLimits {
        max_pack_size: 20,
        ..limits
    };
    assert!(matches!(
        index_pack(&db, &pack[..], &small),
        Err(Error::Limit { .. })
    ));
    let small = IngestLimits {
        max_object_size: 3,
        ..limits
    };
    assert!(matches!(
        index_pack(&db, &pack[..], &small),
        Err(Error::Limit { .. })
    ));
    assert!(pack_dir_entries(&dst).is_empty());
}

fn entry_header(type_bits: u8, size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut byte = (type_bits << 4) | (size & 0x0f) as u8;
    let mut rest = size >> 4;
    while rest != 0 {
        out.push(byte | 0x80);
        byte = (rest & 0x7f) as u8;
        rest >>= 7;
    }
    out.push(byte);
    out
}

fn zlib(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

/// A delta that rewrites `base_len` bytes into a copy of itself plus `tail`.
fn delta_append(base_len: usize, tail: u8) -> Vec<u8> {
    let varint = |mut n: usize| {
        let mut v = Vec::new();
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                v.push(b);
                return v;
            }
            v.push(b | 0x80);
        }
    };
    let mut d = varint(base_len);
    d.extend(varint(base_len + 1));
    d.extend([0x90, base_len as u8]);
    d.extend([1, tail]);
    d
}

fn seal(alg: Algorithm, mut body: Vec<u8>) -> Vec<u8> {
    use sha1_checked::Digest as _;
    let sum = match alg {
        Algorithm::Sha1 => sha1_checked::Sha1::digest(&body).to_vec(),
        Algorithm::Sha256 => {
            use sha2::Digest as _;
            sha2::Sha256::digest(&body).to_vec()
        }
    };
    body.extend(sum);
    body
}

fn pack_header(count: u32) -> Vec<u8> {
    let mut v = b"PACK".to_vec();
    v.extend(2u32.to_be_bytes());
    v.extend(count.to_be_bytes());
    v
}

/// A blob followed by `n` offset deltas, each appending one byte to its predecessor.
fn chain_pack(alg: Algorithm, n: usize) -> Vec<u8> {
    let mut body = pack_header(n as u32 + 1);
    let base = b"seed".to_vec();
    let mut offsets = vec![body.len()];
    body.extend(entry_header(3, base.len()));
    body.extend(zlib(&base));
    for i in 0..n {
        let d = delta_append(4 + i, b'a' + (i % 26) as u8);
        let at = body.len();
        let mut back = at - offsets[i];
        let mut enc = vec![(back & 0x7f) as u8];
        back >>= 7;
        while back > 0 {
            back -= 1;
            enc.push(0x80 | (back & 0x7f) as u8);
            back >>= 7;
        }
        enc.reverse();
        body.extend(entry_header(6, d.len()));
        body.extend(enc);
        body.extend(zlib(&d));
        offsets.push(at);
    }
    seal(alg, body)
}

#[test]
fn delta_chains_are_bounded_and_resolved() {
    for alg in ALGORITHMS {
        let tmp = tempfile::tempdir().unwrap();
        let dst = Git::init(&tmp.path().join("dst"), alg, true);
        let db = dst.db();
        let pack = chain_pack(alg, 20);

        let ok = index_pack(&db, &pack[..], &IngestLimits::default()).unwrap();
        assert_eq!(ok.pack.len(), 21);
        let tip = hash_object(alg, Kind::Blob, b"seedabcdefghijklmnopqrst").unwrap();
        assert_eq!(db.read(&tip).unwrap().data, b"seedabcdefghijklmnopqrst");
        let f = tmp.path().join("c.pack");
        fs::write(&f, &pack).unwrap();
        dst.run(&["index-pack", f.to_str().unwrap()]);

        let strict = IngestLimits {
            max_delta_depth: 5,
            ..IngestLimits::default()
        };
        let other = Git::init(&tmp.path().join("other"), alg, true);
        let err = index_pack(&other.db(), &pack[..], &strict).unwrap_err();
        assert!(matches!(err, Error::Limit { .. }), "{err:?}");
        assert!(pack_dir_entries(&other).is_empty());
    }
}

#[test]
fn rejects_duplicate_objects_and_orphan_deltas() {
    let alg = Algorithm::Sha1;
    let tmp = tempfile::tempdir().unwrap();
    let dst = Git::init(&tmp.path().join("dst"), alg, true);
    let db = dst.db();

    let mut body = pack_header(2);
    for _ in 0..2 {
        body.extend(entry_header(3, 1));
        body.extend(zlib(b"x"));
    }
    let err = index_pack(&db, &seal(alg, body)[..], &IngestLimits::default()).unwrap_err();
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");

    // An offset delta whose base offset is not the start of an entry.
    let mut body = pack_header(2);
    body.extend(entry_header(3, 1));
    body.extend(zlib(b"x"));
    let d = delta_append(1, b'y');
    body.extend(entry_header(6, d.len()));
    body.push(3);
    body.extend(zlib(&d));
    let err = index_pack(&db, &seal(alg, body)[..], &IngestLimits::default()).unwrap_err();
    assert!(matches!(err, Error::Corrupt { .. }), "{err:?}");

    // Two reference deltas that name each other never resolve.
    let a = hash_object(alg, Kind::Blob, b"x").unwrap();
    let ghost = ObjectId::from_hex(alg, &"cd".repeat(20)).unwrap();
    let mut body = pack_header(1);
    let d = delta_append(1, b'y');
    body.extend(entry_header(7, d.len()));
    body.extend(ghost.as_bytes());
    body.extend(zlib(&d));
    let err = index_pack(&db, &seal(alg, body)[..], &IngestLimits::default()).unwrap_err();
    assert!(
        matches!(err, Error::ObjectNotFound(id) if id == ghost),
        "{err:?}"
    );
    let _ = a;
    assert!(pack_dir_entries(&dst).is_empty());
}

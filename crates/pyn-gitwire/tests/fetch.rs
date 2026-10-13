mod common;

use std::io::Read;

use common::*;
use pyn_gitwire::{Error, FetchRequest, LsRefsOptions, NoHaves, ProtocolVersion, UploadPack};
use pyn_object::{Algorithm, ObjectId};

const ALGS: [Algorithm; 2] = [Algorithm::Sha1, Algorithm::Sha256];

#[test]
fn ls_refs_matches_git_across_transports() {
    for alg in ALGS {
        if !supported(alg) {
            continue;
        }
        let repo = origin(alg);
        let p = repo.path();
        let main = id(&git(p, &["rev-parse", "refs/heads/main"]));
        let topic = id(&git(p, &["rev-parse", "refs/heads/topic"]));
        let v1 = id(&git(p, &["rev-parse", "refs/tags/v1"]));
        let v1_commit = id(&git(p, &["rev-parse", "refs/tags/v1^{commit}"]));
        for (name, mut t) in shapes(p) {
            let mut up = UploadPack::open(t.as_mut(), alg).unwrap();
            assert_eq!(up.object_format(), alg, "{name}");
            let all = up
                .ls_refs(&LsRefsOptions {
                    symrefs: true,
                    peel: true,
                    ..Default::default()
                })
                .unwrap();
            let get = |n: &str| {
                all.iter()
                    .find(|r| r.name == n)
                    .unwrap_or_else(|| panic!("{name}: no {n}"))
            };
            assert_eq!(get("refs/heads/main").id, Some(main), "{name}");
            assert_eq!(get("refs/heads/topic").id, Some(topic), "{name}");
            assert_eq!(get("refs/tags/v1").id, Some(v1), "{name}");
            assert_eq!(get("refs/tags/v1").peeled, Some(v1_commit), "{name}");
            assert_eq!(
                get("HEAD").symref_target.as_deref(),
                Some("refs/heads/main"),
                "{name}"
            );
            assert_eq!(get("HEAD").id, Some(main), "{name}");

            let tags = up
                .ls_refs(&LsRefsOptions {
                    prefixes: vec!["refs/tags/".into()],
                    ..Default::default()
                })
                .unwrap();
            let mut names: Vec<_> = tags.iter().map(|r| r.name.as_str()).collect();
            names.sort();
            assert_eq!(names, ["refs/tags/light", "refs/tags/v1"], "{name}");
            assert!(tags.iter().all(|r| r.peeled.is_none()), "{name}");
            let expect = if name.ends_with("v2") {
                ProtocolVersion::V2
            } else {
                ProtocolVersion::V0
            };
            assert_eq!(up.version(), expect);
        }
    }
}

#[test]
fn unborn_head_in_empty_repository() {
    let repo = init(Algorithm::Sha1, true);
    for (name, mut t) in shapes(repo.path()) {
        let mut up = UploadPack::open(t.as_mut(), Algorithm::Sha1).unwrap();
        let refs = up
            .ls_refs(&LsRefsOptions {
                symrefs: true,
                unborn: true,
                ..Default::default()
            })
            .unwrap();
        if name.ends_with("v2") {
            assert_eq!(refs.len(), 1, "{name}");
            assert_eq!(
                (refs[0].name.as_str(), refs[0].id),
                ("HEAD", None),
                "{name}"
            );
            assert_eq!(refs[0].symref_target.as_deref(), Some("refs/heads/main"));
        } else {
            assert!(refs.is_empty(), "{name}");
        }
    }
}

#[test]
fn object_format_mismatch_is_refused() {
    if !supported(Algorithm::Sha256) {
        return;
    }
    let repo = origin(Algorithm::Sha256);
    for (name, mut t) in shapes(repo.path()) {
        match UploadPack::open(t.as_mut(), Algorithm::Sha1) {
            Err(Error::ObjectFormatMismatch { server, local }) => assert_eq!(
                (server, local),
                (Algorithm::Sha256, Algorithm::Sha1),
                "{name}"
            ),
            other => panic!("{name}: {:?}", other.err()),
        }
    }
    let sha1 = origin(Algorithm::Sha1);
    for (name, mut t) in shapes(sha1.path()) {
        assert!(
            matches!(
                UploadPack::open(t.as_mut(), Algorithm::Sha256),
                Err(Error::ObjectFormatMismatch { .. })
            ),
            "{name}"
        );
    }
}

fn read_all(r: &mut impl Read) -> Vec<u8> {
    let mut v = Vec::new();
    r.read_to_end(&mut v).unwrap();
    v
}

#[test]
fn clone_fetch_verifies_with_git() {
    for alg in ALGS {
        if !supported(alg) {
            continue;
        }
        let repo = origin(alg);
        let p = repo.path();
        let wants: Vec<ObjectId> = ["refs/heads/main", "refs/heads/topic", "refs/tags/v1"]
            .iter()
            .map(|r| id(&git(p, &["rev-parse", r])))
            .collect();
        for (name, mut t) in shapes(p) {
            let mut up = UploadPack::open(t.as_mut(), alg).unwrap();
            let mut progress = Vec::new();
            let pack = {
                let mut req = FetchRequest::new(wants.clone());
                req.include_tag = true;
                let mut r = up
                    .fetch(
                        &req,
                        &mut NoHaves,
                        Box::new(|m| progress.extend_from_slice(m)),
                    )
                    .unwrap();
                read_all(&mut r)
            };
            assert!(pack.starts_with(b"PACK"), "{name}");
            verify_pack(alg, None, &pack, &wants);
        }
    }
}

#[test]
fn incremental_fetch_negotiates_and_sends_a_smaller_thin_pack() {
    for alg in ALGS {
        if !supported(alg) {
            continue;
        }
        let repo = origin(alg);
        let p = repo.path();
        let history: Vec<ObjectId> = git(p, &["rev-list", "HEAD"]).lines().map(id).collect();
        let base = tempfile::TempDir::new().unwrap();
        git(
            base.path(),
            &["clone", "-q", "--bare", p.to_str().unwrap(), "."],
        );
        std::fs::write(p.join("big.txt"), "x".repeat(4000)).unwrap();
        git(p, &["add", "."]);
        git(p, &["commit", "-q", "-m", "four"]);
        let tip = id(&git(p, &["rev-parse", "HEAD"]));

        let full = {
            let mut t = shapes(p).remove(0).1;
            let mut up = UploadPack::open(t.as_mut(), alg).unwrap();
            read_all(
                &mut up
                    .fetch(
                        &FetchRequest::new(vec![tip]),
                        &mut NoHaves,
                        Box::new(|_| {}),
                    )
                    .unwrap(),
            )
        };

        for (name, mut t) in shapes(p) {
            let mut up = UploadPack::open(t.as_mut(), alg).unwrap();
            // Unknown ids first so that more than one round runs, then the real history.
            let mut candidates: Vec<ObjectId> = (1..40u8)
                .map(|i| ObjectId::from_bytes(alg, &vec![i; alg.digest_len()]).unwrap())
                .collect();
            candidates.extend(&history);
            candidates.reverse();
            let mut calls = 0;
            let mut src = |max: usize| {
                calls += 1;
                (0..max)
                    .filter_map(|_| candidates.pop())
                    .collect::<Vec<_>>()
            };
            let mut req = FetchRequest::new(vec![tip]);
            req.thin_pack = true;
            let pack = read_all(&mut up.fetch(&req, &mut src, Box::new(|_| {})).unwrap());
            assert!(
                pack.len() < full.len(),
                "{name}: {} !< {}",
                pack.len(),
                full.len()
            );
            verify_pack(alg, Some(base.path()), &pack, &[tip]);
            if name.ends_with("v2") {
                assert!(calls >= 2, "{name}: negotiation took {calls} call(s)");
            }
        }
    }
}

#[test]
fn fetch_without_wants_is_a_usage_error() {
    let repo = origin(Algorithm::Sha1);
    let mut t = shapes(repo.path()).remove(0).1;
    let mut up = UploadPack::open(t.as_mut(), Algorithm::Sha1).unwrap();
    assert!(matches!(
        up.fetch(&FetchRequest::new(vec![]), &mut NoHaves, Box::new(|_| {})),
        Err(Error::Usage(_))
    ));
}

#[test]
fn unknown_want_surfaces_the_remote_error() {
    let repo = origin(Algorithm::Sha1);
    let bogus = ObjectId::from_bytes(Algorithm::Sha1, &[7; 20]).unwrap();
    for (name, mut t) in shapes(repo.path()) {
        let mut up = UploadPack::open(t.as_mut(), Algorithm::Sha1).unwrap();
        let res = up
            .fetch(
                &FetchRequest::new(vec![bogus]),
                &mut NoHaves,
                Box::new(|_| {}),
            )
            .and_then(|mut r| {
                let mut v = Vec::new();
                r.read_to_end(&mut v).map_err(Error::from)
            });
        assert!(matches!(res, Err(Error::Remote(_))), "{name}: {res:?}");
    }
}

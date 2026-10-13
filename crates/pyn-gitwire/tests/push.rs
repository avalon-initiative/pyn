mod common;

use common::*;
use pyn_gitwire::process::{LocalGit, Shape};
use pyn_gitwire::{
    Error, ProtocolVersion, PushOptions, PushReport, ReceivePack, RefResult, RefStatus, RefUpdate,
    Transport,
};
use pyn_object::{Algorithm, ObjectId};

const ALGS: [Algorithm; 2] = [Algorithm::Sha1, Algorithm::Sha256];

fn transports(repo: &std::path::Path) -> Vec<(&'static str, Box<dyn Transport>)> {
    vec![
        (
            "duplex",
            Box::new(LocalGit::new(repo, Shape::Duplex).force_version(ProtocolVersion::V0)),
        ),
        (
            "stateless",
            Box::new(LocalGit::new(repo, Shape::StatelessRpc).force_version(ProtocolVersion::V0)),
        ),
    ]
}

fn make_pack(src: &std::path::Path, revs: &[&str]) -> Vec<u8> {
    git_stdin(
        src,
        &["pack-objects", "--revs", "--stdout", "-q"],
        revs.join("\n").as_bytes(),
    )
}

fn push(
    t: &mut dyn Transport,
    alg: Algorithm,
    ups: &[RefUpdate],
    pack: Option<&[u8]>,
    opts: &PushOptions,
) -> PushReport {
    let mut rp = ReceivePack::open(t, alg).unwrap();
    let mut r = pack;
    let mut progress = |_: &[u8]| {};
    rp.push(
        ups,
        r.as_mut().map(|p| p as &mut dyn std::io::Read),
        opts,
        &mut progress,
    )
    .unwrap()
}

fn bare(alg: Algorithm) -> tempfile::TempDir {
    let d = init(alg, true);
    git(d.path(), &["config", "receive.denyNonFastForwards", "true"]);
    d
}

#[test]
fn create_update_and_delete_move_refs() {
    for alg in ALGS {
        if !supported(alg) {
            continue;
        }
        let src = origin(alg);
        let sp = src.path();
        let first = id(&git(sp, &["rev-parse", "HEAD~1"]));
        let tip = id(&git(sp, &["rev-parse", "HEAD"]));
        for idx in 0..2 {
            let dst = bare(alg);
            let dp = dst.path();
            let mk = |dp: &std::path::Path| transports(dp).remove(idx);
            let (name, mut t) = mk(dp);

            let pack = make_pack(sp, &[&first.to_hex()]);
            let rep = push(
                t.as_mut(),
                alg,
                &[
                    RefUpdate::create("refs/heads/main", first),
                    RefUpdate::create("refs/heads/topic", first),
                ],
                Some(&pack),
                &PushOptions::default(),
            );
            assert!(rep.is_ok(), "{name}: {rep:?}");
            assert_eq!(rep.refs.len(), 2);
            assert_eq!(
                git(dp, &["rev-parse", "refs/heads/main"]),
                first.to_hex(),
                "{name}"
            );
            git(dp, &["fsck", "--strict"]);

            let (_, mut t) = mk(dp);
            let pack = make_pack(sp, &[&tip.to_hex(), &format!("^{first}")]);
            let rep = push(
                t.as_mut(),
                alg,
                &[RefUpdate::update("refs/heads/main", first, tip)],
                Some(&pack),
                &PushOptions {
                    quiet: true,
                    ..Default::default()
                },
            );
            assert_eq!(
                rep.refs,
                vec![RefResult {
                    name: "refs/heads/main".into(),
                    status: RefStatus::Ok
                }],
                "{name}"
            );
            assert_eq!(git(dp, &["rev-parse", "refs/heads/main"]), tip.to_hex());
            git(dp, &["fsck", "--strict"]);

            let (_, mut t) = mk(dp);
            let rep = push(
                t.as_mut(),
                alg,
                &[RefUpdate::delete("refs/heads/topic", first)],
                None,
                &PushOptions::default(),
            );
            assert!(rep.is_ok(), "{name}: {rep:?}");
            assert!(
                git_cmd(dp)
                    .args(["rev-parse", "--verify", "-q", "refs/heads/topic"])
                    .output()
                    .unwrap()
                    .status
                    .code()
                    == Some(1),
                "{name}: topic still exists"
            );
        }
    }
}

#[test]
fn rejections_are_reported_per_ref() {
    for alg in ALGS {
        if !supported(alg) {
            continue;
        }
        let src = origin(alg);
        let sp = src.path();
        let first = id(&git(sp, &["rev-parse", "HEAD~1"]));
        let tip = id(&git(sp, &["rev-parse", "HEAD"]));
        for idx in 0..2 {
            let dst = bare(alg);
            let dp = dst.path();
            let (name, mut t) = transports(dp).remove(idx);
            let pack = make_pack(sp, &[&tip.to_hex()]);
            assert!(
                push(
                    t.as_mut(),
                    alg,
                    &[
                        RefUpdate::create("refs/heads/main", tip),
                        RefUpdate::create("refs/heads/other", tip)
                    ],
                    Some(&pack),
                    &PushOptions::default()
                )
                .is_ok()
            );

            // Rewinding is refused by denyNonFastForwards while a sibling update still succeeds.
            let (_, mut t) = transports(dp).remove(idx);
            let pack = make_pack(sp, &[&first.to_hex()]);
            let rep = push(
                t.as_mut(),
                alg,
                &[
                    RefUpdate::update("refs/heads/main", tip, first),
                    RefUpdate::create("refs/heads/new", first),
                ],
                Some(&pack),
                &PushOptions::default(),
            );
            assert!(!rep.is_ok(), "{name}");
            assert!(
                matches!(rep.refs[0].status, RefStatus::Rejected(_)),
                "{name}: {rep:?}"
            );
            assert_eq!(rep.refs[1].status, RefStatus::Ok, "{name}");
            assert_eq!(git(dp, &["rev-parse", "refs/heads/main"]), tip.to_hex());
            assert_eq!(git(dp, &["rev-parse", "refs/heads/new"]), first.to_hex());

            // Atomic: one rejection fails every update.
            let (_, mut t) = transports(dp).remove(idx);
            let rep = push(
                t.as_mut(),
                alg,
                &[
                    RefUpdate::update("refs/heads/main", tip, first),
                    RefUpdate::create("refs/heads/atomic", first),
                ],
                Some(&pack),
                &PushOptions {
                    atomic: true,
                    ..Default::default()
                },
            );
            assert!(
                rep.refs
                    .iter()
                    .all(|r| matches!(r.status, RefStatus::Rejected(_))),
                "{name}: {rep:?}"
            );
            assert!(
                git_cmd(dp)
                    .args(["rev-parse", "--verify", "-q", "refs/heads/atomic"])
                    .output()
                    .unwrap()
                    .status
                    .code()
                    == Some(1)
            );

            // A stale old id fails the compare-and-swap.
            let (_, mut t) = transports(dp).remove(idx);
            let stale = ObjectId::from_bytes(alg, &vec![9; alg.digest_len()]).unwrap();
            let rep = push(
                t.as_mut(),
                alg,
                &[RefUpdate::update("refs/heads/other", stale, first)],
                Some(&pack),
                &PushOptions::default(),
            );
            assert!(
                matches!(rep.refs[0].status, RefStatus::Rejected(_)),
                "{name}: {rep:?}"
            );
            assert_eq!(git(dp, &["rev-parse", "refs/heads/other"]), tip.to_hex());

            // Deleting the checked-out branch of the bare repo's HEAD is refused.
            let (_, mut t) = transports(dp).remove(idx);
            git(dp, &["config", "receive.denyDeleteCurrent", "true"]);
            let rep = push(
                t.as_mut(),
                alg,
                &[RefUpdate::delete("refs/heads/main", tip)],
                None,
                &PushOptions::default(),
            );
            assert!(
                matches!(rep.refs[0].status, RefStatus::Rejected(_)),
                "{name}: {rep:?}"
            );
            assert_eq!(git(dp, &["rev-parse", "refs/heads/main"]), tip.to_hex());
        }
    }
}

#[test]
fn missing_objects_are_rejected() {
    let alg = Algorithm::Sha1;
    let src = origin(alg);
    let tip = id(&git(src.path(), &["rev-parse", "HEAD"]));
    let empty = git_stdin(src.path(), &["pack-objects", "--stdout", "-q"], b"");
    for idx in 0..2 {
        let dst = bare(alg);
        let (name, mut t) = transports(dst.path()).remove(idx);
        let rep = push(
            t.as_mut(),
            alg,
            &[RefUpdate::create("refs/heads/main", tip)],
            Some(&empty),
            &PushOptions::default(),
        );
        assert!(!rep.is_ok(), "{name}: {rep:?}");
        assert!(
            matches!(rep.refs[0].status, RefStatus::Rejected(_)),
            "{name}"
        );
    }
}

#[test]
fn advertised_refs_and_usage_errors() {
    for alg in ALGS {
        if !supported(alg) {
            continue;
        }
        let src = origin(alg);
        let tip = id(&git(src.path(), &["rev-parse", "HEAD"]));
        let (_, mut t) = transports(src.path()).remove(0);
        let mut rp = ReceivePack::open(t.as_mut(), alg).unwrap();
        assert!(
            rp.refs()
                .iter()
                .any(|r| r.name == "refs/heads/main" && r.id == tip)
        );
        assert!(rp.capabilities().has("delete-refs"));
        let mut p = progress_sink();
        assert!(matches!(
            rp.push(&[], None, &PushOptions::default(), &mut p),
            Err(Error::Usage(_))
        ));
        assert!(matches!(
            rp.push(
                &[RefUpdate::create("refs/heads/x", tip)],
                None,
                &PushOptions::default(),
                &mut p
            ),
            Err(Error::Usage(_))
        ));
        let mut empty: &[u8] = b"";
        assert!(matches!(
            rp.push(
                &[RefUpdate::delete("refs/heads/x", tip)],
                Some(&mut empty),
                &PushOptions::default(),
                &mut p
            ),
            Err(Error::Usage(_))
        ));
    }
}

fn progress_sink() -> impl FnMut(&[u8]) {
    |_| {}
}

#[test]
fn receive_pack_format_mismatch_is_refused() {
    if !supported(Algorithm::Sha256) {
        return;
    }
    let dst = bare(Algorithm::Sha256);
    let (_, mut t) = transports(dst.path()).remove(0);
    assert!(matches!(
        ReceivePack::open(t.as_mut(), Algorithm::Sha1),
        Err(Error::ObjectFormatMismatch { .. })
    ));
}

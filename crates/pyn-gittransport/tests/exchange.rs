mod common;

use std::path::{Path, PathBuf};

use common::*;
use pyn_git::Repository;
use pyn_gittransport::exchange::{fetch_objects, list_remote_refs, push_objects};
use pyn_gittransport::{Env, Result};
use pyn_gitwire::{LsRefsOptions, PushOptions, RefUpdate};
use pyn_object::{Algorithm, ObjectId};
use tempfile::tempdir;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Ssh,
    Http,
}

struct Remote {
    url: String,
    env: Env,
    _server: Option<Server>,
}

fn remote(kind: Kind, root: &Path, bare: &Path, home: &Path) -> Remote {
    match kind {
        Kind::Ssh => {
            let shim = ssh_shim(root, false);
            Remote {
                url: format!("host:{}", bare.display()),
                env: env(home).with("GIT_SSH_COMMAND", shim.to_str().unwrap()),
                _server: None,
            }
        }
        Kind::Http => {
            let server = Server::start(bare.parent().unwrap(), Opts::default());
            Remote {
                url: format!(
                    "{}/{}",
                    server.url,
                    bare.file_name().unwrap().to_str().unwrap()
                ),
                env: env(home),
                _server: Some(server),
            }
        }
    }
}

fn local_refs(work: &Path) -> Vec<(String, String)> {
    run(
        work,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
            "refs/tags",
        ],
    )
    .lines()
    .map(|l| {
        let (n, i) = l.split_once(' ').unwrap();
        (n.to_owned(), i.to_owned())
    })
    .collect()
}

fn push_all(
    r: &Remote,
    work: &Path,
    old: &[(String, ObjectId)],
) -> Result<pyn_gitwire::PushReport> {
    let repo = Repository::open(work).unwrap();
    let alg = repo.algorithm();
    let updates: Vec<RefUpdate> = local_refs(work)
        .into_iter()
        .filter_map(|(name, id)| {
            let new = ObjectId::from_hex(alg, &id).unwrap();
            match old.iter().find(|(n, _)| *n == name) {
                Some((_, o)) if *o == new => None,
                Some((_, o)) => Some(RefUpdate::update(name, *o, new)),
                None => Some(RefUpdate::create(name, new)),
            }
        })
        .collect();
    let mut conn = connector("", r.env.clone()).connect(&r.url, true)?;
    push_objects(
        &repo,
        &mut *conn.transport,
        &updates,
        &PushOptions::default(),
        &mut |_| {},
    )
}

fn remote_ids(r: &Remote, alg: Algorithm) -> Vec<(String, ObjectId)> {
    let mut conn = connector("", r.env.clone()).connect(&r.url, false).unwrap();
    let opts = LsRefsOptions {
        prefixes: vec!["refs/heads/".into(), "refs/tags/".into()],
        ..LsRefsOptions::default()
    };
    let (_, refs) = list_remote_refs(&mut *conn.transport, Some(alg), &opts).unwrap();
    refs.into_iter().map(|r| (r.name, r.id.unwrap())).collect()
}

fn fetch_into(r: &Remote, dest: &Path, format: &str) -> usize {
    if !dest.exists() {
        init(dest, format, false);
    }
    let repo = Repository::open(dest).unwrap();
    let refs = remote_ids(r, repo.algorithm());
    let wants: Vec<ObjectId> = refs.iter().map(|(_, id)| *id).collect();
    let mut conn = connector("", r.env.clone()).connect(&r.url, false).unwrap();
    let mut progress = 0usize;
    fetch_objects(&repo, &mut *conn.transport, &wants, &mut |m| {
        progress += m.len()
    })
    .unwrap();
    for (name, id) in &refs {
        run(dest, &["update-ref", name, &id.to_hex()]);
    }
    fsck(dest);
    refs.len()
}

fn roundtrip(kind: Kind, format: &'static str) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let work = root.join("work");
    sample_work(&work, format);
    let bare = root.join("served.git");
    init(&bare, format, true);
    let r = remote(kind, root, &bare, &root.join("home"));
    let alg = Algorithm::from_name(format).unwrap();

    let report = push_all(&r, &work, &[]).unwrap();
    assert!(report.is_ok(), "{kind:?} {format}: {report:?}");
    fsck(&bare);
    let heads = oracle_refs(&bare, &["--heads", "--tags"]);
    for (name, id) in local_refs(&work) {
        assert_eq!(heads[&name], id, "{name}");
    }

    let clone = root.join("clone");
    assert!(fetch_into(&r, &clone, format) >= 4);
    assert_eq!(
        run(&clone, &["rev-parse", "refs/heads/main"]),
        run(&work, &["rev-parse", "main"])
    );

    // A second round: new history on both sides, so the push and the fetch each share a base with the peer.
    commit(&work, "d.txt", b"four\n", "four");
    run(&work, &["tag", "v3"]);
    let old = remote_ids(&r, alg);
    let report = push_all(&r, &work, &old).unwrap();
    assert!(report.is_ok(), "{report:?}");
    fsck(&bare);
    fetch_into(&r, &clone, format);
    assert_eq!(
        run(&clone, &["rev-parse", "refs/tags/v3"]),
        run(&work, &["rev-parse", "v3"])
    );
}

#[test]
fn round_trips_over_ssh_and_http_for_both_object_formats() {
    for kind in [Kind::Ssh, Kind::Http] {
        for format in FORMATS {
            roundtrip(kind, format);
        }
    }
}

#[test]
fn deletes_and_stale_updates_are_reported() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let bare = sample_bare(root, "sha1");
    let work = root.join("seed");
    let r = remote(Kind::Ssh, root, &bare, &root.join("home"));
    let repo = Repository::open(&work).unwrap();
    let feature = remote_ids(&r, Algorithm::Sha1)
        .into_iter()
        .find(|(n, _)| n == "refs/heads/feature")
        .unwrap()
        .1;
    let mut conn = connector("", r.env.clone()).connect(&r.url, true).unwrap();
    let report = push_objects(
        &repo,
        &mut *conn.transport,
        &[RefUpdate::delete("refs/heads/feature", feature)],
        &PushOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert!(!oracle_refs(&bare, &[]).contains_key("refs/heads/feature"));

    let main = repo.refs().resolve("refs/heads/main").unwrap().unwrap();
    let mut conn = connector("", r.env.clone()).connect(&r.url, true).unwrap();
    let report = push_objects(
        &repo,
        &mut *conn.transport,
        &[RefUpdate::update("refs/heads/main", feature, main)],
        &PushOptions::default(),
        &mut |_| {},
    )
    .unwrap();
    assert!(
        !report.is_ok(),
        "a stale old value must be rejected: {report:?}"
    );
}

fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn large_pushes_stream_and_large_fetch_requests_are_gzipped() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let work = root.join("work");
    sample_work(&work, "sha1");
    for i in 0..30 {
        run(&work, &["switch", "-q", "-c", &format!("b{i}"), "main"]);
        commit(&work, "n.txt", format!("{i}\n").as_bytes(), "n");
    }
    run(&work, &["switch", "-q", "main"]);
    commit(&work, "big.bin", &noise(3 << 20), "big");
    let bare = root.join("served.git");
    init(&bare, "sha1", true);
    let server = Server::start(root, Opts::default());
    let url = format!("{}/served.git", server.url);
    let r = Remote {
        url,
        env: env(&root.join("home")),
        _server: None,
    };
    let report = push_all(&r, &work, &[]).unwrap();
    assert!(report.is_ok(), "{report:?}");
    fsck(&bare);
    let push = server
        .log()
        .into_iter()
        .find(|s| s.method == "POST" && s.target.ends_with("git-receive-pack"))
        .unwrap();
    assert_eq!(
        push.headers.get("transfer-encoding").map(String::as_str),
        Some("chunked"),
        "{push:?}"
    );
    assert!(!push.headers.contains_key("content-length"), "{push:?}");
    assert_eq!(push.status, 200);

    server.clear_log();
    let dest: PathBuf = root.join("clone");
    assert!(fetch_into(&r, &dest, "sha1") > 30);
    let fetch = server
        .log()
        .into_iter()
        .find(|s| {
            s.method == "POST"
                && s.target.ends_with("git-upload-pack")
                && s.headers.contains_key("content-encoding")
        })
        .expect("a gzipped fetch request");
    assert_eq!(fetch.headers["content-encoding"], "gzip");
}

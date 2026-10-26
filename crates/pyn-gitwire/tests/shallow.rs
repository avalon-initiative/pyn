mod common;

use std::io::Read;

use common::*;
use pyn_gitwire::{Deepen, FetchRequest, NoHaves, ShallowInfo, UploadPack};
use pyn_object::{Algorithm, ObjectId};

fn linear(n: usize) -> (tempfile::TempDir, Vec<ObjectId>) {
    let d = init(Algorithm::Sha1, false);
    let ids: Vec<ObjectId> = (0..n)
        .map(|i| commit(d.path(), "f.txt", &format!("c{i}")))
        .collect();
    (d, ids)
}

fn fetch(
    t: &mut dyn pyn_gitwire::Transport,
    wants: Vec<ObjectId>,
    held: Vec<ObjectId>,
    deepen: Deepen,
) -> (ShallowInfo, Vec<u8>) {
    let mut up = UploadPack::open(t, Algorithm::Sha1).unwrap();
    let mut req = FetchRequest::new(wants);
    req.shallow = held;
    req.deepen = Some(deepen);
    let mut pack = Vec::new();
    up.fetch(&req, &mut NoHaves, Box::new(|_| {}))
        .unwrap()
        .read_to_end(&mut pack)
        .unwrap();
    (up.shallow_info().clone(), pack)
}

fn depth(n: u32, relative: bool) -> Deepen {
    Deepen {
        depth: Some(n),
        relative,
        ..Deepen::default()
    }
}

#[test]
fn depth_and_deepen_report_the_boundary_in_every_shape() {
    let (repo, ids) = linear(5);
    let tip = ids[4];
    for (name, mut t) in shapes(repo.path()) {
        let (info, pack) = fetch(t.as_mut(), vec![tip], vec![], depth(2, false));
        assert_eq!(info.shallow, vec![ids[3]], "{name}");
        assert!(info.unshallow.is_empty(), "{name}");
        let scratch = init(Algorithm::Sha1, true);
        git_stdin(scratch.path(), &["index-pack", "--stdin"], &pack);
        let count = git(scratch.path(), &["count-objects", "-v"]);
        assert!(count.contains("in-pack: 6"), "{name}: {count}");

        let (info, _) = fetch(t.as_mut(), vec![tip], vec![ids[3]], depth(1, true));
        assert_eq!(info.shallow, vec![ids[2]], "{name}");
        assert_eq!(info.unshallow, vec![ids[3]], "{name}");

        let (info, _) = fetch(
            t.as_mut(),
            vec![tip],
            vec![ids[3]],
            depth(0x7fff_ffff, false),
        );
        assert!(info.shallow.is_empty(), "{name}");
        assert_eq!(info.unshallow, vec![ids[3]], "{name}");
    }
}

#[test]
fn since_and_exclude_cut_by_date_and_revision() {
    let (repo, ids) = linear(4);
    git(repo.path(), &["tag", "base", &ids[1].to_hex()]);
    for (name, mut t) in shapes(repo.path()) {
        let not = Deepen {
            not: vec!["refs/tags/base".into()],
            ..Deepen::default()
        };
        let (info, _) = fetch(t.as_mut(), vec![ids[3]], vec![], not);
        assert_eq!(info.shallow, vec![ids[2]], "{name}");
    }
}

#[test]
fn a_repeated_boundary_is_not_reported_again() {
    let (repo, ids) = linear(3);
    for (name, mut t) in shapes(repo.path()) {
        let (info, _) = fetch(t.as_mut(), vec![ids[2]], vec![ids[1]], depth(2, false));
        assert!(info.shallow.is_empty(), "{name}");
        assert!(info.unshallow.is_empty(), "{name}");
    }
}

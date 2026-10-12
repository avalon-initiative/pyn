mod common;

use std::collections::BTreeMap;

use common::{FIXTURES, copy_git_dir, expected, fixture_dir, id};
use pyn_git::{EntryKind, Error, Index, Ref, Repository};
use pyn_object::{Algorithm, Kind};

fn open(name: &str) -> Repository {
    Repository::open(&fixture_dir(name).join("dot-git")).unwrap()
}

#[test]
fn detects_object_format() {
    for f in &FIXTURES {
        assert_eq!(open(f.name).algorithm(), f.algorithm, "{}", f.name);
    }
}

#[test]
fn reads_every_object_with_kind_and_size() {
    for f in &FIXTURES {
        let db = open(f.name).objects().unwrap();
        let rows = expected(f.name, "objects.txt");
        assert!(rows.len() > 20);
        for row in rows {
            let want = id(f.algorithm, &row[0]);
            let object = db.read(&want).unwrap();
            assert_eq!(object.kind.name(), row[1], "{} {want}", f.name);
            assert_eq!(object.data.len().to_string(), row[2], "{} {want}", f.name);
            assert!(db.contains(&want).unwrap());
        }
    }
}

#[test]
fn packed_fixtures_use_the_expected_delta_kinds() {
    let count = |name: &str| {
        let repo = open(name);
        let db = repo.objects().unwrap();
        let mut kinds = BTreeMap::new();
        let packs = db.packs();
        assert_eq!(packs.len(), 1, "{name}");
        for pack in packs {
            pack.verify().unwrap();
            for oid in pack.ids() {
                let k = match pack.entry_kind(&oid).unwrap().unwrap() {
                    EntryKind::Base(_) => "base",
                    EntryKind::OfsDelta => "ofs",
                    EntryKind::RefDelta => "ref",
                };
                *kinds.entry(k).or_insert(0) += 1;
            }
        }
        kinds
    };
    for name in ["sha1-packed", "sha256-packed"] {
        let k = count(name);
        assert!(k.get("ofs").copied().unwrap_or(0) > 0, "{name}: {k:?}");
        assert!(!k.contains_key("ref"), "{name}: {k:?}");
    }
    let k = count("sha1-refdelta");
    assert!(k.get("ref").copied().unwrap_or(0) > 0, "{k:?}");
    assert!(!k.contains_key("ofs"), "{k:?}");
}

#[test]
fn loose_fixtures_have_no_packs() {
    for name in ["sha1-loose", "sha256-loose"] {
        assert!(open(name).objects().unwrap().packs().is_empty());
    }
}

#[test]
fn refs_and_head_match_git() {
    for f in &FIXTURES {
        let repo = open(f.name);
        let refs = repo.refs();
        let mut want: Vec<(String, String)> = expected(f.name, "refs.txt")
            .into_iter()
            .map(|r| (r[1].clone(), r[0].clone()))
            .collect();
        want.sort();
        let got: Vec<(String, String)> = refs
            .list("refs/")
            .unwrap()
            .into_iter()
            .map(|(n, i)| (n, i.to_hex()))
            .collect();
        assert_eq!(got, want, "{}", f.name);

        let head = expected(f.name, "head.txt").remove(0).remove(0);
        match refs.head().unwrap() {
            Ref::Symbolic(target) => assert_eq!(target, head, "{}", f.name),
            Ref::Direct(oid) => assert_eq!(oid.to_hex(), head, "{}", f.name),
        }
        let tip = refs.resolve("HEAD").unwrap().unwrap();
        assert_eq!(repo.objects().unwrap().read(&tip).unwrap().kind, Kind::Commit);
    }
}

#[test]
fn detached_head_and_packed_refs_with_peeled_tags() {
    assert!(matches!(open("sha1-refdelta").refs().head().unwrap(), Ref::Direct(_)));
    for name in ["sha1-packed", "sha256-packed"] {
        let packed = open(name).refs().packed().unwrap();
        assert_eq!(packed.len(), 4, "{name}");
        let tag = packed.iter().find(|p| p.name == "refs/tags/v1.0").unwrap();
        let main = packed.iter().find(|p| p.name == "refs/heads/main").unwrap();
        assert_eq!(tag.peeled, Some(main.id));
        assert!(main.peeled.is_none());
    }
}

#[test]
fn unborn_and_missing_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-loose", tmp.path(), "r/.git");
    std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/nothing\n").unwrap();
    let repo = Repository::open(&tmp.path().join("r")).unwrap();
    let refs = repo.refs();
    assert_eq!(refs.head().unwrap(), Ref::Symbolic("refs/heads/nothing".into()));
    assert_eq!(refs.resolve("HEAD").unwrap(), None);
    assert!(refs.read("refs/../x").is_err());
}

#[test]
fn index_matches_git_ls_files() {
    for f in &FIXTURES {
        let repo = open(f.name);
        let index = repo.read_index().unwrap().unwrap();
        assert_eq!(index.version, f.index_version, "{}", f.name);
        let want = expected(f.name, "index.txt");
        assert!(want.len() >= 5);
        assert_eq!(index.entries.len(), want.len());
        for (e, row) in index.entries.iter().zip(&want) {
            assert_eq!(format!("{:o}", e.mode), row[0], "{}", f.name);
            assert_eq!(e.id.to_hex(), row[1]);
            assert_eq!(e.stage.to_string(), row[2]);
            assert_eq!(String::from_utf8_lossy(&e.path), row[3]);
        }
    }
}

#[test]
fn index_reencodes_in_every_version() {
    for f in &FIXTURES {
        let index = open(f.name).read_index().unwrap().unwrap();
        for version in [2, 3, 4] {
            let mut copy = index.clone();
            copy.version = version;
            let bytes = copy.encode(f.algorithm).unwrap();
            assert_eq!(Index::parse(&bytes, f.algorithm).unwrap(), copy, "{} v{version}", f.name);
        }
    }
}

#[test]
fn index_blobs_exist_in_the_object_store() {
    for f in &FIXTURES {
        let repo = open(f.name);
        let db = repo.objects().unwrap();
        for e in repo.read_index().unwrap().unwrap().entries {
            assert_eq!(db.read(&e.id).unwrap().kind, Kind::Blob);
        }
    }
}

#[test]
fn linked_worktree_through_a_git_file() {
    let tmp = tempfile::tempdir().unwrap();
    let main = copy_git_dir("sha1-loose", tmp.path(), "main/.git");
    let wt = tmp.path().join("wt1");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: ../main/.git/worktrees/wt1\n").unwrap();

    let repo = Repository::open(&wt).unwrap();
    assert_eq!(repo.git_dir(), main.join("worktrees/wt1").canonicalize().unwrap());
    assert_eq!(repo.common_dir(), main.canonicalize().unwrap());
    assert_eq!(repo.work_tree(), Some(wt.as_path()));
    assert_eq!(repo.refs().head().unwrap(), Ref::Symbolic("refs/heads/wt-branch".into()));
    let tip = repo.refs().resolve("HEAD").unwrap().unwrap();
    assert_eq!(repo.refs().resolve("refs/heads/side").unwrap(), Some(tip));
    assert_eq!(repo.objects().unwrap().read(&tip).unwrap().kind, Kind::Commit);
    let wt_index = repo.read_index().unwrap().unwrap();
    assert!(!wt_index.entries.is_empty());

    let from_main = Repository::open(&tmp.path().join("main")).unwrap();
    assert_eq!(from_main.refs().head().unwrap(), Ref::Symbolic("refs/heads/main".into()));
    assert_ne!(from_main.refs().resolve("HEAD").unwrap().unwrap(), tip);

    std::fs::create_dir_all(wt.join("nested/deeper")).unwrap();
    let found = Repository::discover(&wt.join("nested/deeper")).unwrap();
    assert_eq!(found.git_dir(), repo.git_dir());
}

#[test]
fn submodule_style_git_file_with_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let modules = copy_git_dir("sha256-packed", tmp.path(), "super/.git/modules/sub");
    let sub = tmp.path().join("super/sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join(".git"), format!("gitdir: {}\n", modules.display())).unwrap();
    let repo = Repository::open(&sub).unwrap();
    assert_eq!(repo.git_dir(), modules.canonicalize().unwrap());
    assert_eq!(repo.common_dir(), modules.canonicalize().unwrap());
    assert_eq!(repo.algorithm(), Algorithm::Sha256);
    assert_eq!(repo.work_tree(), Some(sub.as_path()));
    assert!(repo.refs().resolve("HEAD").unwrap().is_some());
}

#[test]
fn bad_git_files_fail_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    for (i, body) in ["nonsense\n", "gitdir:\n", "gitdir: ../missing\n"].iter().enumerate() {
        let d = tmp.path().join(format!("w{i}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(".git"), body).unwrap();
        assert!(Repository::open(&d).is_err(), "{body:?}");
    }
    assert!(Repository::open(tmp.path()).is_err());
}

#[test]
fn unsupported_configurations_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-loose", tmp.path(), "a/.git");
    let config = std::fs::read_to_string(git_dir.join("config")).unwrap();
    std::fs::write(git_dir.join("config"), format!("{config}[extensions]\n\trefstorage = reftable\n")).unwrap();
    assert!(matches!(Repository::open(&tmp.path().join("a")), Err(Error::Unsupported(_))));
    std::fs::write(git_dir.join("config"), format!("{config}[extensions]\n\tobjectformat = md5\n")).unwrap();
    assert!(matches!(Repository::open(&tmp.path().join("a")), Err(Error::Unsupported(_))));
}

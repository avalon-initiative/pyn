//! The real abbreviation policy and object database, with the database's own operation counters.

use std::path::{Path, PathBuf};

use pyn_git::counters;
use pyn_gitwork::Repo;
use pyn_object::{Algorithm, ObjectId};

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

fn fixture(algorithm: Algorithm, layout: &str, root: &Path) -> (Repo, ObjectId, usize) {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pyn-git/tests/fixtures")
        .join(format!("{}-packed/dot-git/objects", algorithm.name()));
    let id = pyn_git::ObjectDb::open(&fixture, algorithm)
        .unwrap()
        .packs()[0]
        .ids()
        .next()
        .unwrap();
    let git = root.join(".git");
    let local = git.join("objects");
    for dir in ["info", "pack", "../refs/heads"] {
        std::fs::create_dir_all(local.join(dir)).unwrap();
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let mut config = "[core]\n\trepositoryformatversion = 0\n".to_owned();
    if algorithm == Algorithm::Sha256 {
        config = config.replace("= 0", "= 1") + "[extensions]\n\tobjectformat = sha256\n";
    }
    std::fs::write(git.join("config"), config).unwrap();
    let alternate = layout.starts_with("alternate");
    let store = if alternate {
        let store = root.join("alternate");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(
            local.join("info/alternates"),
            format!("{}\n", store.display()),
        )
        .unwrap();
        store
    } else {
        local
    };
    if layout.ends_with("packed") {
        copy_tree(&fixture, &store);
    } else {
        let hex = id.to_hex();
        std::fs::create_dir_all(store.join(&hex[..2])).unwrap();
        std::fs::write(store.join(&hex[..2]).join(&hex[2..]), b"").unwrap();
    }
    (
        Repo::open(root, None, None).unwrap(),
        id,
        if alternate { 2 } else { 1 },
    )
}

#[test]
fn every_render_uses_lazy_lookup_and_cached_default() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        for layout in ["loose", "packed", "alternate-loose", "alternate-packed"] {
            let tmp = tempfile::tempdir().unwrap();
            let (repo, id, stores) = fixture(algorithm, layout, tmp.path());
            counters::take();
            let context = repo.abbreviations(None).unwrap();
            let created = counters::take();
            assert_eq!(created.id_scans, 0, "{layout}: renderer enumerated ids");
            assert_eq!(
                created.abbreviation_lookups, 0,
                "{layout}: renderer looked up"
            );
            assert_eq!(created.object_counts, 1, "{layout}: default computed once");
            assert_eq!(created.config_scans, 1, "{layout}: configuration read once");
            let mut bytes = id.as_bytes().to_vec();
            bytes[algorithm.digest_len() - 1] ^= 1;
            let absent = ObjectId::from_bytes(algorithm, &bytes).unwrap();
            for _ in 0..3 {
                assert_eq!(context.short(&id), &id.to_hex()[..7], "{layout}: hit");
                assert_eq!(
                    context.short_hex(&absent.to_hex()),
                    absent.to_hex(),
                    "{layout}: miss must extend to full width"
                );
                assert_eq!(context.short_null(), "0000000", "{layout}: null miss");
                assert_eq!(
                    repo.abbreviate(&id).unwrap(),
                    &id.to_hex()[..7],
                    "{layout}: one-id renderer"
                );
                assert_eq!(
                    repo.abbreviations(Some(4)).unwrap().short(&absent),
                    absent.to_hex(),
                    "{layout}: explicit renderer"
                );
            }
            let used = counters::take();
            assert_eq!(used.id_scans, 0, "{layout}: rendering enumerated ids");
            assert_eq!(
                used.pack_enumerations, 0,
                "{layout}: rendering enumerated a pack"
            );
            assert_eq!(
                used.pack_opens, 0,
                "{layout}: rendering reopened a pack index"
            );
            assert_eq!(
                used.pack_reads, 0,
                "{layout}: rendering read an object body"
            );
            assert_eq!(
                used.abbreviation_lookups, 15,
                "{layout}: every path queries the database"
            );
            assert_eq!(used.object_counts, 0, "{layout}: default recomputed");
            assert_eq!(used.config_scans, 0, "{layout}: configuration reread");
            assert!(
                used.loose_fanout_reads <= stores * 15,
                "{layout}: at most one fan-out read per store per lookup, got {}",
                used.loose_fanout_reads
            );
        }
    }
}

#[test]
fn core_abbrev_above_the_hash_width_renders_full_ids() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let full = algorithm.digest_len() * 2;
        for value in ["999", "1k", "64", "128", "0x7fffffff", "2147483647"] {
            let tmp = tempfile::tempdir().unwrap();
            let (_, id, _) = fixture(algorithm, "packed", tmp.path());
            let config = tmp.path().join(".git/config");
            let mut text = std::fs::read_to_string(&config).unwrap();
            text.push_str(&format!("[core]\n\tabbrev = {value}\n"));
            std::fs::write(&config, text).unwrap();
            let repo = Repo::open(tmp.path(), None, None).unwrap();
            assert_eq!(repo.abbreviation_len(None).unwrap(), full, "{value}");
            assert_eq!(repo.abbreviate(&id).unwrap(), id.to_hex(), "{value}");
        }
    }
}

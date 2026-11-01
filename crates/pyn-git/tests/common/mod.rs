#![allow(dead_code)]

pub mod git;

use std::fs;
use std::path::{Path, PathBuf};

use pyn_object::{Algorithm, ObjectId};

pub struct Fixture {
    pub name: &'static str,
    pub algorithm: Algorithm,
    pub index_version: u32,
}

pub const FIXTURES: [Fixture; 5] = [
    Fixture {
        name: "sha1-loose",
        algorithm: Algorithm::Sha1,
        index_version: 2,
    },
    Fixture {
        name: "sha1-packed",
        algorithm: Algorithm::Sha1,
        index_version: 4,
    },
    Fixture {
        name: "sha1-refdelta",
        algorithm: Algorithm::Sha1,
        index_version: 2,
    },
    Fixture {
        name: "sha256-loose",
        algorithm: Algorithm::Sha256,
        index_version: 4,
    },
    Fixture {
        name: "sha256-packed",
        algorithm: Algorithm::Sha256,
        index_version: 2,
    },
];

pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

pub fn expected(name: &str, file: &str) -> Vec<Vec<String>> {
    let text = fs::read_to_string(fixture_dir(name).join("expected").join(file)).unwrap();
    text.lines()
        .map(|l| l.split([' ', '\t']).map(str::to_owned).collect())
        .collect()
}

pub fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            make_writable(&target);
        }
    }
}

/// Copies a fixture's `dot-git` to `<tmp>/<dest>` and returns that path.
pub fn copy_git_dir(name: &str, tmp: &Path, dest: &str) -> PathBuf {
    let to = tmp.join(dest);
    copy_dir(&fixture_dir(name).join("dot-git"), &to);
    to
}

pub fn id(algorithm: Algorithm, hex: &str) -> ObjectId {
    ObjectId::from_hex(algorithm, hex).unwrap()
}

pub fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}

/// A small index-only pack with occupied first/last buckets and a null-adjacent collision.
pub fn write_boundary_pack(objects: &Path, algorithm: Algorithm) -> Vec<ObjectId> {
    let mut ids = Vec::new();
    for first in [0, 1, 0x40, 0x7f, 0x80, 0xc0, 0xfe, 0xff] {
        for fill in [0, 255] {
            let mut bytes = vec![fill; algorithm.digest_len()];
            bytes[0] = first;
            bytes[algorithm.digest_len() - 1] = if fill == 0 { 1 } else { 254 };
            ids.push(ObjectId::from_bytes(algorithm, &bytes).unwrap());
        }
    }
    let mut second = vec![0; algorithm.digest_len()];
    second[algorithm.digest_len() - 1] = 2;
    ids.push(ObjectId::from_bytes(algorithm, &second).unwrap());
    let mut bytes = Vec::new();
    let mut writer = pyn_git::PackWriter::new(&mut bytes, algorithm, ids.len() as u32).unwrap();
    // Controlled index ids avoid hash-preimage searches; these tests never read object bodies.
    for id in &ids {
        writer
            .add(
                *id,
                pyn_git::PackEntry::Whole {
                    kind: pyn_object::Kind::Blob,
                    data: b"index boundary fixture",
                },
            )
            .unwrap();
    }
    let summary = writer.finish().unwrap();
    let dir = objects.join("pack");
    fs::create_dir_all(&dir).unwrap();
    let name = format!("pack-{}", summary.checksum.to_hex());
    fs::write(dir.join(format!("{name}.pack")), bytes).unwrap();
    fs::write(dir.join(format!("{name}.idx")), summary.index()).unwrap();
    ids
}

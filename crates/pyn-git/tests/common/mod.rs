#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use pyn_object::{Algorithm, ObjectId};

pub struct Fixture {
    pub name: &'static str,
    pub algorithm: Algorithm,
    pub index_version: u32,
}

pub const FIXTURES: [Fixture; 5] = [
    Fixture { name: "sha1-loose", algorithm: Algorithm::Sha1, index_version: 2 },
    Fixture { name: "sha1-packed", algorithm: Algorithm::Sha1, index_version: 4 },
    Fixture { name: "sha1-refdelta", algorithm: Algorithm::Sha1, index_version: 2 },
    Fixture { name: "sha256-loose", algorithm: Algorithm::Sha256, index_version: 4 },
    Fixture { name: "sha256-packed", algorithm: Algorithm::Sha256, index_version: 2 },
];

pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
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
            let mut perms = fs::metadata(&target).unwrap().permissions();
            perms.set_readonly(false);
            fs::set_permissions(&target, perms).unwrap();
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

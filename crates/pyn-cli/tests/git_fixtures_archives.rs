//! Archived git fixtures retain the oracle bytes, repository state and worktree metadata without running git.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::gitfix::*;
use pyn_git::{Index, Repository};
use pyn_object::Algorithm;

const CLEANUP_PATH_ENV: &str = "PYN_GIT_FIXTURE_CLEANUP_PATH";

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/git")
}

fn archive_names() -> Vec<&'static str> {
    ARCHIVES
        .lines()
        .map(|line| line.split_whitespace().next().unwrap())
        .collect()
}

/// Read the recorded oracle directly, independently of the fixture extractor.
fn oracle(format: &str) -> BTreeMap<String, Vec<u8>> {
    let file = fs::File::open(source().join(format!("{format}.tar.zst"))).unwrap();
    let mut archive = tar::Archive::new(zstd::Decoder::new(file).unwrap());
    let mut out = BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().to_string_lossy().into_owned();
        if entry.header().entry_type().is_file() && path.contains("/expected/") {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            out.insert(path, bytes);
        }
    }
    assert!(!out.is_empty());
    out
}

#[test]
fn tracked_fixtures_contain_archives_and_no_raw_payload_trees() {
    let mut archives = Vec::new();
    for entry in fs::read_dir(source()).unwrap() {
        let entry = entry.unwrap();
        assert!(!entry.file_type().unwrap().is_dir(), "{entry:?}");
        let name = entry.file_name().into_string().unwrap();
        if name.ends_with(".tar.zst") {
            archives.push(name);
        }
    }
    archives.sort();
    let mut names = archive_names();
    names.sort();
    assert_eq!(archives, names);
    assert_eq!(names.len(), 3);
}

#[test]
fn both_formats_load_main_linked_and_merge_repositories_from_archives() {
    for format in FORMATS {
        let recorded = oracle(format);
        let main = Repo::new(format);
        let merge = Repo::merge(format);
        for (repo, root, fixture, scenario, link) in [
            (&main, &main.main, "main", "out-branch-list", "README.md"),
            (
                &main,
                &main.linked,
                "main",
                "out-branch-list-linked",
                "README.md",
            ),
            (&merge, &merge.main, "merge", "merge-up-to-date", "a.txt"),
        ] {
            let prefix = if fixture == "merge" {
                format!("merge/{format}/expected/writes/{scenario}")
            } else {
                format!("{format}/expected/writes/{scenario}")
            };
            let want = |file: &str| recorded[&format!("{prefix}/{file}")].as_slice();
            assert_eq!(tree_dump(root).as_bytes(), want("tree.txt"));
            assert_eq!(fs::read_link(root.join("link")).unwrap(), Path::new(link));
            assert_eq!(
                fs::metadata(root.join("run.sh"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0o111
            );
            assert!(!root.join("dot-gitignore").exists());
            assert!(!root.join("src/dot-gitignore").exists());
            assert_eq!(root.join(".gitignore").is_file(), fixture == "main");
            assert_eq!(root.join("src/.gitignore").is_file(), fixture == "main");

            let git = Repository::open(root).unwrap();
            let gitdir = pyn_git::resolve_git_dir(&root.join(".git")).unwrap();
            let index =
                Index::read(&gitdir.join("index"), Algorithm::from_name(format).unwrap()).unwrap();
            let listing: String = index
                .entries
                .iter()
                .map(|entry| {
                    format!(
                        "{:06o} {} {}\t{}\n",
                        entry.mode,
                        entry.id,
                        entry.stage,
                        String::from_utf8_lossy(&entry.path)
                    )
                })
                .collect();
            assert_eq!(listing.as_bytes(), want("ls-files.txt"));
            let mut refs: String = git
                .refs()
                .list("refs/")
                .unwrap()
                .iter()
                .map(|(name, id)| format!("{name} {id}\n"))
                .collect();
            refs.push_str(&format!(
                "HEAD {}",
                fs::read_to_string(gitdir.join("HEAD")).unwrap()
            ));
            assert_eq!(refs.as_bytes(), want("refs.txt"));
            assert_eq!(
                repo.expected_writes(fixture, scenario, "tree.txt")
                    .as_bytes(),
                want("tree.txt")
            );
        }
        assert_eq!(
            fs::read_to_string(main.linked.join(".git")).unwrap(),
            format!(
                "gitdir: {}\n",
                main.main.join(".git/worktrees/linked").display()
            )
        );
        assert_eq!(
            fs::read_to_string(main.main.join(".git/worktrees/linked/gitdir")).unwrap(),
            format!("{}\n", main.linked.join(".git").display())
        );
        for (root, readme) in [
            (&main.main, "README-worktree.md"),
            (&main.linked, "README-linked.md"),
        ] {
            assert_eq!(
                fs::read(root.join("README.md")).unwrap(),
                fs::read(source().join(readme)).unwrap()
            );
        }
        for (path, bytes) in &recorded {
            if let Some(name) = path.strip_prefix(&format!("{format}/expected/")) {
                assert_eq!(main.expected(name).as_bytes(), bytes, "{path}");
            } else {
                let name = path
                    .strip_prefix(&format!("merge/{format}/expected/writes/"))
                    .unwrap();
                let (scenario, file) = name.split_once('/').unwrap();
                assert_eq!(
                    merge.expected_writes("merge", scenario, file).as_bytes(),
                    bytes,
                    "{path}"
                );
            }
        }
    }
}

#[test]
fn parallel_readers_share_one_private_extraction() {
    let threads: Vec<_> = (0..8).map(|_| std::thread::spawn(fixtures)).collect();
    let roots: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    let root = &roots[0];
    assert!(!root.starts_with(source()));
    assert!(root.is_dir());
    assert!(roots.iter().all(|path| path == root));
    let before = snapshot(root);
    let repo = Repo::new("sha1");
    fs::write(repo.main.join("README.md"), "changed\n").unwrap();
    assert_eq!(fixtures(), *root);
    assert_eq!(snapshot(root), before);
}

#[test]
fn extracted_fixtures_are_removed_when_the_process_exits() {
    let parent_root = fixtures();
    let tmp = tempfile::tempdir().unwrap();
    let path_file = tmp.path().join("extraction-path");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fixture_cleanup_child"])
        .env(CLEANUP_PATH_ENV, &path_file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let path = fs::read(path_file).unwrap();
    assert!(!path.is_empty());
    let child_root = PathBuf::from(OsString::from_vec(path));
    assert_ne!(child_root, parent_root);
    assert_eq!(
        fs::symlink_metadata(&child_root).unwrap_err().kind(),
        io::ErrorKind::NotFound,
        "{} should have been removed at child exit",
        child_root.display()
    );
    assert!(parent_root.join("worktree").is_dir());
}

#[test]
fn fixture_cleanup_child() {
    let Some(path_file) = std::env::var_os(CLEANUP_PATH_ENV) else {
        return;
    };
    let root = std::thread::spawn(fixtures).join().unwrap();
    assert_eq!(fixtures(), root);
    assert!(root.join("worktree").is_dir());
    fs::write(path_file, root.as_os_str().as_bytes()).unwrap();
}

#[test]
fn every_archive_is_required() {
    for missing in archive_names() {
        let source_dir = tempfile::tempdir().unwrap();
        for name in archive_names() {
            if name != missing {
                fs::copy(source().join(name), source_dir.path().join(name)).unwrap();
            }
        }
        let error = extract_archives(source_dir.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound, "{missing}: {error}");
        assert!(!source_dir.path().join("worktree").exists());
    }
}

#[test]
fn invalid_compression_and_invalid_tar_are_refused() {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_mode(0o644);
    header.set_mtime(0);
    builder
        .append_data(&mut header, "partial.txt", &b"x"[..])
        .unwrap();
    // Keep the valid member before the builder adds end-of-archive blocks.
    let mut partial = builder.get_ref().clone();
    partial.extend_from_slice(b"truncated next header");
    let corruptions = [
        ("invalid zstd", b"invalid zstd".to_vec()),
        (
            "invalid tar",
            zstd::encode_all(&b"invalid tar"[..], 0).unwrap(),
        ),
        ("partial tar", zstd::encode_all(&partial[..], 0).unwrap()),
    ];
    for selected in archive_names() {
        for (kind, bytes) in &corruptions {
            let source_dir = tempfile::tempdir().unwrap();
            for name in archive_names() {
                fs::copy(source().join(name), source_dir.path().join(name)).unwrap();
            }
            fs::write(source_dir.path().join(selected), bytes).unwrap();
            assert!(
                extract_archives(source_dir.path()).is_err(),
                "{selected}: {kind}"
            );
            assert!(
                !source_dir.path().join("worktree").exists(),
                "{selected}: {kind}"
            );
        }
    }
}

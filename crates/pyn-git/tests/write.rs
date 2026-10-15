mod common;

use std::fs;
use std::process::Command;

use common::copy_git_dir;
use pyn_git::{Index, IndexEntry, Ref, Repository};
use pyn_object::{
    Algorithm, Commit, FileMode, Kind, ObjectId, Signature, TreeEntry, encode_tree, hash_object,
};

fn signature() -> Signature {
    Signature {
        name: b"Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        time: 1_700_000_000,
        offset_minutes: 120,
    }
}

/// Writes blob, tree and commit through the library and points `refs/heads/main` at the commit.
fn write_history(repo: &Repository) -> (ObjectId, ObjectId, ObjectId) {
    let db = repo.objects().unwrap();
    let blob = db.write(Kind::Blob, b"hello\n").unwrap();
    let tree_bytes = encode_tree(
        repo.algorithm(),
        &[TreeEntry {
            mode: FileMode::Regular,
            name: b"hello.txt".to_vec(),
            id: blob,
        }],
    )
    .unwrap();
    let tree = db.write(Kind::Tree, &tree_bytes).unwrap();
    let commit = Commit {
        tree,
        parents: vec![],
        author: signature(),
        committer: signature(),
        extra_headers: vec![],
        message: b"initial\n".to_vec(),
    };
    let commit_id = db
        .write(Kind::Commit, &commit.encode(repo.algorithm()).unwrap())
        .unwrap();
    repo.refs().update("refs/heads/main", &commit_id).unwrap();
    (blob, tree, commit_id)
}

fn empty_repo(tmp: &std::path::Path, algorithm: Algorithm) -> Repository {
    let root = tmp.join("r");
    let git_dir = root.join(".git");
    for d in ["objects", "refs/heads", "refs/tags"] {
        fs::create_dir_all(git_dir.join(d)).unwrap();
    }
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let mut config = "[core]\n\trepositoryformatversion = 0\n\tbare = false\n".to_owned();
    if algorithm == Algorithm::Sha256 {
        config = config.replace("= 0", "= 1") + "[extensions]\n\tobjectformat = sha256\n";
    }
    fs::write(git_dir.join("config"), config).unwrap();
    Repository::open(&root).unwrap()
}

#[test]
fn written_objects_round_trip_in_both_formats() {
    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let tmp = tempfile::tempdir().unwrap();
        let repo = empty_repo(tmp.path(), algorithm);
        assert_eq!(repo.algorithm(), algorithm);
        let (blob, tree, commit) = write_history(&repo);
        let db = repo.objects().unwrap();
        assert_eq!(
            blob,
            hash_object(algorithm, Kind::Blob, b"hello\n").unwrap()
        );
        assert_eq!(db.read(&blob).unwrap().data, b"hello\n");
        assert_eq!(db.read(&tree).unwrap().kind, Kind::Tree);
        assert_eq!(repo.refs().resolve("HEAD").unwrap(), Some(commit));
        assert_eq!(
            repo.refs().list("refs/heads/").unwrap(),
            vec![("refs/heads/main".to_owned(), commit)]
        );
    }
}

#[test]
fn writing_is_idempotent_and_read_only_on_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = empty_repo(tmp.path(), Algorithm::Sha1);
    let db = repo.objects().unwrap();
    let a = db.write(Kind::Blob, b"same").unwrap();
    let b = db.write(Kind::Blob, b"same").unwrap();
    assert_eq!(a, b);
    let hex = a.to_hex();
    let path = repo
        .common_dir()
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert!(fs::metadata(&path).unwrap().permissions().readonly());
    let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("tmp_obj")
        })
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn empty_blob_and_large_blob() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = empty_repo(tmp.path(), Algorithm::Sha1);
    let db = repo.objects().unwrap();
    let empty = db.write(Kind::Blob, b"").unwrap();
    assert_eq!(empty.to_hex(), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    assert!(db.read(&empty).unwrap().data.is_empty());
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let id = db.write(Kind::Blob, &big).unwrap();
    assert_eq!(db.read(&id).unwrap().data, big);
}

#[test]
fn ref_updates_use_a_lock_and_refuse_a_held_one() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = empty_repo(tmp.path(), Algorithm::Sha1);
    let (_, _, commit) = write_history(&repo);
    let refs = repo.refs();
    refs.update("refs/heads/feature/x", &commit).unwrap();
    assert_eq!(refs.resolve("refs/heads/feature/x").unwrap(), Some(commit));
    refs.set_symbolic("HEAD", "refs/heads/feature/x").unwrap();
    assert_eq!(
        refs.head().unwrap(),
        Ref::Symbolic("refs/heads/feature/x".into())
    );

    fs::write(repo.git_dir().join("refs/heads/main.lock"), "").unwrap();
    assert!(refs.update("refs/heads/main", &commit).is_err());
    assert!(refs.update("refs/heads/../escape", &commit).is_err());
    let foreign = hash_object(Algorithm::Sha256, Kind::Blob, b"x").unwrap();
    assert!(refs.update("refs/heads/y", &foreign).is_err());
}

#[test]
fn index_with_paths_across_padding_boundaries_round_trips() {
    let mut paths: Vec<Vec<u8>> = (1..=24).map(|n| vec![b'a' + (n % 26) as u8; n]).collect();
    paths.push(b"dir/sub/file-one.txt".to_vec());
    paths.push(b"dir/sub/file-two.txt".to_vec());
    paths.push(b"dir/other.txt".to_vec());
    paths.push(format!("long/{}", "p".repeat(5000)).into_bytes());
    paths.push("unicode/\u{e9}\u{4e2d}.txt".as_bytes().to_vec());

    for algorithm in [Algorithm::Sha1, Algorithm::Sha256] {
        let id = hash_object(algorithm, Kind::Blob, b"x").unwrap();
        for version in [2, 3, 4] {
            let mut index = Index::new(version);
            for (n, p) in paths.iter().enumerate() {
                let mut e = IndexEntry::new(p.clone(), 0o100644, id);
                e.size = n as u32;
                e.mtime.seconds = 1_700_000_000 + n as u32;
                e.mtime.nanos = 999;
                index.entries.push(e);
            }
            index.entries[0].stage = 2;
            index.entries[1].assume_valid = true;
            index.entries[2].skip_worktree = true;
            index.entries[3].intent_to_add = true;
            let bytes = index.encode(algorithm).unwrap();
            let back = Index::parse(&bytes, algorithm).unwrap();
            assert_eq!(back.version, if version == 2 { 3 } else { version });
            let mut want = index.entries.clone();
            want.sort_by(|a, b| (&a.path, a.stage).cmp(&(&b.path, b.stage)));
            assert_eq!(back.entries, want, "{algorithm:?} v{version}");
        }
    }
}

#[test]
fn index_rejects_duplicates_and_foreign_ids() {
    let sha1 = hash_object(Algorithm::Sha1, Kind::Blob, b"x").unwrap();
    let sha256 = hash_object(Algorithm::Sha256, Kind::Blob, b"x").unwrap();
    let mut index = Index::new(2);
    index.entries.push(IndexEntry::new("a", 0o100644, sha1));
    index.entries.push(IndexEntry::new("a", 0o100644, sha1));
    assert!(index.encode(Algorithm::Sha1).is_err());
    index.entries.pop();
    assert!(index.encode(Algorithm::Sha256).is_err());
    index.entries[0].id = sha256;
    assert!(index.encode(Algorithm::Sha1).is_err());
    assert!(Index::new(5).encode(Algorithm::Sha1).is_err());
}

#[test]
fn written_index_is_read_back_from_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let git_dir = copy_git_dir("sha1-packed", tmp.path(), "r/.git");
    let repo = Repository::open(&tmp.path().join("r")).unwrap();
    let before = repo.read_index().unwrap().unwrap();
    assert_eq!(before.version, 4);
    repo.write_index(&before).unwrap();
    assert_eq!(repo.read_index().unwrap().unwrap(), before);
    assert!(!git_dir.join("index.lock").exists());
}

/// Needs the git binary as an oracle; run with `cargo test -p pyn-git -- --ignored`.
#[test]
#[ignore = "requires the git binary; run with --ignored"]
fn git_fsck_accepts_objects_refs_and_index_written_here() {
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    for (algorithm, format, version) in [
        (Algorithm::Sha1, "sha1", 2),
        (Algorithm::Sha1, "sha1", 4),
        (Algorithm::Sha256, "sha256", 2),
        (Algorithm::Sha256, "sha256", 4),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        fs::create_dir_all(&root).unwrap();
        git(
            &root,
            &[
                "init",
                "-q",
                &format!("--object-format={format}"),
                "-b",
                "main",
                ".",
            ],
        );
        let repo = Repository::open(&root).unwrap();
        assert_eq!(repo.algorithm(), algorithm);
        let (blob, _, commit) = write_history(&repo);

        let mut index = Index::new(version);
        let mut entry = IndexEntry::new("hello.txt", 0o100644, blob);
        entry.size = 6;
        index.entries.push(entry);
        repo.write_index(&index).unwrap();

        git(&root, &["fsck", "--strict", "--no-dangling"]);
        assert_eq!(git(&root, &["rev-parse", "HEAD"]).trim(), commit.to_hex());
        assert_eq!(git(&root, &["cat-file", "-p", "HEAD:hello.txt"]), "hello\n");
        let staged = git(&root, &["ls-files", "-s"]);
        assert!(
            staged.contains(&blob.to_hex()) && staged.contains("hello.txt"),
            "{staged}"
        );
        assert!(
            git(&root, &["cat-file", "commit", "HEAD"])
                .contains("author Writer <writer@example.com> 1700000000 +0200")
        );
    }
}

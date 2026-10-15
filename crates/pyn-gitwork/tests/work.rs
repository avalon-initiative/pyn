//! Status, revisions, history and tree diffs on repositories built with the in-house writer.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use pyn_git::{Index, IndexEntry, Repository, Timestamp};
use pyn_gitwork::{Kind, Pathspec, Repo};
use pyn_object::{
    Algorithm, Commit, FileMode, Kind as ObjectKind, ObjectId, Signature, Tag, TreeEntry,
    encode_tree,
};

const ALGORITHMS: [Algorithm; 2] = [Algorithm::Sha1, Algorithm::Sha256];

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new(algorithm: Algorithm) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let git = root.join(".git");
        for d in ["objects", "refs/heads", "refs/tags"] {
            fs::create_dir_all(git.join(d)).unwrap();
        }
        fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let mut config = "[core]\n\trepositoryformatversion = 0\n".to_owned();
        if algorithm == Algorithm::Sha256 {
            config = config.replace("= 0", "= 1") + "[extensions]\n\tobjectformat = sha256\n";
        }
        fs::write(git.join("config"), config).unwrap();
        Self { _tmp: tmp, root }
    }

    fn git(&self) -> Repository {
        Repository::open(&self.root).unwrap()
    }

    fn repo(&self) -> Repo {
        Repo::open(&self.root, None, None).unwrap()
    }

    fn write(&self, kind: ObjectKind, bytes: &[u8]) -> ObjectId {
        self.git().objects().unwrap().write(kind, bytes).unwrap()
    }

    fn blob(&self, text: &str) -> ObjectId {
        self.write(ObjectKind::Blob, text.as_bytes())
    }

    /// A tree from `(path, mode, blob)` triples; folders are built as needed.
    fn tree(&self, files: &[(&str, FileMode, ObjectId)]) -> ObjectId {
        let mut entries: Vec<TreeEntry> = Vec::new();
        let mut folders: Vec<&str> = Vec::new();
        for (path, mode, id) in files {
            match path.split_once('/') {
                None => entries.push(TreeEntry {
                    mode: *mode,
                    name: path.as_bytes().to_vec(),
                    id: *id,
                }),
                Some((dir, _)) if !folders.contains(&dir) => folders.push(dir),
                Some(_) => {}
            }
        }
        for dir in folders {
            let prefix = format!("{dir}/");
            let inner: Vec<(&str, FileMode, ObjectId)> = files
                .iter()
                .filter_map(|(p, m, i)| p.strip_prefix(&prefix).map(|r| (r, *m, *i)))
                .collect();
            entries.push(TreeEntry {
                mode: FileMode::Tree,
                name: dir.as_bytes().to_vec(),
                id: self.tree(&inner),
            });
        }
        self.write(
            ObjectKind::Tree,
            &encode_tree(self.git().algorithm(), &entries).unwrap(),
        )
    }

    fn commit(&self, tree: ObjectId, parents: &[ObjectId], time: i64, message: &str) -> ObjectId {
        let who = |offset| Signature {
            name: b"T".to_vec(),
            email: b"t@example.com".to_vec(),
            time,
            offset_minutes: offset,
        };
        let commit = Commit {
            tree,
            parents: parents.to_vec(),
            author: who(60),
            committer: who(0),
            extra_headers: vec![],
            message: message.as_bytes().to_vec(),
        };
        self.write(
            ObjectKind::Commit,
            &commit.encode(self.git().algorithm()).unwrap(),
        )
    }

    fn point(&self, name: &str, id: &ObjectId) {
        self.git().refs().update(name, id).unwrap();
    }

    fn file(&self, path: &str, text: &str) {
        let p = self.root.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn index(&self, entries: Vec<IndexEntry>) {
        let mut index = Index::new(2);
        index.entries = entries;
        self.git().write_index(&index).unwrap();
    }

    /// An index entry for `path` as `git add` would have recorded it for `text` right now.
    fn entry(&self, path: &str, text: &str) -> IndexEntry {
        let meta = fs::metadata(self.root.join(path)).unwrap();
        let mut e = IndexEntry::new(path, 0o100644, self.blob(text));
        e.size = meta.len() as u32;
        e.mtime = Timestamp {
            seconds: meta.mtime() as u32,
            nanos: meta.mtime_nsec() as u32,
        };
        e
    }

    /// The paths with an unstaged state (a change to the work tree, or untracked) and that state.
    fn unstaged(&self) -> Vec<(String, Kind)> {
        self.status()
            .into_iter()
            .filter_map(|(path, _, unstaged)| unstaged.map(|k| (path, k)))
            .collect()
    }

    fn status(&self) -> Vec<(String, Option<Kind>, Option<Kind>)> {
        self.repo()
            .status()
            .unwrap()
            .entries
            .into_iter()
            .map(|e| (String::from_utf8(e.path).unwrap(), e.staged, e.unstaged))
            .collect()
    }
}

fn set_mtime(path: &Path, seconds: u64) {
    let f = fs::OpenOptions::new().write(true).open(path).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

#[test]
fn a_matching_stat_is_trusted_unless_the_entry_is_racy() {
    for algorithm in ALGORITHMS {
        let fx = Fx::new(algorithm);
        fx.file("a", "aaa");
        set_mtime(&fx.root.join("a"), 1_000_000);
        let mut entry = fx.entry("a", "bbb");
        entry.mtime = Timestamp {
            seconds: 1_000_000,
            nanos: 0,
        };
        fx.index(vec![entry.clone()]);
        set_mtime(&fx.root.join(".git/index"), 2_000_000);
        assert_eq!(
            fx.unstaged(),
            [],
            "the stat matches, so the content is not read"
        );

        set_mtime(&fx.root.join(".git/index"), 1_000_000);
        assert_eq!(
            fx.unstaged(),
            [("a".to_string(), Kind::Modified)],
            "an entry as new as the index is hashed"
        );

        entry.size = 4;
        fx.index(vec![entry]);
        set_mtime(&fx.root.join(".git/index"), 2_000_000);
        assert_eq!(fx.unstaged().len(), 1, "a different size is a change");
    }
}

#[test]
fn unchanged_content_with_a_new_stat_is_clean() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file("a", "same");
    let mut entry = fx.entry("a", "same");
    entry.mtime = Timestamp {
        seconds: 5,
        nanos: 5,
    };
    fx.index(vec![entry]);
    assert_eq!(fx.unstaged(), []);
}

#[test]
fn file_types_modes_and_flags_are_compared_as_git_does() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file("script", "#!/bin/sh\n");
    fx.file("was-file", "x");
    fx.file("hidden", "changed");
    fx.file("gone-dir/x", "x");
    std::os::unix::fs::symlink("elsewhere", fx.root.join("link")).unwrap();
    let mut was_file = fx.entry("was-file", "x");
    was_file.mode = 0o100644;
    fs::remove_file(fx.root.join("was-file")).unwrap();
    std::os::unix::fs::symlink("x", fx.root.join("was-file")).unwrap();
    let mut script = fx.entry("script", "#!/bin/sh\n");
    script.mode = 0o100755;
    let mut link = IndexEntry::new("link", 0o120000, fx.blob("elsewhere"));
    link.size = 9;
    let mut hidden = fx.entry("hidden", "original");
    hidden.assume_valid = true;
    let mut sparse = IndexEntry::new("not-checked-out", 0o100644, fx.blob("x"));
    sparse.skip_worktree = true;
    let mut intent = fx.entry("gone-dir/x", "");
    intent.intent_to_add = true;
    let missing = IndexEntry::new("missing", 0o100644, fx.blob("m"));
    fx.index(vec![
        script,
        was_file,
        link,
        hidden,
        sparse,
        intent,
        missing,
        IndexEntry::new("gone-dir", 0o100644, fx.blob("was a file")),
    ]);
    let got = fx.status();
    let find = |p: &str| got.iter().find(|r| r.0 == p).map(|r| (r.1, r.2));
    assert_eq!(
        find("script"),
        Some((Some(Kind::Added), Some(Kind::Modified))),
        "{got:?}"
    );
    assert_eq!(
        find("was-file"),
        Some((Some(Kind::Added), Some(Kind::TypeChange)))
    );
    assert_eq!(find("link"), Some((Some(Kind::Added), None)));
    assert_eq!(
        find("hidden"),
        Some((Some(Kind::Added), None)),
        "assume-valid is not checked"
    );
    assert_eq!(find("not-checked-out"), Some((Some(Kind::Added), None)));
    assert_eq!(
        find("gone-dir/x"),
        Some((None, Some(Kind::Added))),
        "intent to add"
    );
    assert_eq!(
        find("missing"),
        Some((Some(Kind::Added), Some(Kind::Deleted)))
    );
    assert_eq!(
        find("gone-dir"),
        Some((Some(Kind::Added), Some(Kind::Deleted))),
        "a folder replaced the file"
    );
}

#[test]
fn the_executable_bit_matters_only_with_core_filemode() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file("run", "x");
    fs::set_permissions(fx.root.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
    fx.index(vec![fx.entry("run", "x")]);
    assert_eq!(
        fx.status()
            .iter()
            .filter(|r| r.2 == Some(Kind::Modified))
            .count(),
        1
    );
    let config = fx.root.join(".git/config");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{text}\tfileMode = false\n")).unwrap();
    assert!(fx.status().iter().all(|r| r.2.is_none()));
}

#[test]
fn conflicted_paths_are_unmerged_and_not_staged() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file("c", "ours");
    let mut entries = Vec::new();
    for stage in 1..=3u8 {
        let mut e = IndexEntry::new("c", 0o100644, fx.blob(&format!("side {stage}")));
        e.stage = stage;
        entries.push(e);
    }
    fx.index(entries);
    assert_eq!(fx.status(), [("c".to_string(), None, Some(Kind::Unmerged))]);
}

#[test]
fn staged_changes_compare_the_index_with_head() {
    for algorithm in ALGORITHMS {
        let fx = Fx::new(algorithm);
        let (keep, edit, drop, link) = (fx.blob("k"), fx.blob("e1"), fx.blob("d"), fx.blob("t"));
        let tree = fx.tree(&[
            ("keep", FileMode::Regular, keep),
            ("dir/edit", FileMode::Regular, edit),
            ("drop", FileMode::Regular, drop),
            ("becomes-link", FileMode::Regular, link),
        ]);
        let commit = fx.commit(tree, &[], 1, "c\n");
        fx.point("refs/heads/main", &commit);
        for (p, t) in [
            ("keep", "k"),
            ("dir/edit", "e2"),
            ("added", "a"),
            ("becomes-link", "t"),
        ] {
            fx.file(p, t);
        }
        let mut link_entry = IndexEntry::new("becomes-link", 0o120000, fx.blob("t"));
        link_entry.size = 1;
        let mut entries = vec![
            fx.entry("keep", "k"),
            fx.entry("dir/edit", "e2"),
            fx.entry("added", "a"),
            link_entry,
        ];
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        fx.index(entries);
        let got: Vec<(String, Option<Kind>)> =
            fx.status().into_iter().map(|r| (r.0, r.1)).collect();
        assert_eq!(
            got,
            [
                ("added".into(), Some(Kind::Added)),
                ("becomes-link".into(), Some(Kind::TypeChange)),
                ("dir/edit".into(), Some(Kind::Modified)),
                ("drop".into(), Some(Kind::Deleted)),
            ]
        );
    }
}

#[test]
fn untracked_folders_collapse_and_nested_repositories_count() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file("tracked/a", "a");
    fx.index(vec![fx.entry("tracked/a", "a")]);
    fx.file("tracked/new", "n");
    fx.file("only/deep/er/f", "f");
    fx.file("ignored-only/x.log", "x");
    fx.file(".gitignore", "*.log\n");
    fs::create_dir_all(fx.root.join("empty/dir")).unwrap();
    fs::create_dir_all(fx.root.join("inner/.git")).unwrap();
    fx.file("tracked/sub/z", "z");
    let paths: Vec<String> = fx.unstaged().into_iter().map(|r| r.0).collect();
    assert_eq!(
        paths,
        [
            ".gitignore",
            "inner/",
            "only/",
            "tracked/new",
            "tracked/sub/"
        ]
    );
}

#[test]
fn ignored_folders_hide_tracked_siblings_only_from_the_untracked_list() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file(".gitignore", "build/\n");
    fx.file("build/kept", "k");
    fx.file("build/other", "o");
    let mut entries = vec![
        fx.entry(".gitignore", "build/\n"),
        fx.entry("build/kept", "k"),
    ];
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    fx.index(entries);
    fx.file("build/kept", "changed");
    let got = fx.unstaged();
    assert_eq!(got, [("build/kept".to_string(), Kind::Modified)]);
}

#[test]
fn core_ignorecase_folds_the_pattern_match() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file(".gitignore", "README\n");
    fx.file("readme", "x");
    assert!(fx.status().iter().any(|r| r.0 == "readme"));
    let config = fx.root.join(".git/config");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, format!("{text}\tignoreCase = true\n")).unwrap();
    assert!(!fx.status().iter().any(|r| r.0 == "readme"));
}

#[test]
fn a_symlinked_gitignore_is_not_followed() {
    let fx = Fx::new(Algorithm::Sha1);
    fx.file("elsewhere", "*.x\n");
    std::os::unix::fs::symlink("elsewhere", fx.root.join(".gitignore")).unwrap();
    fx.file("a.x", "x");
    let paths: Vec<String> = fx.status().into_iter().map(|r| r.0).collect();
    assert!(paths.contains(&"a.x".to_string()), "{paths:?}");
}

struct History {
    fx: Fx,
    ids: Vec<ObjectId>,
}

/// c1 adds `a`; c2 adds `b`; side (from c1) edits `a`; merge of c2 and side takes `a` from side.
fn history(algorithm: Algorithm) -> History {
    let fx = Fx::new(algorithm);
    let (a1, a2, b) = (fx.blob("a1"), fx.blob("a2"), fx.blob("b"));
    let t1 = fx.tree(&[("a", FileMode::Regular, a1)]);
    let t2 = fx.tree(&[("a", FileMode::Regular, a1), ("b", FileMode::Regular, b)]);
    let ts = fx.tree(&[("a", FileMode::Regular, a2)]);
    let tm = fx.tree(&[("a", FileMode::Regular, a2), ("b", FileMode::Regular, b)]);
    let c1 = fx.commit(t1, &[], 100, "one\n");
    let c2 = fx.commit(t2, &[c1], 200, "two\n");
    let side = fx.commit(ts, &[c1], 300, "side\n");
    let merge = fx.commit(tm, &[c2, side], 400, "merge\n");
    fx.point("refs/heads/main", &merge);
    fx.point("refs/heads/side", &side);
    fx.point("refs/tags/light", &c2);
    let tag = Tag {
        object: c1,
        kind: ObjectKind::Commit,
        name: b"v1".to_vec(),
        tagger: None,
        message: b"release\n".to_vec(),
    };
    let tag_id = fx.write(ObjectKind::Tag, &tag.encode(fx.git().algorithm()).unwrap());
    fx.point("refs/tags/v1", &tag_id);
    History {
        fx,
        ids: vec![c1, c2, side, merge],
    }
}

fn subjects(repo: &Repo, start: &[ObjectId], spec: Pathspec) -> Vec<String> {
    repo.log(start, spec)
        .unwrap()
        .map(|e| {
            String::from_utf8(e.unwrap().commit.message)
                .unwrap()
                .trim()
                .to_string()
        })
        .collect()
}

#[test]
fn revisions_resolve_like_git_ones() {
    for algorithm in ALGORITHMS {
        let h = history(algorithm);
        let repo = h.fx.repo();
        let [c1, c2, side, merge] = [h.ids[0], h.ids[1], h.ids[2], h.ids[3]];
        let at = |s: &str| repo.resolve(s).unwrap();
        assert_eq!(at("HEAD"), merge);
        assert_eq!(at("@"), merge);
        assert_eq!(at("main"), merge);
        assert_eq!(at("heads/main"), merge);
        assert_eq!(at("refs/heads/side"), side);
        assert_eq!(at("HEAD^"), c2);
        assert_eq!(at("HEAD^1"), c2);
        assert_eq!(at("HEAD^2"), side);
        assert_eq!(at("HEAD^0"), merge);
        assert_eq!(at("HEAD~1"), c2);
        assert_eq!(at("HEAD~2"), c1);
        assert_eq!(at("HEAD^2^"), c1);
        assert_eq!(at("HEAD^^"), c1);
        assert_eq!(at("light"), c2);
        assert_eq!(at("v1"), c1, "an annotated tag is followed to its commit");
        assert_eq!(at(&merge.to_hex()), merge);
        assert_eq!(at(&merge.to_hex()[..6]), merge);
        assert_eq!(at(&format!("{}~1", &merge.to_hex()[..8])), c2);
        for bad in [
            "nope",
            "HEAD^3",
            "HEAD~9",
            "",
            "^",
            "HEAD~x",
            "abc",
            "tags/nope",
        ] {
            assert!(repo.resolve(bad).is_err(), "{bad:?} should not resolve");
        }
    }
}

#[test]
fn a_ref_wins_over_an_abbreviation_and_a_shared_prefix_is_ambiguous() {
    let h = history(Algorithm::Sha1);
    let prefix = h.ids[3].to_hex()[..5].to_string();
    h.fx.point(&format!("refs/heads/{prefix}"), &h.ids[0]);
    assert_eq!(h.fx.repo().resolve(&prefix).unwrap(), h.ids[0]);

    let tree = h.fx.tree(&[]);
    let mut seen = std::collections::HashMap::new();
    let shared = (0..5000)
        .find_map(|i| {
            let id = h.fx.commit(tree, &[], i, &format!("c{i}\n"));
            seen.insert(id.to_hex()[..4].to_string(), id)
                .map(|_| id.to_hex()[..4].to_string())
        })
        .expect("two commits share four digits");
    let err = h.fx.repo().resolve(&shared).unwrap_err().to_string();
    assert!(err.contains("ambiguous"), "{err}");
}

#[test]
fn log_walks_by_commit_time_across_parents() {
    for algorithm in ALGORITHMS {
        let h = history(algorithm);
        let repo = h.fx.repo();
        let all = subjects(&repo, &[h.ids[3]], Pathspec::default());
        assert_eq!(all, ["merge", "side", "two", "one"]);
        let side_only = subjects(&repo, &[h.ids[2]], Pathspec::default());
        assert_eq!(side_only, ["side", "one"]);
    }
}

#[test]
fn a_path_limit_simplifies_history_as_git_does() {
    for algorithm in ALGORITHMS {
        let h = history(algorithm);
        let repo = h.fx.repo();
        let start = [h.ids[3]];
        assert_eq!(
            subjects(&repo, &start, Pathspec::new(["a"])),
            ["side", "one"],
            "the merge equals its second parent for `a`, so the first parent line is not followed"
        );
        assert_eq!(subjects(&repo, &start, Pathspec::new(["b"])), ["two"]);
        assert_eq!(
            subjects(&repo, &start, Pathspec::new(["missing"])),
            Vec::<String>::new()
        );
    }
}

#[test]
fn tree_diffs_prune_equal_folders_and_handle_type_changes() {
    for algorithm in ALGORITHMS {
        let fx = Fx::new(algorithm);
        let (x, y, z) = (fx.blob("x"), fx.blob("y"), fx.blob("z"));
        let old = fx.tree(&[
            ("same/deep/f", FileMode::Regular, x),
            ("was-file", FileMode::Regular, x),
            ("was-dir/inner", FileMode::Regular, x),
            ("mode", FileMode::Regular, x),
        ]);
        let new = fx.tree(&[
            ("same/deep/f", FileMode::Regular, x),
            ("was-file/inner", FileMode::Regular, y),
            ("was-dir", FileMode::Symlink, z),
            ("mode", FileMode::Executable, x),
            ("added", FileMode::Regular, y),
        ]);
        let repo = fx.repo();
        let changes = repo
            .diff_trees(Some(&old), Some(&new), &Pathspec::default())
            .unwrap();
        let summary: Vec<(String, bool, bool)> = changes
            .iter()
            .map(|c| {
                (
                    String::from_utf8(c.path.clone()).unwrap(),
                    c.old.is_some(),
                    c.new.is_some(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                ("added".into(), false, true),
                ("mode".into(), true, true),
                ("was-dir".into(), false, true),
                ("was-dir/inner".into(), true, false),
                ("was-file".into(), true, false),
                ("was-file/inner".into(), false, true),
            ]
        );
        let limited = repo
            .diff_trees(Some(&old), Some(&new), &Pathspec::new(["was-file"]))
            .unwrap();
        assert_eq!(limited.len(), 2);
        let none = repo
            .diff_trees(None, Some(&new), &Pathspec::new(["same"]))
            .unwrap();
        assert_eq!(none.len(), 1);
        assert!(
            repo.diff_trees(Some(&new), Some(&new), &Pathspec::default())
                .unwrap()
                .is_empty()
        );
        let flat = repo
            .flatten(Some(&new), &Pathspec::new(["mode", "added"]))
            .unwrap();
        assert_eq!(flat.len(), 2);
        assert_eq!(repo.entry_at(&new, b"same/deep/f").unwrap().unwrap().id, x);
        assert!(repo.entry_at(&new, b"same/deep/nope").unwrap().is_none());
        assert!(repo.entry_at(&new, b"mode/under-a-file").unwrap().is_none());
    }
}

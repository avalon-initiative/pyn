mod common;

use std::collections::HashSet;

use common::git::{ALGORITHMS, Git, build_history};
use pyn_git::{Error, commits_newest_first, existing, reachable_objects};
use pyn_object::{Algorithm, ObjectId};

fn oracle(g: &Git, want: &[ObjectId], have: &[ObjectId]) -> (HashSet<ObjectId>, HashSet<ObjectId>) {
    let mut args = vec![
        "rev-list".to_owned(),
        "--objects-edge-aggressive".to_owned(),
    ];
    args.extend(want.iter().map(ObjectId::to_hex));
    args.extend(have.iter().map(|h| format!("^{h}")));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let objects = g.id_set(&g.run(&refs), true);

    let mut args = vec!["rev-list".to_owned(), "--boundary".to_owned()];
    args.extend(want.iter().map(ObjectId::to_hex));
    args.extend(have.iter().map(|h| format!("^{h}")));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = g.run(&refs);
    let boundary = String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.strip_prefix('-'))
        .map(|h| ObjectId::from_hex(g.algorithm, h).unwrap())
        .collect();
    (objects, boundary)
}

fn check(g: &Git, want: &[ObjectId], have: &[ObjectId], label: &str) {
    let db = g.db();
    let got = reachable_objects(&db, want, have).unwrap();
    let set: HashSet<ObjectId> = got.objects.iter().copied().collect();
    assert_eq!(set.len(), got.objects.len(), "{label}: duplicates");
    let (objects, boundary) = oracle(g, want, have);
    assert_eq!(set, objects, "{label}");
    assert_eq!(
        got.boundary.iter().copied().collect::<HashSet<_>>(),
        boundary,
        "{label}: boundary"
    );
    assert_eq!(
        got,
        reachable_objects(&db, want, have).unwrap(),
        "{label}: stable"
    );
}

#[test]
fn matches_rev_list_objects() {
    for alg in ALGORITHMS {
        let tmp = tempfile::tempdir().unwrap();
        let g = Git::init(&tmp.path().join("r"), alg, false);
        let h = build_history(&g);
        let [c1, c2, c3, c4, c5] = h.c;
        let tree2 = g.rev(&format!("{}^{{tree}}", c2.to_hex()));

        check(&g, &[c5], &[], "everything");
        check(&g, &[c5], &[c2], "linear");
        check(&g, &[c4], &[c1], "merge");
        check(&g, &[c5], &[c4], "after the merge");
        check(&g, &[c2], &[c3], "diverged");
        check(&g, &[c3, c5], &[c1], "two wants");
        check(&g, &[c5], &[c5], "nothing new");
        check(&g, &[c1], &[c5], "want behind have");
        check(&g, &[h.tag], &[], "tag");
        check(&g, &[h.tag], &[c1], "tag with have");
        check(&g, &[h.tag_of_tag], &[], "tag of tag");
        check(&g, &[h.tag_of_tag, c5], &[c1], "tags and commit");
        check(&g, &[c5], &[h.tag], "tag as have");
        check(&g, &[tree2], &[c1], "tree want");
        check(&g, &[c1], &[c1, c2, c3], "several haves");
        check(&g, &[c5, c5], &[c1, c1], "repeated ids");

        let db = g.db();
        let none = reachable_objects(&db, &[], &[c1]).unwrap();
        assert!(none.objects.is_empty() && none.boundary.is_empty());
    }
}

#[test]
fn order_is_tags_then_commits_then_trees_and_blobs() {
    let tmp = tempfile::tempdir().unwrap();
    let g = Git::init(&tmp.path().join("r"), Algorithm::Sha1, false);
    let h = build_history(&g);
    let db = g.db();
    let got = reachable_objects(&db, &[h.tag_of_tag, h.c[4]], &[])
        .unwrap()
        .objects;
    let kinds: Vec<_> = got
        .iter()
        .map(|id| db.read(id).unwrap().kind.name())
        .collect();
    let first_tree = kinds.iter().position(|k| *k == "tree").unwrap();
    assert!(kinds[..2].iter().all(|k| *k == "tag"));
    assert!(kinds[2..first_tree].iter().all(|k| *k == "commit"));
    assert!(
        kinds[first_tree..]
            .iter()
            .all(|k| *k == "tree" || *k == "blob")
    );
    let commits: Vec<_> = got[2..first_tree].to_vec();
    assert_eq!(commits.len(), 5);
    assert_eq!(commits[0], h.c[4]);
}

#[test]
fn missing_objects_are_errors_and_unknown_haves_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let g = Git::init(&tmp.path().join("r"), Algorithm::Sha1, false);
    let h = build_history(&g);
    let db = g.db();
    let ghost = ObjectId::from_hex(Algorithm::Sha1, &"ef".repeat(20)).unwrap();
    assert!(matches!(
        reachable_objects(&db, &[ghost], &[]),
        Err(Error::ObjectNotFound(_))
    ));
    let with_ghost = reachable_objects(&db, &[h.c[4]], &[ghost, h.c[1]]).unwrap();
    let without = reachable_objects(&db, &[h.c[4]], &[h.c[1]]).unwrap();
    assert_eq!(with_ghost, without);

    // Removing a tree from the store makes the walk fail rather than skip it.
    let tree = g.text(&["rev-parse", &format!("{}^{{tree}}", h.c[4].to_hex())]);
    let loose = g.objects_dir().join(&tree[..2]).join(&tree[2..]);
    if loose.exists() {
        std::fs::remove_file(loose).unwrap();
        assert!(reachable_objects(&g.db(), &[h.c[4]], &[]).is_err());
    }
}

#[test]
fn newest_first_walk_and_existing() {
    for alg in ALGORITHMS {
        let tmp = tempfile::tempdir().unwrap();
        let g = Git::init(&tmp.path().join("r"), alg, false);
        let h = build_history(&g);
        let db = g.db();
        let all = commits_newest_first(&db, &[h.c[4]], usize::MAX).unwrap();
        let oracle = g.id_set(&g.run(&["rev-list", &h.c[4].to_hex()]), false);
        assert_eq!(all.iter().copied().collect::<HashSet<_>>(), oracle);
        assert_eq!(all, vec![h.c[4], h.c[3], h.c[2], h.c[1], h.c[0]]);

        assert_eq!(
            commits_newest_first(&db, &[h.c[4]], 2).unwrap(),
            vec![h.c[4], h.c[3]]
        );
        assert!(commits_newest_first(&db, &[h.c[4]], 0).unwrap().is_empty());
        assert_eq!(
            commits_newest_first(&db, &[h.tag_of_tag, h.c[2]], usize::MAX).unwrap(),
            vec![h.c[2], h.c[1], h.c[0]]
        );
        assert!(commits_newest_first(&db, &[], 5).unwrap().is_empty());

        let ghost = ObjectId::from_hex(alg, &"ab".repeat(alg.digest_len())).unwrap();
        assert_eq!(
            existing(&db, &[ghost, h.c[0], ghost, h.c[2]]).unwrap(),
            vec![h.c[0], h.c[2]]
        );
    }
}

#[test]
fn empty_repository_has_nothing_to_walk() {
    let tmp = tempfile::tempdir().unwrap();
    let g = Git::init(&tmp.path().join("r"), Algorithm::Sha1, true);
    let db = g.db();
    let r = reachable_objects(&db, &[], &[]).unwrap();
    assert!(r.objects.is_empty());
    assert!(commits_newest_first(&db, &[], 10).unwrap().is_empty());
}

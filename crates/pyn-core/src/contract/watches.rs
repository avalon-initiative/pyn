//! Watches in an `AccessStore`: one level per user and repository, counted without the ignoring, listed newest first, and dropped with either side.

use super::{Access, at, user};
use crate::{RepoId, WatchLevel};

fn repo(id: &str) -> RepoId {
    RepoId::new(id)
}

pub async fn a_watch_level_is_one_per_user_and_repository_and_replaced(store: Access) {
    let (alice, bob) = (user("alice"), user("bob"));
    for u in [&alice, &bob] {
        store.create_user(u, at(0)).await.unwrap();
    }
    let game = repo("game");
    assert_eq!(store.watch_of(&game, &alice).await.unwrap(), None);
    assert_eq!(store.watcher_count(&game).await.unwrap(), 0);

    store
        .set_watch(&game, &alice, WatchLevel::All, at(1))
        .await
        .unwrap();
    store
        .set_watch(&game, &bob, WatchLevel::Locks, at(2))
        .await
        .unwrap();
    assert_eq!(store.watcher_count(&game).await.unwrap(), 2);
    assert_eq!(
        store.watch_of(&game, &bob).await.unwrap(),
        Some(WatchLevel::Locks)
    );

    store
        .set_watch(&game, &bob, WatchLevel::Ignore, at(3))
        .await
        .unwrap();
    assert_eq!(
        store.watch_of(&game, &bob).await.unwrap(),
        Some(WatchLevel::Ignore),
        "a new level replaces the old one"
    );
    assert_eq!(
        store.watcher_count(&game).await.unwrap(),
        1,
        "an ignoring account is not a watcher"
    );
    assert_eq!(store.watcher_count(&repo("tools")).await.unwrap(), 0);

    assert!(store.clear_watch(&game, &bob).await.unwrap());
    assert!(!store.clear_watch(&game, &bob).await.unwrap());
    assert_eq!(store.watch_of(&game, &bob).await.unwrap(), None);
}

pub async fn a_users_watches_list_newest_first_and_only_theirs(store: Access) {
    let (alice, bob) = (user("alice"), user("bob"));
    for u in [&alice, &bob] {
        store.create_user(u, at(0)).await.unwrap();
    }
    store
        .set_watch(&repo("a"), &alice, WatchLevel::All, at(1))
        .await
        .unwrap();
    store
        .set_watch(&repo("c"), &alice, WatchLevel::Locks, at(3))
        .await
        .unwrap();
    store
        .set_watch(&repo("b"), &alice, WatchLevel::Ignore, at(2))
        .await
        .unwrap();
    store
        .set_watch(&repo("z"), &bob, WatchLevel::All, at(9))
        .await
        .unwrap();

    let listed = store.watches_of(&alice).await.unwrap();
    let seen: Vec<_> = listed.iter().map(|(r, l, _)| (r.as_str(), *l)).collect();
    assert_eq!(
        seen,
        [
            ("c", WatchLevel::Locks),
            ("b", WatchLevel::Ignore),
            ("a", WatchLevel::All)
        ]
    );
    assert_eq!(listed[0].2, at(3));
    assert!(store.watches_of(&user("nobody")).await.unwrap().is_empty());
}

pub async fn watches_go_with_their_repository_and_follow_a_rename(store: Access) {
    let (alice, bob) = (user("alice"), user("bob"));
    for u in [&alice, &bob] {
        store.create_user(u, at(0)).await.unwrap();
    }
    for (r, u) in [("game", &alice), ("game", &bob), ("tools", &alice)] {
        store
            .set_watch(&repo(r), u, WatchLevel::All, at(1))
            .await
            .unwrap();
    }

    store.delete_repo_access(&repo("game")).await.unwrap();
    assert_eq!(store.watcher_count(&repo("game")).await.unwrap(), 0);
    let left = store.watches_of(&alice).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].0, repo("tools"));

    let carol = user("carol");
    store
        .rename_account(&alice, &carol, at(5), &[])
        .await
        .unwrap();
    assert_eq!(
        store.watch_of(&repo("tools"), &carol).await.unwrap(),
        Some(WatchLevel::All)
    );
    assert_eq!(store.watch_of(&repo("tools"), &alice).await.unwrap(), None);
    assert_eq!(store.watcher_count(&repo("tools")).await.unwrap(), 1);
}

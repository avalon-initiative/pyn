//! Stars in an `AccessStore`: one mark per user and repository, counted, listed newest first, and dropped with either side.

use super::{Access, at, user};
use crate::RepoId;

fn repo(id: &str) -> RepoId {
    RepoId::new(id)
}

pub async fn stars_are_one_per_user_and_repository_and_counted(store: Access) {
    let (alice, bob) = (user("alice"), user("bob"));
    for u in [&alice, &bob] {
        store.create_user(u, at(0)).await.unwrap();
    }
    let game = repo("game");
    assert_eq!(store.star_count(&game).await.unwrap(), 0);
    assert!(!store.has_star(&game, &alice).await.unwrap());

    assert!(store.add_star(&game, &alice, at(1)).await.unwrap());
    assert!(
        !store.add_star(&game, &alice, at(2)).await.unwrap(),
        "a second star changes nothing"
    );
    assert!(store.add_star(&game, &bob, at(3)).await.unwrap());
    assert_eq!(store.star_count(&game).await.unwrap(), 2);
    assert!(store.has_star(&game, &alice).await.unwrap());
    assert_eq!(store.star_count(&repo("tools")).await.unwrap(), 0);

    assert!(store.remove_star(&game, &alice).await.unwrap());
    assert!(
        !store.remove_star(&game, &alice).await.unwrap(),
        "removing a missing star changes nothing"
    );
    assert!(!store.has_star(&game, &alice).await.unwrap());
    assert_eq!(store.star_count(&game).await.unwrap(), 1);
}

pub async fn a_users_stars_list_newest_first_and_only_theirs(store: Access) {
    let (alice, bob) = (user("alice"), user("bob"));
    for u in [&alice, &bob] {
        store.create_user(u, at(0)).await.unwrap();
    }
    store.add_star(&repo("a"), &alice, at(1)).await.unwrap();
    store.add_star(&repo("c"), &alice, at(3)).await.unwrap();
    store.add_star(&repo("b"), &alice, at(2)).await.unwrap();
    store.add_star(&repo("z"), &bob, at(9)).await.unwrap();

    let listed: Vec<_> = store.stars_of(&alice).await.unwrap();
    let ids: Vec<_> = listed.iter().map(|(r, _)| r.as_str()).collect();
    assert_eq!(ids, ["c", "b", "a"]);
    assert_eq!(listed[0].1, at(3));
    assert!(store.stars_of(&user("nobody")).await.unwrap().is_empty());
}

pub async fn stars_go_with_their_repository_and_follow_a_rename(store: Access) {
    let (alice, bob) = (user("alice"), user("bob"));
    for u in [&alice, &bob] {
        store.create_user(u, at(0)).await.unwrap();
    }
    store.add_star(&repo("game"), &alice, at(1)).await.unwrap();
    store.add_star(&repo("game"), &bob, at(1)).await.unwrap();
    store.add_star(&repo("tools"), &alice, at(2)).await.unwrap();

    store.delete_repo_access(&repo("game")).await.unwrap();
    assert_eq!(store.star_count(&repo("game")).await.unwrap(), 0);
    let left: Vec<_> = store.stars_of(&alice).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].0, repo("tools"));

    let carol = user("carol");
    store
        .rename_account(&alice, &carol, at(5), &[])
        .await
        .unwrap();
    assert!(store.has_star(&repo("tools"), &carol).await.unwrap());
    assert!(!store.has_star(&repo("tools"), &alice).await.unwrap());
    assert_eq!(store.star_count(&repo("tools")).await.unwrap(), 1);
}

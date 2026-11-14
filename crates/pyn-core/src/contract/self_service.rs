//! Account self-service (#982): added email addresses, renames, address redirects and the name-span ledger.

use super::{Access, Store, at, harness, new_account, path, repo_record, token, user};
use crate::access::{OrgRole, Role, SignupStage};
use crate::account_self::{EmailAdd, EmailRemove, PrimaryChange, RenameOutcome};
use crate::{RepoId, TokenId};

async fn active(store: &Access, name: &str, address: &str, minute: i64) {
    store
        .create_account(new_account(name, address, minute))
        .await
        .unwrap();
    assert!(
        store
            .complete_verification(&user(name), SignupStage::Complete, at(minute))
            .await
            .unwrap()
    );
}

pub async fn added_addresses_are_unique_pending_until_verified_and_the_primary_must_be_verified(
    store: Access,
) {
    active(&store, "alice", "a@example.org", 0).await;
    active(&store, "bob", "b@example.org", 0).await;
    let (alice, bob) = (user("alice"), user("bob"));

    assert_eq!(
        store
            .add_email(&alice, "a2@example.org", at(1))
            .await
            .unwrap(),
        EmailAdd::Added
    );
    assert_eq!(
        store
            .add_email(&alice, "a2@example.org", at(2))
            .await
            .unwrap(),
        EmailAdd::Own
    );
    assert_eq!(
        store
            .add_email(&alice, "a@example.org", at(2))
            .await
            .unwrap(),
        EmailAdd::Own,
        "the primary counts as held"
    );
    assert_eq!(
        store
            .add_email(&bob, "a2@example.org", at(2))
            .await
            .unwrap(),
        EmailAdd::Taken,
        "another account's pending address is refused"
    );
    assert_eq!(
        store.add_email(&bob, "a@example.org", at(2)).await.unwrap(),
        EmailAdd::Taken,
        "so is its primary"
    );

    let held = store.account_emails(&alice).await.unwrap();
    assert_eq!(held.len(), 2);
    assert!(held[0].primary && held[0].verified_at.is_some());
    assert!(!held[1].primary && held[1].verified_at.is_none());

    assert_eq!(
        store
            .set_primary_email(&alice, "a2@example.org")
            .await
            .unwrap(),
        PrimaryChange::Unverified
    );
    assert_eq!(
        store
            .set_primary_email(&alice, "nope@example.org")
            .await
            .unwrap(),
        PrimaryChange::NotFound
    );
    assert!(
        !store
            .confirm_added_email(&bob, "a2@example.org", at(3))
            .await
            .unwrap()
    );
    assert!(
        store
            .confirm_added_email(&alice, "a2@example.org", at(3))
            .await
            .unwrap()
    );
    assert!(
        !store
            .confirm_added_email(&alice, "a2@example.org", at(4))
            .await
            .unwrap(),
        "a link works once"
    );
    assert_eq!(
        store.verified_email_owner("a2@example.org").await.unwrap(),
        Some(alice.clone())
    );

    assert_eq!(
        store
            .set_primary_email(&alice, "a2@example.org")
            .await
            .unwrap(),
        PrimaryChange::Changed
    );
    let held = store.account_emails(&alice).await.unwrap();
    assert_eq!(held[0].address, "a2@example.org");
    assert!(held[0].primary);
    assert_eq!(held[1].address, "a@example.org");
    assert!(
        held[1].verified_at.is_some(),
        "the old primary stays, verified"
    );
    assert_eq!(
        store
            .account(&alice)
            .await
            .unwrap()
            .unwrap()
            .email
            .as_deref(),
        Some("a2@example.org")
    );
}

pub async fn removing_an_address_promotes_a_verified_one_and_can_keep_the_last(store: Access) {
    active(&store, "alice", "a@example.org", 0).await;
    let alice = user("alice");
    store
        .add_email(&alice, "b@example.org", at(1))
        .await
        .unwrap();
    store
        .add_email(&alice, "c@example.org", at(2))
        .await
        .unwrap();
    store
        .confirm_added_email(&alice, "b@example.org", at(3))
        .await
        .unwrap();

    assert_eq!(
        store
            .remove_email(&alice, "zzz@example.org", true)
            .await
            .unwrap(),
        EmailRemove::NotFound
    );
    assert_eq!(
        store
            .remove_email(&alice, "c@example.org", true)
            .await
            .unwrap(),
        EmailRemove::Removed,
        "an unverified address always goes"
    );
    assert_eq!(
        store
            .remove_email(&alice, "a@example.org", true)
            .await
            .unwrap(),
        EmailRemove::Removed
    );
    let held = store.account_emails(&alice).await.unwrap();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].address, "b@example.org");
    assert!(held[0].primary, "the verified address was promoted");

    assert_eq!(
        store
            .remove_email(&alice, "b@example.org", true)
            .await
            .unwrap(),
        EmailRemove::LastVerified
    );
    assert_eq!(store.account_emails(&alice).await.unwrap().len(), 1);
    assert_eq!(
        store
            .remove_email(&alice, "b@example.org", false)
            .await
            .unwrap(),
        EmailRemove::Removed
    );
    assert!(store.account_emails(&alice).await.unwrap().is_empty());
    assert_eq!(
        store
            .add_email(&user("bob"), "b@example.org", at(5))
            .await
            .unwrap_or(EmailAdd::Added),
        EmailAdd::Added,
        "a removed address is free again"
    );
}

pub async fn a_rename_moves_what_names_the_account_and_refuses_a_taken_name(store: Access) {
    active(&store, "alice", "a@example.org", 0).await;
    store.create_user(&user("bob"), at(0)).await.unwrap();
    store
        .create_org(&user("acme"), &user("alice"), at(1))
        .await
        .unwrap();
    let repo = RepoId::new("game");
    store
        .set_role(&repo, &user("alice"), Role::Admin)
        .await
        .unwrap();
    store
        .create_token(token("aaaaaaaaaaaa", "alice", 1))
        .await
        .unwrap();
    store
        .add_email(&user("alice"), "x@example.org", at(2))
        .await
        .unwrap();
    let id = store.account(&user("alice")).await.unwrap().unwrap().id;

    assert_eq!(
        store
            .rename_account(&user("alice"), &user("bob"), at(5), &[])
            .await
            .unwrap(),
        RenameOutcome::NameTaken
    );
    assert_eq!(
        store
            .rename_account(&user("alice"), &user("acme"), at(5), &[])
            .await
            .unwrap(),
        RenameOutcome::NameTaken,
        "an organization's name cannot be taken by renaming"
    );
    assert_eq!(
        store
            .rename_account(&user("acme"), &user("acme2"), at(5), &[])
            .await
            .unwrap(),
        RenameOutcome::NoAccount,
        "organizations are not renamed"
    );
    assert!(
        store.account(&user("alice")).await.unwrap().is_some(),
        "a refusal changes nothing"
    );

    assert_eq!(
        store
            .rename_account(&user("alice"), &user("alicia"), at(10), &[])
            .await
            .unwrap(),
        RenameOutcome::Renamed {
            account: id.clone()
        }
    );
    assert!(store.account(&user("alice")).await.unwrap().is_none());
    let moved = store.account(&user("alicia")).await.unwrap().unwrap();
    assert_eq!(moved.id, id, "the account id does not change");
    assert_eq!(moved.user, user("alicia"));
    assert_eq!(
        store
            .password_hash(&user("alicia"))
            .await
            .unwrap()
            .as_deref(),
        Some("hash-alice")
    );
    assert_eq!(
        store.role_of(&repo, &user("alicia")).await.unwrap(),
        Some(Role::Admin)
    );
    assert_eq!(store.role_of(&repo, &user("alice")).await.unwrap(), None);
    assert_eq!(
        store
            .org_role(&user("acme"), &user("alicia"))
            .await
            .unwrap(),
        Some(OrgRole::Owner)
    );
    let tok = store
        .get_token(&TokenId("aaaaaaaaaaaa".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tok.user, user("alicia"), "a token follows the account");
    assert_eq!(
        store.account_emails(&user("alicia")).await.unwrap().len(),
        2
    );

    assert!(
        store.create_user(&user("alice"), at(11)).await.unwrap(),
        "the old name is free"
    );
}

pub async fn name_spans_resolve_an_author_to_the_accounts_current_name(store: Access) {
    active(&store, "alice", "a@example.org", 0).await;
    let rename = |from: &'static str, to: &'static str, minute| {
        let store = store.clone();
        async move {
            store
                .rename_account(&user(from), &user(to), at(minute), &[])
                .await
                .unwrap()
        }
    };
    rename("alice", "alicia", 10).await;
    store.create_user(&user("alice"), at(11)).await.unwrap();
    rename("alicia", "ali", 20).await;

    let names = store
        .current_names(&[
            (user("alice"), at(5)),
            (user("alice"), at(10)),
            (user("alicia"), at(15)),
            (user("alicia"), at(20)),
            (user("ali"), at(30)),
            (user("nobody"), at(5)),
        ])
        .await
        .unwrap();
    assert_eq!(
        names[0],
        Some(user("ali")),
        "written before the first rename"
    );
    assert_eq!(
        names[1], None,
        "at the rename instant the new holder owns the name"
    );
    assert_eq!(
        names[2],
        Some(user("ali")),
        "an intermediate name resolves to the latest"
    );
    assert_eq!(names[3], None);
    assert_eq!(names[4], None, "the current name needs no mapping");
    assert_eq!(names[5], None);
}

pub async fn a_rename_moves_repositories_and_locks_and_leaves_redirects(store: Store) {
    let h = harness(store.clone());
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    store
        .create_repo(repo_record("r1", "alice", "Game"))
        .await
        .unwrap();
    store
        .create_repo(repo_record("r2", "alice", "tools"))
        .await
        .unwrap();
    store
        .create_repo(repo_record("r3", "bob", "other"))
        .await
        .unwrap();

    let moved = store
        .rename_owner(&user("alice"), &user("alicia"), at(5))
        .await
        .unwrap();
    let names: Vec<_> = moved.iter().map(|r| r.address()).collect();
    assert_eq!(names, ["alicia/Game", "alicia/tools"]);
    assert!(
        store
            .find_repo(&user("alice"), "game")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .find_repo(&user("alicia"), "game")
            .await
            .unwrap()
            .unwrap()
            .id,
        RepoId::new("r1")
    );
    assert_eq!(
        h.svc.locks().await.unwrap()[0].owner,
        user("alicia"),
        "a held lock follows its holder"
    );

    let via = store
        .find_repo_redirect(&user("alice"), "GAME")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        via.address(),
        "alicia/Game",
        "a redirect matches the name ignoring case"
    );
    assert!(
        store
            .find_repo_redirect(&user("alice"), "nothing")
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .find_repo_redirect(&user("bob"), "other")
            .await
            .unwrap()
            .is_none()
    );

    assert!(store.delete_repo(&RepoId::new("r2")).await.unwrap());
    assert!(
        store
            .find_repo_redirect(&user("alice"), "tools")
            .await
            .unwrap()
            .is_none(),
        "a deleted repository leaves no redirect"
    );
}

pub async fn a_live_repository_ends_the_redirect_for_its_own_name_only(store: Store) {
    store
        .create_repo(repo_record("r1", "alice", "game"))
        .await
        .unwrap();
    store
        .create_repo(repo_record("r2", "alice", "tools"))
        .await
        .unwrap();
    store
        .rename_owner(&user("alice"), &user("alicia"), at(5))
        .await
        .unwrap();

    store
        .create_repo(repo_record("n1", "alice", "game"))
        .await
        .unwrap();
    assert!(
        store
            .find_repo_redirect(&user("alice"), "game")
            .await
            .unwrap()
            .is_none(),
        "creating the repository under the old owner removes its redirect"
    );
    assert_eq!(
        store
            .find_repo(&user("alice"), "game")
            .await
            .unwrap()
            .unwrap()
            .id,
        RepoId::new("n1")
    );
    assert_eq!(
        store
            .find_repo_redirect(&user("alice"), "tools")
            .await
            .unwrap()
            .unwrap()
            .id,
        RepoId::new("r2"),
        "the other repository still redirects"
    );

    store
        .create_repo(repo_record("n2", "alice", "extra"))
        .await
        .unwrap();
    store
        .update_repo(
            &RepoId::new("n2"),
            crate::RepoUpdate {
                name: Some("Tools".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        store
            .find_repo_redirect(&user("alice"), "tools")
            .await
            .unwrap()
            .is_none(),
        "renaming a repository to the name ends that redirect too"
    );
}

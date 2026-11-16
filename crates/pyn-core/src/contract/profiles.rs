//! Account profiles in an `AccessStore`: text fields and the avatar record are stored apart and survive each other.

use super::{Access, at, user};
use crate::access::SignupStage;
use crate::profile::{Avatar, ImageKind, ProfileFields, ProfileVisibility};
use crate::{AccountDeletion, ObjectId};

fn fields(name: &str) -> ProfileFields {
    ProfileFields {
        display_name: Some(name.into()),
        bio: Some("builds tools\nand more".into()),
        location: Some("Lisbon".into()),
        company: None,
        links: vec!["https://a.example".into(), "https://b.example/x".into()],
    }
}

fn avatar(byte: u8) -> Avatar {
    Avatar {
        content: ObjectId::from_bytes(pyn_object::Algorithm::Sha256, &[byte; 32]).unwrap(),
        kind: ImageKind::Webp,
        size: 4096,
    }
}

pub async fn profiles_store_text_fields_and_an_avatar_apart(store: Access) {
    let alice = user("alice");
    store.create_user(&alice, at(0)).await.unwrap();
    assert_eq!(
        store.profile(&alice).await.unwrap(),
        Default::default(),
        "no profile until one is saved"
    );

    store
        .set_profile(&alice, &fields("Alice"), at(1))
        .await
        .unwrap();
    let saved = store.profile(&alice).await.unwrap();
    assert_eq!(saved.fields, fields("Alice"));
    assert_eq!(saved.avatar, None);

    store
        .set_avatar(&alice, Some(&avatar(7)), at(2))
        .await
        .unwrap();
    let saved = store.profile(&alice).await.unwrap();
    assert_eq!(saved.avatar, Some(avatar(7)));
    assert_eq!(
        saved.fields,
        fields("Alice"),
        "an avatar leaves the text alone"
    );

    store
        .set_profile(&alice, &ProfileFields::default(), at(3))
        .await
        .unwrap();
    let saved = store.profile(&alice).await.unwrap();
    assert_eq!(
        saved.fields,
        ProfileFields::default(),
        "saving replaces every field"
    );
    assert_eq!(
        saved.avatar,
        Some(avatar(7)),
        "text leaves the avatar alone"
    );

    store
        .set_avatar(&alice, Some(&avatar(8)), at(4))
        .await
        .unwrap();
    assert_eq!(store.profile(&alice).await.unwrap().avatar, Some(avatar(8)));
    store.set_avatar(&alice, None, at(5)).await.unwrap();
    assert_eq!(store.profile(&alice).await.unwrap().avatar, None);
}

pub async fn profiles_belong_to_one_account_each(store: Access) {
    let (alice, bob) = (user("alice"), user("bob"));
    store.create_user(&alice, at(0)).await.unwrap();
    store.create_user(&bob, at(0)).await.unwrap();
    store
        .set_profile(&alice, &fields("Alice"), at(1))
        .await
        .unwrap();
    store
        .set_avatar(&alice, Some(&avatar(1)), at(1))
        .await
        .unwrap();
    assert_eq!(store.profile(&bob).await.unwrap(), Default::default());
    store
        .set_profile(&bob, &fields("Bob"), at(2))
        .await
        .unwrap();
    let alice_profile = store.profile(&alice).await.unwrap();
    assert_eq!(alice_profile.fields.display_name.as_deref(), Some("Alice"));
    assert_eq!(store.profile(&bob).await.unwrap().avatar, None);
}

pub async fn profile_visibility_defaults_public_and_is_stored_apart(store: Access) {
    let alice = user("alice");
    store.create_user(&alice, at(0)).await.unwrap();
    assert_eq!(
        store.profile(&alice).await.unwrap().visibility,
        ProfileVisibility::default(),
        "everything is public until set"
    );

    let hidden = ProfileVisibility {
        display_name: false,
        links: false,
        avatar: false,
        ..Default::default()
    };
    store
        .set_profile_visibility(&alice, &hidden, at(1))
        .await
        .unwrap();
    store
        .set_profile(&alice, &fields("Alice"), at(2))
        .await
        .unwrap();
    store
        .set_avatar(&alice, Some(&avatar(3)), at(3))
        .await
        .unwrap();
    let saved = store.profile(&alice).await.unwrap();
    assert_eq!(saved.visibility, hidden, "text and avatar keep visibility");
    assert_eq!(saved.fields, fields("Alice"));
    assert_eq!(saved.avatar, Some(avatar(3)));

    let public = ProfileVisibility::default();
    store
        .set_profile_visibility(&alice, &public, at(4))
        .await
        .unwrap();
    let saved = store.profile(&alice).await.unwrap();
    assert_eq!(saved.visibility, public);
    assert_eq!(saved.fields, fields("Alice"), "visibility keeps the text");
    assert_eq!(saved.avatar, Some(avatar(3)));
}

pub async fn org_profiles_store_a_description_and_an_avatar_apart(store: Access) {
    let (acme, alice) = (user("acme"), user("alice"));
    store.create_user(&alice, at(0)).await.unwrap();
    assert!(store.create_org(&acme, &alice, at(1)).await.unwrap());
    assert_eq!(
        store.org_profile(&acme).await.unwrap(),
        Default::default(),
        "no profile until one is saved"
    );

    store
        .set_org_description(&acme, Some("we build tools"), at(2))
        .await
        .unwrap();
    store
        .set_org_avatar(&acme, Some(&avatar(5)), at(3))
        .await
        .unwrap();
    let saved = store.org_profile(&acme).await.unwrap();
    assert_eq!(saved.description.as_deref(), Some("we build tools"));
    assert_eq!(saved.avatar, Some(avatar(5)));

    store.set_org_description(&acme, None, at(4)).await.unwrap();
    let saved = store.org_profile(&acme).await.unwrap();
    assert_eq!(saved.description, None, "clearing the text");
    assert_eq!(saved.avatar, Some(avatar(5)), "keeps the avatar");

    store
        .set_org_description(&acme, Some("again"), at(5))
        .await
        .unwrap();
    store.set_org_avatar(&acme, None, at(6)).await.unwrap();
    let saved = store.org_profile(&acme).await.unwrap();
    assert_eq!(saved.avatar, None);
    assert_eq!(
        saved.description.as_deref(),
        Some("again"),
        "keeps the text"
    );
}

pub async fn org_profiles_are_apart_from_user_profiles_and_go_with_the_org(store: Access) {
    let (acme, alice) = (user("acme"), user("alice"));
    store.create_user(&alice, at(0)).await.unwrap();
    assert!(store.create_org(&acme, &alice, at(1)).await.unwrap());
    store
        .set_org_description(&acme, Some("acme"), at(2))
        .await
        .unwrap();
    store
        .set_org_avatar(&acme, Some(&avatar(2)), at(2))
        .await
        .unwrap();
    store
        .set_profile(&alice, &fields("Alice"), at(3))
        .await
        .unwrap();
    assert_eq!(store.profile(&acme).await.unwrap(), Default::default());
    assert_eq!(store.org_profile(&alice).await.unwrap(), Default::default());

    assert!(store.delete_org(&acme).await.unwrap());
    assert_eq!(
        store.org_profile(&acme).await.unwrap(),
        Default::default(),
        "deleting the organization drops its profile"
    );
    assert!(store.create_org(&acme, &alice, at(4)).await.unwrap());
    assert_eq!(
        store.org_profile(&acme).await.unwrap(),
        Default::default(),
        "a recreated organization starts empty"
    );
}

pub async fn the_people_search_lists_active_accounts_and_matches_only_what_is_visible(
    store: Access,
) {
    for (i, n) in ["alice", "bob", "carol", "dave", "erin", "Frank"]
        .iter()
        .enumerate()
    {
        store.create_user(&user(n), at(i as i64)).await.unwrap();
    }
    let named = |n: &str| ProfileFields {
        display_name: Some(n.into()),
        ..Default::default()
    };
    store
        .set_profile(&user("alice"), &named("Zed Quartz"), at(9))
        .await
        .unwrap();
    store
        .set_profile(&user("bob"), &named("Secret Plum"), at(9))
        .await
        .unwrap();
    let hidden = ProfileVisibility {
        display_name: false,
        ..Default::default()
    };
    store
        .set_profile_visibility(&user("bob"), &hidden, at(9))
        .await
        .unwrap();
    store
        .set_disabled(&user("carol"), Some((at(9), None)))
        .await
        .unwrap();
    store
        .set_signup(&user("dave"), SignupStage::PendingApproval)
        .await
        .unwrap();
    let deletion = AccountDeletion {
        user: user("erin"),
        requested_by: user("erin"),
        by_owner: true,
        reason: None,
        scheduled_at: at(9),
        purge_at: at(99),
    };
    store
        .schedule_account_deletion_with_events(deletion, false, &[])
        .await
        .unwrap();

    let names = |found: Vec<crate::UserId>| -> Vec<String> {
        found.iter().map(|u| u.to_string()).collect()
    };
    let all = store.search_people(None, None, 10).await.unwrap();
    assert_eq!(
        names(all),
        ["Frank", "alice", "bob"],
        "active only, by name"
    );
    let page = store.search_people(None, None, 2).await.unwrap();
    assert_eq!(names(page), ["Frank", "alice"]);
    let next = store
        .search_people(None, Some(&user("alice")), 2)
        .await
        .unwrap();
    assert_eq!(names(next), ["bob"], "strictly after the cursor");

    let hit = |q: &'static str| {
        let store = store.clone();
        async move { names(store.search_people(Some(q), None, 10).await.unwrap()) }
    };
    assert_eq!(hit("ali").await, ["alice"], "by account name");
    assert_eq!(hit("frank").await, ["Frank"], "case-insensitive");
    assert_eq!(hit("quartz").await, ["alice"], "by a visible display name");
    assert!(
        hit("plum").await.is_empty(),
        "a hidden display name matches nothing"
    );
    assert!(hit("secret").await.is_empty());
    assert_eq!(hit("bob").await, ["bob"], "its account name still does");
    for gone in ["carol", "dave", "erin"] {
        assert!(hit(gone).await.is_empty(), "{gone} is not listed");
    }
    assert!(
        hit("%").await.is_empty() && hit("_").await.is_empty(),
        "no wildcards"
    );
}

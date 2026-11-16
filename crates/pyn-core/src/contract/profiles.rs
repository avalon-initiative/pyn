//! Account profiles in an `AccessStore`: text fields and the avatar record are stored apart and survive each other.

use super::{Access, at, user};
use crate::ObjectId;
use crate::profile::{Avatar, ImageKind, ProfileFields, ProfileVisibility};

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

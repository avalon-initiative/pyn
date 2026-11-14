//! Account profiles in an `AccessStore`: text fields and the avatar record are stored apart and survive each other.

use super::{Access, at, user};
use crate::ObjectId;
use crate::profile::{Avatar, ImageKind, ProfileFields};

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

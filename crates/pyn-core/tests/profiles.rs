//! Public profiles: what a viewer may see of an account, and avatar storage.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::profiles::Profiles;
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Credential, Identity, ManualClock, ObjectStore,
    OrgRole, OwnerProfile, ProfileFields, ProfileVisibility, PynError, Repositories, Role, Rules,
    UserId, Visibility,
};

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    store: Arc<MemoryAccessStore>,
    objects: Arc<dyn ObjectStore>,
}

fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig::default())
        .with_audit(audit.clone()),
    );
    let objects: Arc<dyn ObjectStore> = Arc::new(MemoryObjectStore::new());
    let repos = Repositories::new(
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    );
    World {
        repos,
        access,
        store,
        objects,
    }
}

impl World {
    fn profiles(&self) -> Profiles<'_> {
        Profiles {
            access: &self.access,
            repos: &self.repos,
            objects: &self.objects,
        }
    }

    async fn user(&self, name: &str) -> Identity {
        self.store
            .create_user(&UserId::new(name), Utc::now())
            .await
            .unwrap();
        session(name)
    }
}

fn session(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

async fn orgs(p: &Profiles<'_>, v: Option<&Identity>) -> Vec<(String, OrgRole)> {
    let found = p.public("alice", v).await.unwrap().organizations;
    found
        .into_iter()
        .map(|(o, role)| (o.user.to_string(), role))
        .collect()
}

fn names(p: &pyn_core::PublicProfile) -> Vec<String> {
    p.repositories.iter().map(|(r, _)| r.name.clone()).collect()
}

fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.resize(64, 7);
    b
}

#[tokio::test]
async fn a_stranger_sees_only_public_repositories_and_the_owner_and_members_see_more() {
    let w = world();
    let alice = w.user("alice").await;
    let bob = w.user("bob").await;
    let carol = w.user("carol").await;
    let dave = w.user("dave").await;
    let (a, o) = (UserId::new("alice"), w.profiles());
    for name in ["open", "secret", "shared"] {
        let rec = w.repos.create(&alice, &a, name, None, None).await.unwrap();
        if name == "open" {
            let update = pyn_core::RepoUpdate {
                visibility: Some(Visibility::Public),
                ..Default::default()
            };
            w.repos.update(&alice, &rec, update).await.unwrap();
        }
        if name == "shared" {
            w.store
                .set_role(&rec.id, &UserId::new("bob"), Role::Reader)
                .await
                .unwrap();
        }
    }

    let signed_out = o.public("alice", None).await.unwrap();
    assert_eq!(names(&signed_out), ["open"]);
    let stranger = o.public("alice", Some(&carol)).await.unwrap();
    assert_eq!(names(&stranger), ["open"]);
    let member = o.public("alice", Some(&bob)).await.unwrap();
    assert_eq!(names(&member), ["open", "shared"]);
    let owner = o.public("alice", Some(&alice)).await.unwrap();
    assert_eq!(names(&owner), ["open", "secret", "shared"]);
    assert!(
        o.public("alice", Some(&dave))
            .await
            .unwrap()
            .repositories
            .iter()
            .all(|(r, _)| r.name != "secret")
    );
}

#[tokio::test]
async fn organizations_appear_only_to_viewers_who_belong_to_them_too() {
    let w = world();
    let alice = w.user("alice").await;
    let bob = w.user("bob").await;
    let carol = w.user("carol").await;
    let acme = UserId::new("acme");
    w.store
        .create_org(&acme, &UserId::new("alice"), Utc::now())
        .await
        .unwrap();
    w.store
        .add_org_member(&acme, &UserId::new("bob"), OrgRole::Member)
        .await
        .unwrap();

    let p = w.profiles();
    assert!(orgs(&p, None).await.is_empty());
    assert!(orgs(&p, Some(&carol)).await.is_empty());
    assert_eq!(
        orgs(&p, Some(&bob)).await,
        [("acme".into(), OrgRole::Member)]
    );
    assert_eq!(
        orgs(&p, Some(&alice)).await,
        [("acme".into(), OrgRole::Owner)]
    );
}

#[tokio::test]
async fn missing_disabled_unapproved_and_organization_accounts_are_all_not_found() {
    let w = world();
    w.user("alice").await;
    w.user("gone").await;
    w.store
        .set_disabled(&UserId::new("gone"), Some((Utc::now(), None)))
        .await
        .unwrap();
    w.store
        .create_org(&UserId::new("acme"), &UserId::new("alice"), Utc::now())
        .await
        .unwrap();
    let p = w.profiles();
    for name in ["nobody", "gone", "acme"] {
        let err = p.public(name, None).await.unwrap_err();
        assert!(matches!(err, PynError::UserNotFound(_)), "{name}: {err}");
        assert_eq!(err.code(), "user_not_found");
    }
    assert!(p.avatar("gone", None).await.is_err());
    assert!(p.public("alice", None).await.is_ok());
}

#[tokio::test]
async fn profile_text_is_validated_and_replaced() {
    let w = world();
    let alice = w.user("alice").await;
    let saved = w
        .access
        .update_profile(
            &alice,
            ProfileFields {
                display_name: Some("  Alice  ".into()),
                links: vec!["https://a.example".into()],
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(saved.fields.display_name.as_deref(), Some("Alice"));
    let err = w
        .access
        .update_profile(
            &alice,
            ProfileFields {
                bio: Some("x".repeat(161)),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_request");
    let again = w.access.profile_of(&UserId::new("alice")).await.unwrap();
    assert_eq!(again, saved, "a refused update changes nothing");
}

#[tokio::test]
async fn an_avatar_is_stored_by_content_hash_served_and_removed() {
    let w = world();
    let alice = w.user("alice").await;
    let p = w.profiles();
    let saved = p.set_avatar(&alice, png()).await.unwrap();
    let avatar = saved.avatar.expect("avatar recorded");
    assert_eq!(avatar.kind.media_type(), "image/png");
    assert_eq!(avatar.size, 64);
    assert!(w.objects.exists(&avatar.content).await.unwrap());

    let (got, bytes) = p.avatar("alice", None).await.unwrap();
    assert_eq!((got, bytes), (avatar.clone(), png()));
    let same = p.set_avatar(&alice, png()).await.unwrap();
    assert_eq!(
        same.avatar.unwrap().content,
        avatar.content,
        "same bytes, same hash"
    );

    let err = p
        .set_avatar(&alice, b"GIF89a....".to_vec())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "unsupported_image");
    let mut big = png();
    big.resize(1 << 20 | 1, 0);
    let err = p.set_avatar(&alice, big).await.unwrap_err();
    assert_eq!(err.code(), "object_too_large");
    assert!(
        p.avatar("alice", None).await.is_ok(),
        "refusals keep the old avatar"
    );

    p.remove_avatar(&alice).await.unwrap();
    assert_eq!(
        p.avatar("alice", None).await.unwrap_err().code(),
        "avatar_not_found"
    );
}

#[tokio::test]
async fn hidden_fields_are_absent_for_everyone_but_the_owner() {
    let w = world();
    let alice = w.user("alice").await;
    let bob = w.user("bob").await;
    let p = w.profiles();
    p.set_avatar(&alice, png()).await.unwrap();
    let fields = ProfileFields {
        display_name: Some("Alice".into()),
        bio: Some("bio".into()),
        location: Some("Lisbon".into()),
        links: vec!["https://a.example".into()],
        ..Default::default()
    };
    let hide = ProfileVisibility {
        display_name: false,
        bio: false,
        links: false,
        avatar: false,
        ..Default::default()
    };
    w.access
        .update_profile(&alice, fields, Some(hide))
        .await
        .unwrap();

    let seen = p.public("alice", Some(&bob)).await.unwrap();
    assert!(!seen.own);
    let f = seen.profile.fields;
    assert_eq!((f.display_name, f.bio, f.links), (None, None, vec![]));
    assert_eq!(f.location.as_deref(), Some("Lisbon"));
    assert!(seen.profile.avatar.is_none());
    assert!(
        p.public("alice", None)
            .await
            .unwrap()
            .profile
            .avatar
            .is_none()
    );
    assert_eq!(
        p.avatar("alice", Some(&bob)).await.unwrap_err().code(),
        "avatar_not_found"
    );
    let ids = w
        .access
        .public_profiles(&[UserId::new("alice")])
        .await
        .unwrap();
    let (_, listed) = ids[0].clone().unwrap();
    assert_eq!(listed.fields.display_name, None);
    assert!(listed.avatar.is_none());

    let own = p.public("alice", Some(&alice)).await.unwrap();
    assert!(own.own);
    assert_eq!(own.profile.fields.display_name.as_deref(), Some("Alice"));
    assert!(own.profile.avatar.is_some());
    assert_eq!(own.profile.visibility, hide);
    assert!(p.avatar("alice", Some(&alice)).await.is_ok());
}

#[tokio::test]
async fn an_update_without_visibility_keeps_it() {
    let w = world();
    let alice = w.user("alice").await;
    let hide = ProfileVisibility {
        bio: false,
        ..Default::default()
    };
    w.access
        .update_profile(&alice, ProfileFields::default(), Some(hide))
        .await
        .unwrap();
    let saved = w
        .access
        .update_profile(&alice, ProfileFields::default(), None)
        .await
        .unwrap();
    assert_eq!(saved.visibility, hide);
}

#[tokio::test]
async fn only_organization_owners_edit_its_profile() {
    let w = world();
    let (alice, bob) = (w.user("alice").await, w.user("bob").await);
    let acme = UserId::new("acme");
    w.store
        .create_org(&acme, &alice.user, Utc::now())
        .await
        .unwrap();
    w.store
        .add_org_member(&acme, &bob.user, OrgRole::Member)
        .await
        .unwrap();
    let p = w.profiles();

    assert_eq!(p.org_profile("acme").await.unwrap(), Default::default());
    let saved = w
        .access
        .update_org_fields(
            &alice,
            &acme,
            pyn_core::OrgProfileFields {
                description: Some(" hi ".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.description.as_deref(), Some("hi"));
    let saved = p.set_org_avatar(&alice, "acme", png()).await.unwrap();
    let avatar = saved.avatar.expect("avatar");
    assert_eq!(saved.description.as_deref(), Some("hi"));
    let (served, bytes) = p.org_avatar("acme").await.unwrap();
    assert_eq!((served, bytes), (avatar, png()));

    let err = p.set_org_avatar(&bob, "acme", png()).await.unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");
    let err = w
        .access
        .update_org_fields(&bob, &acme, Default::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");
    let err = p
        .set_org_avatar(&alice, "acme", b"nope".to_vec())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UnsupportedImage(_)), "{err}");
    let err = p.set_org_avatar(&alice, "alice", png()).await.unwrap_err();
    assert!(matches!(err, PynError::OrgNotFound(_)), "{err}");

    p.remove_org_avatar(&alice, "acme").await.unwrap();
    assert!(matches!(
        p.org_avatar("acme").await.unwrap_err(),
        PynError::AvatarNotFound(_)
    ));
    let owners = w
        .access
        .owner_profiles(&[alice.user.clone(), acme.clone()])
        .await
        .unwrap();
    assert!(matches!(owners[0], Some(OwnerProfile::User(_))));
    assert!(matches!(owners[1], Some(OwnerProfile::Org(_))));
}

#[tokio::test]
async fn owner_profiles_show_what_a_stranger_sees_and_only_active_users() {
    let w = world();
    let alice = w.user("alice").await;
    w.user("gone").await;
    w.store
        .set_disabled(&UserId::new("gone"), Some((Utc::now(), None)))
        .await
        .unwrap();
    let fields = ProfileFields {
        display_name: Some("Alice".into()),
        bio: Some("hidden too".into()),
        ..Default::default()
    };
    let hidden = ProfileVisibility {
        display_name: false,
        bio: false,
        ..Default::default()
    };
    w.access
        .update_profile(&alice, fields, Some(hidden))
        .await
        .unwrap();
    w.profiles().set_avatar(&alice, png()).await.unwrap();

    let names = ["alice", "gone", "nobody"].map(UserId::new);
    let found = w.access.owner_profiles(&names).await.unwrap();
    let Some(OwnerProfile::User(p)) = &found[0] else {
        panic!("{found:?}");
    };
    assert_eq!(
        (p.fields.display_name.as_ref(), p.fields.bio.as_ref()),
        (None, None),
        "hidden even from its owner"
    );
    assert!(p.avatar.is_some());
    assert!(found[1].is_none(), "disabled");
    assert!(found[2].is_none(), "no account");
}

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::MemoryAccessStore;
use pyn_core::{
    AccessService, Credential, Identity, ManualClock, Permission, PynError, RepoId, Role, UserId,
    ssh,
};

const ED1: &str = include_str!("fixtures/keys/ed1.pub");
const ED2: &str = include_str!("fixtures/keys/ed2.pub");
const EC: &str = include_str!("fixtures/keys/ec.pub");
const RSA2048: &str = include_str!("fixtures/keys/rsa2048.pub");
const RSA1024: &str = include_str!("fixtures/keys/rsa1024.pub");
const DSA: &str = include_str!("fixtures/keys/dsa.pub");
const FINGERPRINTS: &str = include_str!("fixtures/keys/fingerprints.txt");

fn fingerprint_from_ssh_keygen(name: &str) -> &'static str {
    FINGERPRINTS
        .lines()
        .find_map(|l| l.strip_prefix(name)?.strip_prefix(' '))
        .expect("fixture")
}

#[test]
fn fingerprints_match_what_ssh_keygen_prints() {
    for (name, key) in [("ed1", ED1), ("ed2", ED2), ("ec", EC), ("rsa2048", RSA2048)] {
        let parsed = ssh::parse(key).unwrap();
        assert_eq!(
            parsed.fingerprint,
            fingerprint_from_ssh_keygen(name),
            "{name}"
        );
    }
}

#[test]
fn keys_are_normalised_without_their_comment() {
    let parsed = ssh::parse(ED1).unwrap();
    assert_eq!(
        (parsed.algorithm.as_str(), parsed.comment.as_str()),
        ("ssh-ed25519", "alice@laptop")
    );
    assert!(
        parsed.public_key.starts_with("ssh-ed25519 AAAA")
            && !parsed.public_key.contains("alice@laptop")
    );
    assert_eq!(
        ssh::parse(&format!("  {}  \n", ED1.trim())).unwrap(),
        parsed,
        "whitespace does not matter"
    );
    assert_eq!(
        ssh::parse(&parsed.public_key).unwrap().fingerprint,
        parsed.fingerprint
    );
}

#[test]
fn weak_unknown_and_malformed_keys_are_refused() {
    for (why, key) in [("dsa", DSA), ("small rsa", RSA1024)] {
        let err = ssh::parse(key).unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{why}: {err}");
    }
    for junk in [
        "",
        "not a key",
        "ssh-ed25519",
        "ssh-ed25519 AAAA",
        "ssh-ed25519 !!!notbase64!!!",
    ] {
        assert!(ssh::parse(junk).is_err(), "{junk:?} should be refused");
    }
    assert!(
        ssh::parse("-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----")
            .is_err()
    );
}

struct World {
    svc: AccessService,
    repo: RepoId,
}

fn session(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

async fn world() -> (World, Identity, Identity) {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let w = World {
        svc: AccessService::new(Arc::new(MemoryAccessStore::new()), clock),
        repo: RepoId::new("game"),
    };
    w.svc.admin_for_tests(&UserId::new("root")).await.unwrap();
    w.svc
        .add_creator(&w.repo, &UserId::new("root"))
        .await
        .unwrap();
    let root = w
        .svc
        .principal(&w.repo, &UserId::new("root"))
        .await
        .unwrap();
    w.svc
        .add_user(&root, &w.repo, "alice", "a long password", Role::Writer)
        .await
        .unwrap();
    (w, session("root"), session("alice"))
}

#[tokio::test]
async fn a_person_links_keys_to_their_own_account() {
    let (w, _root, alice) = world().await;
    let key = w.svc.add_ssh_key(&alice, None, ED1).await.unwrap();
    assert_eq!(
        (key.title.as_str(), key.algorithm.as_str()),
        ("alice@laptop", "ssh-ed25519"),
        "title comes from the comment"
    );
    let named = w
        .svc
        .add_ssh_key(&alice, Some("  work machine "), EC)
        .await
        .unwrap();
    assert_eq!(named.title, "work machine");

    let keys = w
        .svc
        .list_ssh_keys(&alice, &UserId::new("alice"))
        .await
        .unwrap();
    assert_eq!(keys.len(), 2);
    w.svc
        .delete_ssh_key(&alice, &UserId::new("alice"), &key.id)
        .await
        .unwrap();
    assert_eq!(
        w.svc
            .list_ssh_keys(&alice, &UserId::new("alice"))
            .await
            .unwrap()
            .len(),
        1
    );
    let err = w
        .svc
        .delete_ssh_key(&alice, &UserId::new("alice"), &key.id)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::KeyNotFound(_)), "{err}");
}

#[tokio::test]
async fn a_key_belongs_to_one_account_only() {
    let (w, root, alice) = world().await;
    w.svc.add_ssh_key(&alice, None, ED1).await.unwrap();
    let err = w.svc.add_ssh_key(&root, None, ED1).await.unwrap_err();
    assert!(matches!(err, PynError::KeyInUse), "another account: {err}");
    let err = w.svc.add_ssh_key(&alice, None, ED1).await.unwrap_err();
    assert!(matches!(err, PynError::KeyInUse), "the same account: {err}");
    w.svc.add_ssh_key(&root, None, ED2).await.unwrap();
}

#[tokio::test]
async fn other_peoples_keys_are_only_for_administrators() {
    let (w, root, alice) = world().await;
    let key = w.svc.add_ssh_key(&alice, None, ED1).await.unwrap();
    let err = w.svc.list_ssh_keys(&root, &UserId::new("alice")).await;
    assert!(err.is_ok(), "an administrator may look");
    let root_principal = w.svc.principal_in(&w.repo, &root).await.unwrap();
    let bob = w
        .svc
        .add_user(
            &root_principal,
            &w.repo,
            "bob",
            "another long password",
            Role::Reader,
        )
        .await
        .unwrap();
    let bob = session(bob.as_str());
    assert!(matches!(
        w.svc.list_ssh_keys(&bob, &UserId::new("alice")).await,
        Err(PynError::Forbidden(Permission::ManageUsers))
    ));
    assert!(matches!(
        w.svc
            .delete_ssh_key(&bob, &UserId::new("alice"), &key.id)
            .await,
        Err(PynError::Forbidden(Permission::ManageUsers))
    ));
    w.svc
        .delete_ssh_key(&root, &UserId::new("alice"), &key.id)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_key_authenticates_as_its_owner_with_their_current_role() {
    let (w, root, alice) = world().await;
    w.svc.add_ssh_key(&alice, None, ED1).await.unwrap();
    let fp = ssh::parse(ED1).unwrap().fingerprint;

    let who = w.svc.authenticate_ssh_key(&w.repo, &fp).await.unwrap();
    assert_eq!(who.user, UserId::new("alice"));
    assert!(who.has(Permission::Checkin) && !who.has(Permission::Restore));

    let root = w.svc.principal_in(&w.repo, &root).await.unwrap();
    w.svc
        .set_user_role(&root, &w.repo, &UserId::new("alice"), Role::Reader)
        .await
        .unwrap();
    assert!(
        !w.svc
            .authenticate_ssh_key(&w.repo, &fp)
            .await
            .unwrap()
            .has(Permission::Checkin),
        "the role is looked up each time"
    );

    let err = w
        .svc
        .authenticate_ssh_key(&w.repo, &ssh::parse(ED2).unwrap().fingerprint)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
}

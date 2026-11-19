//! Second factors (#1104): factors, recovery codes, the attempt delay, fresh checks and their cleanup.

use super::{Access, at, new_account, user};
use crate::access::SignupStage;
use crate::access::{AccessMode, ServerSettings};
use crate::account_self::RenameOutcome;
use crate::deletion::AccountPurge;
use crate::two_factor::{
    ChallengePurpose, ChallengeRecord, FREE_ATTEMPTS, FactorKind, FactorRecord, FactorRemoval,
    NoticeRecord, PasskeyRecord, TwoFactorPolicy,
};
use crate::{NewSecret, PynError, UserId};
use chrono::Duration;

async fn active(store: &Access, name: &str) {
    store
        .create_account(new_account(name, &format!("{name}@example.org"), 0))
        .await
        .unwrap();
    assert!(
        store
            .complete_verification(&user(name), SignupStage::Complete, at(0))
            .await
            .unwrap()
    );
}

fn factor(id: &str, owner: &str, minute: i64) -> FactorRecord {
    FactorRecord {
        id: id.into(),
        user: user(owner),
        kind: FactorKind::Totp,
        label: format!("{id} label"),
        secret: id.as_bytes().to_vec(),
        encrypted: false,
        key_id: None,
        created_at: at(minute),
        confirmed_at: None,
        last_step: None,
    }
}

async fn enrolled(store: &Access, id: &str, owner: &str, minute: i64) {
    store
        .create_factor(factor(id, owner, minute))
        .await
        .unwrap();
    assert!(
        store
            .confirm_factor(&user(owner), id, at(minute))
            .await
            .unwrap()
    );
}

pub async fn a_factor_is_pending_until_confirmed_and_an_account_keeps_one_pending(store: Access) {
    active(&store, "alice").await;
    active(&store, "bob").await;
    let (alice, bob) = (user("alice"), user("bob"));

    store.create_factor(factor("a1", "alice", 1)).await.unwrap();
    store.create_factor(factor("a2", "alice", 2)).await.unwrap();
    let held = store.factors_of(&alice).await.unwrap();
    assert_eq!(held.len(), 1, "a new pending factor replaces the old one");
    assert_eq!((held[0].id.as_str(), held[0].confirmed_at), ("a2", None));

    assert!(!store.confirm_factor(&bob, "a2", at(3)).await.unwrap());
    assert!(!store.confirm_factor(&alice, "a1", at(3)).await.unwrap());
    assert!(store.confirm_factor(&alice, "a2", at(3)).await.unwrap());
    assert!(
        !store.confirm_factor(&alice, "a2", at(4)).await.unwrap(),
        "only a pending factor is confirmed"
    );

    store.create_factor(factor("a3", "alice", 5)).await.unwrap();
    store.create_factor(factor("b1", "bob", 5)).await.unwrap();
    let held = store.factors_of(&alice).await.unwrap();
    assert_eq!(
        held.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
        ["a2", "a3"],
        "a pending one sits beside the confirmed one, oldest first"
    );
    assert_eq!(held[0].confirmed_at, Some(at(3)));
    assert_eq!(held[0].secret, b"a2");
    assert_eq!(store.factors_of(&bob).await.unwrap().len(), 1);
}

pub async fn removing_the_last_confirmed_factor_takes_the_accounts_other_second_factor_state(
    store: Access,
) {
    active(&store, "alice").await;
    let alice = user("alice");
    enrolled(&store, "a1", "alice", 1).await;
    enrolled(&store, "a2", "alice", 2).await;
    store
        .replace_recovery_codes(&alice, &["h1".into(), "h2".into()], at(3))
        .await
        .unwrap();
    for _ in 0..FREE_ATTEMPTS {
        let _ = store.count_factor_attempt(&alice, at(4)).await.unwrap();
    }
    store
        .mark_fresh("session:x", &alice, at(30), at(4))
        .await
        .unwrap();

    assert_eq!(
        store.remove_factor(&alice, "nope").await.unwrap(),
        FactorRemoval::NotFound
    );
    assert_eq!(
        store.remove_factor(&user("bob"), "a1").await.unwrap(),
        FactorRemoval::NotFound
    );
    assert_eq!(
        store.remove_factor(&alice, "a1").await.unwrap(),
        FactorRemoval::Removed {
            second_factor_left: true
        }
    );
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 2);
    assert_eq!(store.fresh_until("session:x").await.unwrap(), Some(at(30)));

    assert_eq!(
        store.remove_factor(&alice, "a2").await.unwrap(),
        FactorRemoval::Removed {
            second_factor_left: false
        }
    );
    assert!(store.factors_of(&alice).await.unwrap().is_empty());
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 0);
    assert_eq!(store.fresh_until("session:x").await.unwrap(), None);
    assert_eq!(
        store.count_factor_attempt(&alice, at(5)).await.unwrap(),
        Ok(()),
        "the attempt delay went with the factors"
    );
}

pub async fn a_time_step_is_accepted_once_and_only_forward(store: Access) {
    active(&store, "alice").await;
    let alice = user("alice");
    enrolled(&store, "a1", "alice", 1).await;
    assert!(store.accept_factor_step(&alice, "a1", 100).await.unwrap());
    assert!(!store.accept_factor_step(&alice, "a1", 100).await.unwrap());
    assert!(!store.accept_factor_step(&alice, "a1", 99).await.unwrap());
    assert!(store.accept_factor_step(&alice, "a1", 101).await.unwrap());
    assert!(
        !store
            .accept_factor_step(&user("bob"), "a1", 500)
            .await
            .unwrap()
    );
    assert!(!store.accept_factor_step(&alice, "zz", 500).await.unwrap());
    assert_eq!(
        store.factors_of(&alice).await.unwrap()[0].last_step,
        Some(101)
    );
}

pub async fn recovery_codes_are_replaced_as_a_set_and_each_is_spent_once(store: Access) {
    active(&store, "alice").await;
    active(&store, "bob").await;
    let (alice, bob) = (user("alice"), user("bob"));
    store
        .replace_recovery_codes(&alice, &["h1".into(), "h2".into(), "h3".into()], at(1))
        .await
        .unwrap();
    store
        .replace_recovery_codes(&bob, &["h1".into()], at(1))
        .await
        .unwrap();
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 3);

    assert!(store.use_recovery_code(&alice, "h2", at(2)).await.unwrap());
    assert!(!store.use_recovery_code(&alice, "h2", at(3)).await.unwrap());
    assert!(
        !store
            .use_recovery_code(&alice, "nope", at(3))
            .await
            .unwrap()
    );
    assert!(
        store.use_recovery_code(&bob, "h1", at(3)).await.unwrap(),
        "the same hash under another account is a different code"
    );
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 2);

    store
        .replace_recovery_codes(&alice, &["n1".into()], at(4))
        .await
        .unwrap();
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 1);
    assert!(!store.use_recovery_code(&alice, "h1", at(5)).await.unwrap());
    assert!(store.use_recovery_code(&alice, "n1", at(5)).await.unwrap());
}

pub async fn the_delay_after_failed_attempts_grows_clears_on_success_and_is_per_account(
    store: Access,
) {
    active(&store, "alice").await;
    active(&store, "bob").await;
    let (alice, bob) = (user("alice"), user("bob"));
    let now = at(0);
    for n in 1..FREE_ATTEMPTS {
        assert_eq!(
            store.count_factor_attempt(&alice, now).await.unwrap(),
            Ok(()),
            "attempt {n}"
        );
    }
    assert_eq!(
        store.count_factor_attempt(&alice, now).await.unwrap(),
        Ok(()),
        "the attempt that starts the delay is still checked"
    );
    let first = now + Duration::seconds(30);
    assert_eq!(
        store.count_factor_attempt(&alice, now).await.unwrap(),
        Err(first)
    );
    assert_eq!(
        store
            .count_factor_attempt(&alice, now + Duration::seconds(29))
            .await
            .unwrap(),
        Err(first),
        "a refused attempt does not extend the delay"
    );
    assert_eq!(store.count_factor_attempt(&bob, now).await.unwrap(), Ok(()));
    assert_eq!(
        store.count_factor_attempt(&alice, first).await.unwrap(),
        Ok(())
    );
    assert_eq!(
        store.count_factor_attempt(&alice, first).await.unwrap(),
        Err(first + Duration::seconds(60)),
        "the next delay doubles"
    );

    store.clear_factor_attempts(&alice).await.unwrap();
    assert_eq!(
        store.count_factor_attempt(&alice, first).await.unwrap(),
        Ok(())
    );
}

pub async fn a_fresh_check_belongs_to_one_credential_and_expired_ones_are_swept(store: Access) {
    active(&store, "alice").await;
    let alice = user("alice");
    assert_eq!(store.fresh_until("session:a").await.unwrap(), None);
    store
        .mark_fresh("session:a", &alice, at(10), at(0))
        .await
        .unwrap();
    store
        .mark_fresh("token:b", &alice, at(20), at(0))
        .await
        .unwrap();
    assert_eq!(store.fresh_until("session:a").await.unwrap(), Some(at(10)));
    assert_eq!(store.fresh_until("token:b").await.unwrap(), Some(at(20)));
    store
        .mark_fresh("session:a", &alice, at(40), at(5))
        .await
        .unwrap();
    assert_eq!(store.fresh_until("session:a").await.unwrap(), Some(at(40)));
    store
        .mark_fresh("token:c", &alice, at(50), at(30))
        .await
        .unwrap();
    assert_eq!(
        store.fresh_until("token:b").await.unwrap(),
        None,
        "a later mark sweeps the checks that ended"
    );
    assert_eq!(store.fresh_until("session:a").await.unwrap(), Some(at(40)));
}

pub async fn clearing_two_factor_removes_everything_and_counts_the_confirmed_factors(
    store: Access,
) {
    active(&store, "alice").await;
    active(&store, "bob").await;
    let (alice, bob) = (user("alice"), user("bob"));
    enrolled(&store, "a1", "alice", 1).await;
    store.create_factor(factor("a2", "alice", 2)).await.unwrap();
    enrolled(&store, "b1", "bob", 1).await;
    store
        .replace_recovery_codes(&alice, &["h".into()], at(1))
        .await
        .unwrap();
    store
        .mark_fresh("session:a", &alice, at(30), at(1))
        .await
        .unwrap();
    store
        .mark_fresh("session:b", &bob, at(30), at(1))
        .await
        .unwrap();
    for _ in 0..FREE_ATTEMPTS {
        let _ = store.count_factor_attempt(&alice, at(2)).await.unwrap();
    }

    assert_eq!(store.clear_two_factor(&alice).await.unwrap(), 1);
    assert!(store.factors_of(&alice).await.unwrap().is_empty());
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 0);
    assert_eq!(store.fresh_until("session:a").await.unwrap(), None);
    assert_eq!(
        store.count_factor_attempt(&alice, at(2)).await.unwrap(),
        Ok(())
    );
    assert_eq!(store.factors_of(&bob).await.unwrap().len(), 1);
    assert_eq!(store.fresh_until("session:b").await.unwrap(), Some(at(30)));
    assert_eq!(store.clear_two_factor(&alice).await.unwrap(), 0);
}

pub async fn resealing_rewrites_every_seed_or_none(store: Access) {
    active(&store, "alice").await;
    active(&store, "bob").await;
    enrolled(&store, "a1", "alice", 1).await;
    enrolled(&store, "b1", "bob", 1).await;

    let rewritten = store
        .reseal_factor_seeds(&|row| {
            Ok(NewSecret {
                secret: [row.secret.clone(), b"!".to_vec()].concat(),
                encrypted: true,
                key_id: Some("k2".into()),
            })
        })
        .await
        .unwrap();
    assert_eq!(rewritten, 2);
    let a = &store.factors_of(&user("alice")).await.unwrap()[0];
    assert_eq!(a.secret, b"a1!");
    assert!(a.encrypted);
    assert_eq!(a.key_id.as_deref(), Some("k2"));

    let failed = store
        .reseal_factor_seeds(&|row| {
            if row.user == UserId::new("bob") {
                return Err(PynError::Storage("cannot".into()));
            }
            Ok(NewSecret {
                secret: b"changed".to_vec(),
                encrypted: false,
                key_id: None,
            })
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        store.factors_of(&user("alice")).await.unwrap()[0].secret,
        b"a1!",
        "a failure leaves every row as it was"
    );
}

pub async fn a_rename_keeps_the_factors_and_a_purge_removes_them(store: Access) {
    active(&store, "alice").await;
    enrolled(&store, "a1", "alice", 1).await;
    store
        .replace_recovery_codes(&user("alice"), &["h".into()], at(1))
        .await
        .unwrap();
    store
        .mark_fresh("session:a", &user("alice"), at(30), at(1))
        .await
        .unwrap();
    store
        .count_factor_attempt(&user("alice"), at(1))
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(
        store
            .rename_account(&user("alice"), &user("alicia"), at(5), &[])
            .await
            .unwrap(),
        RenameOutcome::Renamed { .. }
    ));
    assert!(store.factors_of(&user("alice")).await.unwrap().is_empty());
    assert_eq!(store.factors_of(&user("alicia")).await.unwrap().len(), 1);
    assert_eq!(
        store.unused_recovery_codes(&user("alicia")).await.unwrap(),
        1
    );
    store.remove_factor(&user("alicia"), "a1").await.unwrap();

    enrolled(&store, "a2", "alicia", 6).await;
    store
        .set_disabled(&user("alicia"), Some((at(7), None)))
        .await
        .unwrap();
    assert!(matches!(
        store
            .purge_account_with_events(&user("alicia"), at(8), at(9_000), &[])
            .await
            .unwrap(),
        AccountPurge::Purged(_)
    ));
    assert!(store.factors_of(&user("alicia")).await.unwrap().is_empty());
}

fn policy(minute: i64, grace_days: u32) -> TwoFactorPolicy {
    TwoFactorPolicy {
        since: at(minute),
        grace_days,
    }
}

pub async fn the_server_requirement_is_stored_with_the_settings(store: Access) {
    assert!(
        !store
            .set_server_two_factor(Some(policy(1, 7)))
            .await
            .unwrap()
    );
    let mut admin = new_account("root", "root@example.org", 0);
    admin.signup = SignupStage::Complete;
    let settings = ServerSettings {
        initialised_at: at(0),
        server_name: None,
        public_url: None,
        registration: None,
        access_mode: AccessMode::Private,
        two_factor: Some(policy(0, 3)),
    };
    assert!(store.complete_setup(admin, settings).await.unwrap());
    let read = || async { store.server_settings().await.unwrap().unwrap().two_factor };
    assert_eq!(read().await, Some(policy(0, 3)));

    assert!(
        store
            .set_server_two_factor(Some(policy(5, 0)))
            .await
            .unwrap()
    );
    assert_eq!(read().await, Some(policy(5, 0)));
    assert!(store.set_server_two_factor(None).await.unwrap());
    assert_eq!(read().await, None);
}

pub async fn an_organization_requirement_is_its_own_and_reaches_only_its_members(store: Access) {
    for name in ["alice", "bob", "carol"] {
        active(&store, name).await;
    }
    for (org, owner) in [("acme", "alice"), ("globex", "bob")] {
        assert!(
            store
                .create_org(&user(org), &user(owner), at(1))
                .await
                .unwrap()
        );
    }
    assert!(
        store
            .add_org_member(&user("acme"), &user("carol"), crate::OrgRole::Member)
            .await
            .unwrap()
    );
    assert_eq!(store.org_two_factor(&user("acme")).await.unwrap(), None);

    assert_eq!(
        store
            .set_org_two_factor(&user("acme"), Some(policy(2, 7)))
            .await
            .unwrap(),
        None
    );
    store
        .set_org_two_factor(&user("globex"), Some(policy(3, 1)))
        .await
        .unwrap();
    assert_eq!(
        store.org_two_factor(&user("acme")).await.unwrap(),
        Some(policy(2, 7))
    );
    assert_eq!(
        store.member_two_factor(&user("carol")).await.unwrap(),
        vec![(user("acme"), policy(2, 7))]
    );
    assert_eq!(
        store.member_two_factor(&user("alice")).await.unwrap(),
        vec![(user("acme"), policy(2, 7))]
    );
    assert_eq!(
        store.member_two_factor(&user("bob")).await.unwrap(),
        vec![(user("globex"), policy(3, 1))]
    );

    assert_eq!(
        store
            .set_org_two_factor(&user("acme"), Some(policy(4, 30)))
            .await
            .unwrap(),
        Some(policy(2, 7))
    );
    assert_eq!(
        store.set_org_two_factor(&user("acme"), None).await.unwrap(),
        Some(policy(4, 30))
    );
    assert!(
        store
            .member_two_factor(&user("carol"))
            .await
            .unwrap()
            .is_empty()
    );

    assert!(
        store
            .delete_org_with_events(&user("globex"), &[])
            .await
            .unwrap()
    );
    assert_eq!(store.org_two_factor(&user("globex")).await.unwrap(), None);
    assert!(
        store
            .member_two_factor(&user("bob"))
            .await
            .unwrap()
            .is_empty()
    );
}

fn notice(id: &str, owner: &str, minute: i64) -> NoticeRecord {
    NoticeRecord {
        id: id.into(),
        user: user(owner),
        kind: "two_factor_reset".into(),
        detail: format!("{id} detail"),
        created_at: at(minute),
    }
}

pub async fn notices_are_listed_oldest_first_dismissed_once_and_follow_the_account(store: Access) {
    active(&store, "alice").await;
    active(&store, "bob").await;
    store.add_notice(notice("n2", "alice", 2)).await.unwrap();
    store.add_notice(notice("n1", "alice", 1)).await.unwrap();
    store.add_notice(notice("n3", "bob", 1)).await.unwrap();
    let ids = |list: Vec<NoticeRecord>| list.into_iter().map(|n| n.id).collect::<Vec<_>>();
    assert_eq!(
        ids(store.notices_of(&user("alice")).await.unwrap()),
        ["n1", "n2"]
    );

    assert!(!store.dismiss_notice(&user("bob"), "n1").await.unwrap());
    assert!(store.dismiss_notice(&user("alice"), "n1").await.unwrap());
    assert!(!store.dismiss_notice(&user("alice"), "n1").await.unwrap());

    store.clear_two_factor(&user("alice")).await.unwrap();
    assert_eq!(
        ids(store.notices_of(&user("alice")).await.unwrap()),
        ["n2"],
        "clearing factors keeps the notice that says so"
    );

    assert!(matches!(
        store
            .rename_account(&user("alice"), &user("alicia"), at(5), &[])
            .await
            .unwrap(),
        RenameOutcome::Renamed { .. }
    ));
    assert!(store.notices_of(&user("alice")).await.unwrap().is_empty());
    assert_eq!(
        ids(store.notices_of(&user("alicia")).await.unwrap()),
        ["n2"]
    );

    store
        .set_disabled(&user("alicia"), Some((at(7), None)))
        .await
        .unwrap();
    assert!(matches!(
        store
            .purge_account_with_events(&user("alicia"), at(8), at(9_000), &[])
            .await
            .unwrap(),
        AccountPurge::Purged(_)
    ));
    assert!(store.notices_of(&user("alicia")).await.unwrap().is_empty());
    assert_eq!(ids(store.notices_of(&user("bob")).await.unwrap()), ["n3"]);
}

fn passkey(id: &str, owner: &str, minute: i64) -> PasskeyRecord {
    PasskeyRecord {
        id: id.into(),
        user: user(owner),
        name: format!("{id} name"),
        credential_id: format!("cred-{id}").into_bytes(),
        public_key: vec![1, 2, 3],
        user_handle: vec![9; 32],
        sign_count: 4,
        transports: vec!["usb".into(), "nfc".into()],
        backup_eligible: true,
        backup_state: false,
        created_at: at(minute),
        last_used_at: None,
    }
}

pub async fn passkeys_are_unique_by_credential_renamed_by_their_owner_and_listed_oldest_first(
    store: Access,
) {
    active(&store, "alice").await;
    active(&store, "bob").await;
    let (alice, bob) = (user("alice"), user("bob"));
    assert!(
        store
            .create_passkey(passkey("p2", "alice", 2))
            .await
            .unwrap()
    );
    assert!(
        store
            .create_passkey(passkey("p1", "alice", 1))
            .await
            .unwrap()
    );
    let mut clash = passkey("p3", "bob", 3);
    clash.credential_id = b"cred-p1".to_vec();
    assert!(!store.create_passkey(clash).await.unwrap());

    let held = store.passkeys_of(&alice).await.unwrap();
    assert_eq!(
        held.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["p1", "p2"]
    );
    assert_eq!(held[0], passkey("p1", "alice", 1));
    assert!(store.passkeys_of(&bob).await.unwrap().is_empty());
    assert_eq!(
        store.passkey_by_credential(b"cred-p2").await.unwrap(),
        Some(passkey("p2", "alice", 2))
    );
    assert_eq!(store.passkey_by_credential(b"nope").await.unwrap(), None);

    assert!(!store.rename_passkey(&bob, "p1", "mine").await.unwrap());
    assert!(!store.rename_passkey(&alice, "zz", "mine").await.unwrap());
    assert!(store.rename_passkey(&alice, "p1", "laptop").await.unwrap());
    assert_eq!(store.passkeys_of(&alice).await.unwrap()[0].name, "laptop");
}

pub async fn a_passkey_use_is_recorded_once_per_counter_value(store: Access) {
    active(&store, "alice").await;
    let alice = user("alice");
    store
        .create_passkey(passkey("p1", "alice", 1))
        .await
        .unwrap();
    assert!(
        !store
            .record_passkey_use(&alice, "p1", 3, 5, true, at(2))
            .await
            .unwrap(),
        "the stored counter is not 3"
    );
    assert!(
        store
            .record_passkey_use(&alice, "p1", 4, 5, true, at(2))
            .await
            .unwrap()
    );
    assert!(
        !store
            .record_passkey_use(&alice, "p1", 4, 6, true, at(3))
            .await
            .unwrap(),
        "a second use that read the old counter loses"
    );
    assert!(
        !store
            .record_passkey_use(&user("bob"), "p1", 5, 6, true, at(3))
            .await
            .unwrap()
    );
    let held = &store.passkeys_of(&alice).await.unwrap()[0];
    assert_eq!(
        (held.sign_count, held.backup_state, held.last_used_at),
        (5, true, Some(at(2)))
    );
}

pub async fn a_passkey_counts_as_a_second_factor_when_the_last_one_is_removed(store: Access) {
    active(&store, "alice").await;
    let alice = user("alice");
    enrolled(&store, "f1", "alice", 1).await;
    store
        .create_passkey(passkey("p1", "alice", 2))
        .await
        .unwrap();
    store
        .replace_recovery_codes(&alice, &["h1".into()], at(3))
        .await
        .unwrap();

    assert_eq!(
        store.remove_passkey(&alice, "zz").await.unwrap(),
        FactorRemoval::NotFound
    );
    assert_eq!(
        store.remove_factor(&alice, "f1").await.unwrap(),
        FactorRemoval::Removed {
            second_factor_left: true
        },
        "the passkey keeps two-factor authentication on"
    );
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 1);
    assert_eq!(
        store.remove_passkey(&alice, "p1").await.unwrap(),
        FactorRemoval::Removed {
            second_factor_left: false
        }
    );
    assert_eq!(store.unused_recovery_codes(&alice).await.unwrap(), 0);

    store
        .create_passkey(passkey("p2", "alice", 4))
        .await
        .unwrap();
    enrolled(&store, "f2", "alice", 5).await;
    assert_eq!(
        store.remove_passkey(&alice, "p2").await.unwrap(),
        FactorRemoval::Removed {
            second_factor_left: true
        }
    );
}

pub async fn clearing_two_factor_counts_and_removes_passkeys_too(store: Access) {
    active(&store, "alice").await;
    let alice = user("alice");
    enrolled(&store, "f1", "alice", 1).await;
    store
        .create_passkey(passkey("p1", "alice", 2))
        .await
        .unwrap();
    store
        .create_passkey(passkey("p2", "alice", 3))
        .await
        .unwrap();
    assert_eq!(store.clear_two_factor(&alice).await.unwrap(), 3);
    assert!(store.passkeys_of(&alice).await.unwrap().is_empty());
    assert_eq!(store.passkey_by_credential(b"cred-p1").await.unwrap(), None);
}

fn challenge(
    id: &str,
    owner: Option<&str>,
    purpose: ChallengePurpose,
    until: i64,
) -> ChallengeRecord {
    ChallengeRecord {
        id: id.into(),
        user: owner.map(user),
        purpose,
        challenge: id.as_bytes().to_vec(),
        expires_at: at(until),
    }
}

pub async fn a_challenge_is_taken_once_expires_and_is_replaced_per_account_and_purpose(
    store: Access,
) {
    active(&store, "alice").await;
    let reg = |id, until| challenge(id, Some("alice"), ChallengePurpose::Register, until);

    store.put_challenge(reg("c1", 10), at(1)).await.unwrap();
    store.put_challenge(reg("c2", 10), at(2)).await.unwrap();
    assert_eq!(
        store.take_challenge("c1", at(3)).await.unwrap(),
        None,
        "replaced"
    );
    assert_eq!(
        store.take_challenge("c2", at(3)).await.unwrap(),
        Some(reg("c2", 10))
    );
    assert_eq!(
        store.take_challenge("c2", at(3)).await.unwrap(),
        None,
        "single use"
    );

    store
        .put_challenge(
            challenge("v1", Some("alice"), ChallengePurpose::Verify, 10),
            at(3),
        )
        .await
        .unwrap();
    store.put_challenge(reg("c3", 10), at(3)).await.unwrap();
    assert!(
        store.take_challenge("v1", at(4)).await.unwrap().is_some(),
        "another purpose is kept"
    );

    store.put_challenge(reg("c4", 5), at(4)).await.unwrap();
    assert_eq!(
        store.take_challenge("c4", at(6)).await.unwrap(),
        None,
        "expired"
    );
    assert_eq!(
        store.take_challenge("c4", at(6)).await.unwrap(),
        None,
        "and gone"
    );

    let sign_in = |id| challenge(id, None, ChallengePurpose::SignIn, 10);
    store.put_challenge(sign_in("s1"), at(4)).await.unwrap();
    store.put_challenge(sign_in("s2"), at(4)).await.unwrap();
    assert!(store.take_challenge("s1", at(5)).await.unwrap().is_some());
    assert!(store.take_challenge("s2", at(5)).await.unwrap().is_some());

    store.put_challenge(reg("c5", 20), at(5)).await.unwrap();
    store.put_challenge(sign_in("s3"), at(30)).await.unwrap();
    assert_eq!(
        store.take_challenge("c5", at(6)).await.unwrap(),
        None,
        "swept by a later put"
    );
}

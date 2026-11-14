//! Account deletion (decision #976) at the store: scheduling and restoring, the lists the timer reads, the purge and
//! the name reservation, plus the history and audit lookups that decide how long a name stays reserved.

use super::{Access, Audit, Store, at, blob, harness, new_account, ssh_key, token, user, who};
use crate::access::{
    InviteId, InviteRecord, OrgInvitation, OrgRole, Role, SessionRecord, SignupStage,
};
use crate::repo_policy::{CreationEffect, CreationRule, CreationScope, CreationSubject};
use crate::{
    AccountDeletion, AccountPurge, AuditAction, AuditScope, DeletionSchedule, Limits, LimitsChange,
    RepoId, RepoPath,
};

fn deletion(name: &str, by_owner: bool, scheduled: i64, purge: i64) -> AccountDeletion {
    AccountDeletion {
        user: user(name),
        requested_by: user(if by_owner { name } else { "root" }),
        by_owner,
        reason: (!by_owner).then(|| "inactive".to_string()),
        scheduled_at: at(scheduled),
        purge_at: at(purge),
    }
}

async fn add(store: &Access, name: &str, minute: i64) {
    let mut account = new_account(name, &format!("{name}@example.org"), minute);
    account.signup = SignupStage::Complete;
    assert!(store.create_account(account).await.unwrap());
}

pub async fn a_deletion_is_scheduled_with_the_disable_and_restored_whole(store: Access) {
    add(&store, "alice", 0).await;
    add(&store, "bob", 0).await;
    assert_eq!(store.account_deletion(&user("alice")).await.unwrap(), None);

    let first = deletion("alice", true, 5, 100);
    assert_eq!(
        store
            .schedule_account_deletion_with_events(first.clone(), true, &[])
            .await
            .unwrap(),
        DeletionSchedule::Scheduled
    );
    assert_eq!(
        store.account_deletion(&user("alice")).await.unwrap(),
        Some(first)
    );
    let alice = store.account(&user("alice")).await.unwrap().unwrap();
    assert_eq!(alice.disabled_at, Some(at(5)));

    let later = deletion("alice", false, 6, 200);
    store
        .schedule_account_deletion_with_events(later.clone(), false, &[])
        .await
        .unwrap();
    assert_eq!(
        store.account_deletion(&user("alice")).await.unwrap(),
        Some(later),
        "scheduling again replaces the schedule"
    );
    assert_eq!(
        store
            .account(&user("alice"))
            .await
            .unwrap()
            .unwrap()
            .disabled_at,
        Some(at(5)),
        "without disable the account's own disable stays as it was"
    );

    assert!(
        store
            .restore_account_with_events(&user("alice"), &[])
            .await
            .unwrap()
    );
    assert_eq!(store.account_deletion(&user("alice")).await.unwrap(), None);
    let alice = store.account(&user("alice")).await.unwrap().unwrap();
    assert!(alice.disabled_at.is_none() && alice.disabled_reason.is_none());
    assert!(
        !store
            .restore_account_with_events(&user("alice"), &[])
            .await
            .unwrap(),
        "nothing left to restore"
    );
    assert!(
        !store
            .restore_account_with_events(&user("bob"), &[])
            .await
            .unwrap()
    );

    assert_eq!(
        store
            .schedule_account_deletion_with_events(deletion("nobody", true, 1, 2), true, &[])
            .await
            .unwrap(),
        DeletionSchedule::NoAccount
    );
    store
        .schedule_account_deletion_with_events(deletion("bob", true, 1, 2), false, &[])
        .await
        .unwrap();
    assert!(store.clear_account_deletion(&user("bob")).await.unwrap());
    assert!(!store.clear_account_deletion(&user("bob")).await.unwrap());
}

pub async fn the_only_active_administrator_cannot_schedule_their_own_deletion(store: Access) {
    add(&store, "root", 0).await;
    store.set_admin(&user("root"), true).await.unwrap();
    assert_eq!(
        store
            .schedule_account_deletion_with_events(deletion("root", true, 1, 2), true, &[])
            .await
            .unwrap(),
        DeletionSchedule::LastAdmin
    );
    assert_eq!(store.account_deletion(&user("root")).await.unwrap(), None);
    let root = store.account(&user("root")).await.unwrap().unwrap();
    assert!(
        root.disabled_at.is_none(),
        "a refusal leaves the account as it was"
    );

    add(&store, "second", 0).await;
    store.set_admin(&user("second"), true).await.unwrap();
    assert_eq!(
        store
            .schedule_account_deletion_with_events(deletion("root", true, 1, 2), true, &[])
            .await
            .unwrap(),
        DeletionSchedule::Scheduled
    );
}

pub async fn deletions_are_due_earliest_first_and_long_disabled_accounts_wait_for_the_cutoff(
    store: Access,
) {
    for (name, minute) in [("a", 0), ("b", 0), ("c", 0), ("d", 0), ("e", 0), ("f", 0)] {
        add(&store, name, minute).await;
    }
    store
        .schedule_account_deletion_with_events(deletion("b", true, 1, 30), true, &[])
        .await
        .unwrap();
    store
        .schedule_account_deletion_with_events(deletion("a", false, 1, 20), false, &[])
        .await
        .unwrap();
    store
        .schedule_account_deletion_with_events(deletion("c", true, 1, 90), true, &[])
        .await
        .unwrap();

    let names = |list: Vec<AccountDeletion>| -> Vec<String> {
        list.into_iter().map(|d| d.user.to_string()).collect()
    };
    assert!(names(store.deletions_due(at(19), 10).await.unwrap()).is_empty());
    assert_eq!(
        names(store.deletions_due(at(30), 10).await.unwrap()),
        ["a", "b"]
    );
    assert_eq!(names(store.deletions_due(at(99), 1).await.unwrap()), ["a"]);

    store
        .set_disabled(&user("d"), Some((at(10), None)))
        .await
        .unwrap();
    store
        .set_disabled(&user("e"), Some((at(5), None)))
        .await
        .unwrap();
    store
        .set_disabled(&user("f"), Some((at(50), None)))
        .await
        .unwrap();
    let before = |cutoff: i64| {
        let store = store.clone();
        async move {
            store
                .disabled_accounts_before(at(cutoff), 10)
                .await
                .unwrap()
                .into_iter()
                .map(|a| a.user.to_string())
                .collect::<Vec<_>>()
        }
    };
    assert!(before(4).await.is_empty());
    assert_eq!(before(10).await, ["e", "d"], "longest disabled first");
    assert_eq!(
        before(60).await,
        ["e", "d", "f"],
        "scheduled accounts are left to their schedule, enabled ones are not listed"
    );
    assert_eq!(
        store
            .disabled_accounts_before(at(60), 2)
            .await
            .unwrap()
            .len(),
        2
    );
}

pub async fn purging_an_account_removes_its_access_and_reserves_its_name(store: Access) {
    let (alice, bob, acme, other) = (user("alice"), user("bob"), user("acme"), user("other"));
    add(&store, "alice", 0).await;
    add(&store, "bob", 0).await;
    store.set_password_hash(&alice, "hash-alice").await.unwrap();
    store
        .complete_verification(&alice, SignupStage::Complete, at(1))
        .await
        .unwrap();
    store.create_token(token("t1", "alice", 1)).await.unwrap();
    store.create_token(token("t2", "bob", 1)).await.unwrap();
    store
        .create_session(SessionRecord {
            id_hash: "s-alice".into(),
            user: alice.clone(),
            csrf_token: "c".into(),
            created_at: at(1),
            expires_at: at(500),
        })
        .await
        .unwrap();
    assert!(
        store
            .add_ssh_key(ssh_key("k1", "alice", "SHA256:alice", 1))
            .await
            .unwrap()
    );
    assert!(
        store
            .link_external_identity(crate::ExternalIdentity {
                id: "i1".into(),
                user: alice.clone(),
                issuer: "https://idp.example".into(),
                subject: "alice-subject".into(),
                email: None,
                created_at: at(1),
                last_sign_in_at: None,
            })
            .await
            .unwrap()
    );
    store
        .create_invite(InviteRecord {
            id: InviteId("aaaaaaaaaaaa".into()),
            secret_hash: "11".repeat(32),
            repo: RepoId::new("game"),
            role: Role::Writer,
            created_by: alice.clone(),
            created_at: at(1),
            expires_at: at(600),
            used_at: None,
            used_by: None,
            revoked_at: None,
        })
        .await
        .unwrap();
    store
        .set_role(&RepoId::new("game"), &alice, Role::Writer)
        .await
        .unwrap();
    store
        .set_role(&RepoId::new("game"), &bob, Role::Writer)
        .await
        .unwrap();
    store
        .update_owner_limits(
            &alice,
            LimitsChange {
                repositories: Some(Some(3)),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert!(store.create_org(&acme, &bob, at(2)).await.unwrap());
    assert!(
        store
            .add_org_member(&acme, &alice, OrgRole::Owner)
            .await
            .unwrap()
    );
    store
        .create_team(crate::TeamRecord {
            id: crate::TeamId::generate().unwrap(),
            org: acme.clone(),
            slug: "devs".into(),
            name: "Devs".into(),
            description: String::new(),
            created_at: at(2),
        })
        .await
        .unwrap();
    store.add_team_member(&acme, "devs", &alice).await.unwrap();
    store.add_team_member(&acme, "devs", &bob).await.unwrap();
    store
        .set_creation_rule(
            &acme,
            &CreationRule {
                subject: CreationSubject::User(alice.clone()),
                effect: CreationEffect::Allow,
                scope: CreationScope::Both,
            },
        )
        .await
        .unwrap();
    assert!(store.create_org(&other, &bob, at(2)).await.unwrap());
    store
        .create_org_invitation(&OrgInvitation {
            org: other.clone(),
            user: alice.clone(),
            role: OrgRole::Member,
            invited_by: bob.clone(),
            created_at: at(3),
            expires_at: at(900),
        })
        .await
        .unwrap();

    assert_eq!(
        store
            .purge_account_with_events(&alice, at(10), at(10_000), &[])
            .await
            .unwrap(),
        AccountPurge::NotDisabled,
        "an enabled account is not purged"
    );
    assert_eq!(
        store
            .purge_account_with_events(&user("nobody"), at(10), at(10_000), &[])
            .await
            .unwrap(),
        AccountPurge::NoAccount
    );
    assert_eq!(
        store
            .purge_account_with_events(&acme, at(10), at(10_000), &[])
            .await
            .unwrap(),
        AccountPurge::NoAccount,
        "an organization is not purged as a user"
    );
    assert!(store.account(&alice).await.unwrap().is_some());

    store
        .schedule_account_deletion_with_events(deletion("alice", true, 5, 6), true, &[])
        .await
        .unwrap();
    let AccountPurge::Purged(record) = store
        .purge_account_with_events(&alice, at(10), at(10_000), &[])
        .await
        .unwrap()
    else {
        panic!("a disabled account is purged");
    };
    assert_eq!(record.user, alice);

    assert!(store.account(&alice).await.unwrap().is_none());
    assert!(!store.user_exists(&alice).await.unwrap());
    assert_eq!(store.password_hash(&alice).await.unwrap(), None);
    assert_eq!(
        store
            .verified_email_owner("alice@example.org")
            .await
            .unwrap(),
        None
    );
    assert!(store.list_tokens(&alice).await.unwrap().is_empty());
    assert!(store.get_session("s-alice").await.unwrap().is_none());
    assert!(store.list_ssh_keys(&alice).await.unwrap().is_empty());
    assert!(store.find_ssh_key("SHA256:alice").await.unwrap().is_none());
    assert!(
        store
            .list_external_identities(&alice)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .find_external_identity("https://idp.example", "alice-subject")
            .await
            .unwrap()
            .is_none(),
        "the provider subject is free to link again"
    );
    assert!(
        store
            .get_invite(&InviteId("aaaaaaaaaaaa".into()))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.role_of(&RepoId::new("game"), &alice).await.unwrap(),
        None
    );
    assert!(store.orgs_of(&alice).await.unwrap().is_empty());
    assert!(store.org_invitations_for(&alice).await.unwrap().is_empty());
    assert_eq!(
        store.teams_of(&acme, &alice).await.unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        store.team_members(&acme, "devs").await.unwrap(),
        std::slice::from_ref(&bob)
    );
    assert_eq!(store.repo_policy(&acme).await.unwrap().rules.len(), 0);
    assert_eq!(store.owner_limits(&alice).await.unwrap(), Limits::default());
    assert_eq!(store.account_deletion(&alice).await.unwrap(), None);

    assert_eq!(
        store.role_of(&RepoId::new("game"), &bob).await.unwrap(),
        Some(Role::Writer)
    );
    assert_eq!(store.list_tokens(&bob).await.unwrap().len(), 1);
    assert_eq!(
        store.org_role(&acme, &bob).await.unwrap(),
        Some(OrgRole::Owner)
    );
    assert!(store.account(&bob).await.unwrap().is_some());

    let reservation = store.name_reservation("alice").await.unwrap().unwrap();
    assert_eq!(reservation.name, "alice");
    assert_eq!(reservation.reserved_at, at(10));
    assert_eq!(reservation.until, Some(at(10_000)));
}

pub async fn name_reservations_expire_become_permanent_and_release_ignoring_case(store: Access) {
    for (name, purged, until) in [("early", 1, 100), ("late", 2, 500), ("kept", 3, 50)] {
        add(&store, name, 0).await;
        store
            .set_disabled(&user(name), Some((at(0), None)))
            .await
            .unwrap();
        store
            .purge_account_with_events(&user(name), at(purged), at(until), &[])
            .await
            .unwrap();
    }
    assert!(store.name_reservation("nobody").await.unwrap().is_none());
    let early = store.name_reservation("EARLY").await.unwrap().unwrap();
    assert_eq!(early.name, "early");
    assert!(early.holds_at(at(99)) && !early.holds_at(at(100)));

    let expired = |now: i64| {
        let store = store.clone();
        async move {
            store
                .expired_name_reservations(at(now), 10)
                .await
                .unwrap()
                .into_iter()
                .map(|r| r.name)
                .collect::<Vec<_>>()
        }
    };
    assert!(expired(49).await.is_empty());
    assert_eq!(
        expired(100).await,
        ["kept", "early"],
        "earliest expiry first"
    );

    assert!(store.make_name_reservation_permanent("Kept").await.unwrap());
    assert!(
        !store
            .make_name_reservation_permanent("nobody")
            .await
            .unwrap()
    );
    let kept = store.name_reservation("kept").await.unwrap().unwrap();
    assert_eq!(kept.until, None);
    assert!(kept.holds_at(at(1_000_000)));
    assert_eq!(
        expired(100).await,
        ["early"],
        "a permanent reservation never expires"
    );

    assert!(store.release_name_reservation("EARLY").await.unwrap());
    assert!(!store.release_name_reservation("early").await.unwrap());
    assert!(store.name_reservation("early").await.unwrap().is_none());
    assert!(store.name_reservation("late").await.unwrap().is_some());
}

pub async fn a_name_appears_in_history_once_a_revision_carries_it(store: Store) {
    let h = harness(store);
    assert!(!h.store.author_appears(&user("alice")).await.unwrap());
    let c = blob(&h, "x").await;
    h.svc
        .checkin(
            &who("alice"),
            &RepoPath::new("Source/a.cpp").unwrap(),
            c,
            None,
            "first".into(),
        )
        .await
        .unwrap();
    assert!(h.store.author_appears(&user("alice")).await.unwrap());
    assert!(h.store.author_appears(&user("ALICE")).await.unwrap());
    assert!(!h.store.author_appears(&user("bob")).await.unwrap());
}

pub async fn a_name_appears_in_the_audit_logs_once_it_acted(store: Audit) {
    assert!(!store.actor_appears(&user("alice")).await.unwrap());
    store
        .record(
            &AuditScope::Server,
            super::event("alice", AuditAction::AccountDeletionScheduled, None, 1),
        )
        .await
        .unwrap();
    store
        .record(
            &AuditScope::Org(user("acme")),
            super::event("bob", AuditAction::OrgCreated, None, 2),
        )
        .await
        .unwrap();
    assert!(store.actor_appears(&user("alice")).await.unwrap());
    assert!(store.actor_appears(&user("Alice")).await.unwrap());
    assert!(
        store.actor_appears(&user("bob")).await.unwrap(),
        "any scope counts"
    );
    assert!(!store.actor_appears(&user("carol")).await.unwrap());
}

/// Generates the account-deletion access-store cases for the store built by `$factory`.
#[macro_export]
macro_rules! account_deletion_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::account_deletion_contract_tests!(@access $factory; [$(#[$attr])*]; a_deletion_is_scheduled_with_the_disable_and_restored_whole);
        $crate::account_deletion_contract_tests!(@access $factory; [$(#[$attr])*]; the_only_active_administrator_cannot_schedule_their_own_deletion);
        $crate::account_deletion_contract_tests!(@access $factory; [$(#[$attr])*]; deletions_are_due_earliest_first_and_long_disabled_accounts_wait_for_the_cutoff);
        $crate::account_deletion_contract_tests!(@access $factory; [$(#[$attr])*]; purging_an_account_removes_its_access_and_reserves_its_name);
        $crate::account_deletion_contract_tests!(@access $factory; [$(#[$attr])*]; name_reservations_expire_become_permanent_and_release_ignoring_case);
    };
    (@access $factory:expr; [$(#[$attr:meta])*]; $name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn $name() {
            let store: $crate::contract::Access = ($factory).await;
            $crate::contract::$name(store).await;
        }
    };
}

/// Generates the history lookup case for the metadata store built by `$factory`.
#[macro_export]
macro_rules! author_lookup_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn a_name_appears_in_history_once_a_revision_carries_it() {
            let store: $crate::contract::Store = ($factory).await;
            $crate::contract::a_name_appears_in_history_once_a_revision_carries_it(store).await;
        }
    };
}

/// Generates the audit lookup case for the audit store built by `$factory`.
#[macro_export]
macro_rules! actor_lookup_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn a_name_appears_in_the_audit_logs_once_it_acted() {
            let store: $crate::contract::Audit = ($factory).await;
            $crate::contract::a_name_appears_in_the_audit_logs_once_it_acted(store).await;
        }
    };
}

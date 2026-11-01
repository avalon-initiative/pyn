use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Credential,
    Identity, ManualClock, OrgAdd, OrgMemberAdd, OrgRole, PynError, Registration, UserId,
};

const PASSWORD: &str = "correct horse battery";

struct World {
    access: AccessService,
    audit: Arc<MemoryAuditStore>,
    clock: Arc<ManualClock>,
}

async fn world(mode: OrgMemberAdd) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let access = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        clock.clone(),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(AccessConfig {
        registration: pyn_core::RegistrationMode::Open,
        require_email_verification: false,
        org_member_add: mode,
        ..AccessConfig::default()
    })
    .with_registry(Arc::new(MemoryMetadataStore::new()))
    .with_audit(audit.clone());
    for name in ["alice", "bob", "carol"] {
        access
            .register(Registration::new(name, PASSWORD))
            .await
            .unwrap();
    }
    access.create_org(&session("alice"), "acme").await.unwrap();
    World {
        access,
        audit,
        clock,
    }
}

fn user(name: &str) -> UserId {
    UserId::new(name)
}

fn session(name: &str) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Session,
    }
}

fn admin(name: &str) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Unrestricted,
    }
}

async fn actions(w: &World) -> Vec<(String, AuditAction)> {
    let mut log = w
        .audit
        .list(
            &AuditScope::Org(user("acme")),
            &AuditQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    log.reverse();
    log.into_iter()
        .map(|e| (e.actor.to_string(), e.action))
        .skip(1)
        .collect()
}

async fn invite(w: &World, name: &str, role: OrgRole) {
    let added = w
        .access
        .add_org_member(&session("alice"), &user("acme"), &user(name), role)
        .await
        .unwrap();
    assert!(matches!(added, OrgAdd::Invited(_)));
}

#[tokio::test]
async fn adding_a_member_invites_and_accepting_creates_the_membership() {
    let w = world(OrgMemberAdd::Invite).await;
    invite(&w, "bob", OrgRole::Owner).await;
    assert_eq!(
        w.access
            .org_role(&user("acme"), &user("bob"))
            .await
            .unwrap(),
        None
    );

    let pending = w.access.my_org_invitations(&session("bob")).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        (
            pending[0].org.as_str(),
            pending[0].role,
            pending[0].invited_by.as_str()
        ),
        ("acme", OrgRole::Owner, "alice")
    );
    assert_eq!(
        pending[0].expires_at - pending[0].created_at,
        Duration::days(30)
    );
    let listed = w
        .access
        .org_invitations(&session("alice"), &user("acme"))
        .await
        .unwrap();
    assert_eq!(listed, pending);

    let (org, role) = w
        .access
        .accept_org_invitation(&session("bob"), &user("acme"))
        .await
        .unwrap();
    assert_eq!((org.user, role), (user("acme"), OrgRole::Owner));
    assert_eq!(
        w.access
            .org_role(&user("acme"), &user("bob"))
            .await
            .unwrap(),
        Some(OrgRole::Owner)
    );
    assert!(
        w.access
            .my_org_invitations(&session("bob"))
            .await
            .unwrap()
            .is_empty()
    );
    let err = w
        .access
        .accept_org_invitation(&session("bob"), &user("acme"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "org_invitation_not_found");
    assert_eq!(
        actions(&w).await,
        [
            ("alice".into(), AuditAction::OrgMemberInvited),
            ("bob".into(), AuditAction::OrgInvitationAccepted),
        ]
    );
}

#[tokio::test]
async fn an_invitation_can_be_declined_or_cancelled_and_made_again() {
    let w = world(OrgMemberAdd::Invite).await;
    invite(&w, "bob", OrgRole::Member).await;
    w.access
        .decline_org_invitation(&session("bob"), &user("acme"))
        .await
        .unwrap();
    assert_eq!(
        w.access
            .org_role(&user("acme"), &user("bob"))
            .await
            .unwrap(),
        None
    );

    invite(&w, "bob", OrgRole::Member).await;
    let err = w
        .access
        .add_org_member(
            &session("alice"),
            &user("acme"),
            &user("bob"),
            OrgRole::Member,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "already_invited");
    let err = w
        .access
        .cancel_org_invitation(&session("bob"), &user("acme"), &user("bob"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");
    w.access
        .cancel_org_invitation(&session("alice"), &user("acme"), &user("bob"))
        .await
        .unwrap();
    let err = w
        .access
        .cancel_org_invitation(&session("alice"), &user("acme"), &user("bob"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "org_invitation_not_found");
    let err = w
        .access
        .decline_org_invitation(&session("bob"), &user("acme"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "org_invitation_not_found");
    assert_eq!(
        actions(&w)
            .await
            .into_iter()
            .map(|a| a.1)
            .collect::<Vec<_>>(),
        [
            AuditAction::OrgMemberInvited,
            AuditAction::OrgInvitationDeclined,
            AuditAction::OrgMemberInvited,
            AuditAction::OrgInvitationCancelled,
        ]
    );
}

#[tokio::test]
async fn invitation_requests_are_checked() {
    let w = world(OrgMemberAdd::Invite).await;
    let add = |actor: Identity, target: &str| {
        let access = &w.access;
        let target = user(target);
        async move {
            access
                .add_org_member(&actor, &user("acme"), &target, OrgRole::Member)
                .await
                .unwrap_err()
        }
    };
    assert_eq!(
        add(session("alice"), "nobody").await.code(),
        "user_not_found"
    );
    assert_eq!(
        add(session("alice"), "acme").await.code(),
        "invalid_request"
    );
    assert_eq!(
        add(session("alice"), "alice").await.code(),
        "already_org_member"
    );
    assert_eq!(add(session("bob"), "carol").await.code(), "not_org_owner");
    let err = w
        .access
        .org_invitations(&session("bob"), &user("acme"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotOrgOwner(_)), "{err}");
    let err = w
        .access
        .accept_org_invitation(&session("bob"), &user("acme"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "org_invitation_not_found");
    assert!(
        w.access
            .my_org_invitations(&session("bob"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_invitation_expires_after_the_configured_time_and_is_logged() {
    let w = world(OrgMemberAdd::Invite).await;
    invite(&w, "bob", OrgRole::Member).await;
    w.clock.advance(Duration::days(30) - Duration::seconds(1));
    assert_eq!(
        w.access
            .my_org_invitations(&session("bob"))
            .await
            .unwrap()
            .len(),
        1
    );

    w.clock.advance(Duration::seconds(1));
    assert!(
        w.access
            .my_org_invitations(&session("bob"))
            .await
            .unwrap()
            .is_empty()
    );
    let err = w
        .access
        .accept_org_invitation(&session("bob"), &user("acme"))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "org_invitation_not_found");
    assert_eq!(
        actions(&w).await,
        [
            ("alice".into(), AuditAction::OrgMemberInvited),
            ("acme".into(), AuditAction::OrgInvitationExpired),
        ]
    );
    invite(&w, "bob", OrgRole::Member).await;
}

#[tokio::test]
async fn direct_add_is_for_server_administrators_in_direct_mode() {
    let w = world(OrgMemberAdd::Direct).await;
    invite(&w, "bob", OrgRole::Member).await;
    let added = w
        .access
        .add_org_member(
            &admin("alice"),
            &user("acme"),
            &user("carol"),
            OrgRole::Member,
        )
        .await
        .unwrap();
    assert_eq!(added, OrgAdd::Added);
    assert_eq!(
        w.access
            .org_role(&user("acme"), &user("carol"))
            .await
            .unwrap(),
        Some(OrgRole::Member)
    );
    let added = w
        .access
        .add_org_member(&admin("alice"), &user("acme"), &user("bob"), OrgRole::Owner)
        .await
        .unwrap();
    assert_eq!(
        added,
        OrgAdd::Added,
        "a pending invitation gives way to the add"
    );
    assert!(
        w.access
            .my_org_invitations(&session("bob"))
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        actions(&w)
            .await
            .into_iter()
            .map(|a| a.1)
            .collect::<Vec<_>>(),
        [
            AuditAction::OrgMemberInvited,
            AuditAction::OrgMemberAdded,
            AuditAction::OrgMemberAdded,
        ]
    );

    let w = world(OrgMemberAdd::Invite).await;
    let added = w
        .access
        .add_org_member(
            &admin("alice"),
            &user("acme"),
            &user("carol"),
            OrgRole::Member,
        )
        .await
        .unwrap();
    assert!(
        matches!(added, OrgAdd::Invited(_)),
        "invite mode invites everyone"
    );
}

//! Server invitations, accounts an administrator adds, the welcome message and resending a confirmation.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AccountStatus, AuditAction, AuditQuery, AuditScope,
    AuditStore, Credential, EmailMessage, EmailSender, Identity, InvitationGrant, ManualClock,
    MemoryEmailSender, NewServerInvitation, NullEmailSender, OrgRole, PynError, Registration,
    RegistrationMode, RepoId, Role, SignupStage, UserId,
};

const PASSWORD: &str = "correct horse battery";

struct World {
    svc: AccessService,
    store: Arc<MemoryAccessStore>,
    clock: Arc<ManualClock>,
    mail: Arc<MemoryEmailSender>,
    audit: Arc<MemoryAuditStore>,
}

fn world_with(mode: RegistrationMode, config: impl FnOnce(&mut AccessConfig)) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let store = Arc::new(MemoryAccessStore::new());
    let mail = Arc::new(MemoryEmailSender::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let mut cfg = AccessConfig {
        registration: mode,
        public_url: "https://pyn.example/".into(),
        require_email_verification: false,
        ..AccessConfig::default()
    };
    config(&mut cfg);
    let svc = AccessService::new(
        store.clone(),
        clock.clone(),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(cfg)
    .with_email(mail.clone())
    .with_audit(audit.clone());
    World {
        svc,
        store,
        clock,
        mail,
        audit,
    }
}

fn invite_only() -> World {
    world_with(RegistrationMode::InviteOnly, |_| {})
}

fn identity(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

async fn admin(w: &World) -> Identity {
    w.svc.admin_for_tests(&UserId::new("root")).await.unwrap();
    identity("root")
}

async fn server_actions(w: &World) -> Vec<AuditAction> {
    let mut events = w
        .audit
        .list(
            &AuditScope::Server,
            &AuditQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    events.reverse();
    events.into_iter().map(|e| e.action).collect()
}

fn code_in(body: &str) -> String {
    let (_, rest) = body.split_once("Invitation code: ").expect("a code");
    rest.split_whitespace().next().unwrap().to_string()
}

#[tokio::test]
async fn an_invitation_registers_one_account_once() {
    let w = invite_only();
    let root = admin(&w).await;
    let made = w
        .svc
        .create_server_invitation(&root, NewServerInvitation::default())
        .await
        .unwrap();
    assert!(made.code.starts_with("pynv_"));
    assert!(!made.emailed);
    assert_eq!(
        made.invitation.expires_at - made.invitation.created_at,
        Duration::days(7)
    );

    let up = w
        .svc
        .register(Registration::new("alice", PASSWORD).invite(&made.code))
        .await
        .unwrap();
    assert_eq!(up.status, AccountStatus::Active);

    let again = w
        .svc
        .register(Registration::new("bob", PASSWORD).invite(&made.code))
        .await
        .unwrap_err();
    assert!(matches!(again, PynError::InvalidInvite(m) if m.contains("already been used")));

    let listed = w.svc.list_server_invitations(&root).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].used_by, Some(UserId::new("alice")));
    assert_eq!(
        server_actions(&w).await,
        [
            AuditAction::ServerInvitationCreated,
            AuditAction::ServerInvitationUsed
        ]
    );
}

#[tokio::test]
async fn expired_revoked_and_unknown_codes_are_refused() {
    let w = invite_only();
    let root = admin(&w).await;
    let make = |days| {
        w.svc.create_server_invitation(
            &root,
            NewServerInvitation {
                days: Some(days),
                ..Default::default()
            },
        )
    };
    let short = make(1).await.unwrap();
    let revoked = make(30).await.unwrap();
    w.svc
        .revoke_server_invitation(&root, &revoked.invitation.id)
        .await
        .unwrap();
    w.clock.advance(Duration::days(2));

    for (code, why) in [
        (short.code.as_str(), "expired"),
        (revoked.code.as_str(), "revoked"),
        ("pynv_000000000000_nope", "not valid"),
        ("pyni_000000000000_nope", "not valid"),
    ] {
        let err = w
            .svc
            .register(Registration::new("alice", PASSWORD).invite(code))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, PynError::InvalidInvite(m) if m.contains(why)),
            "{code}: {err}"
        );
    }
    let err = w
        .svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidInvite(_)));
    assert!(!w.store.user_exists(&UserId::new("alice")).await.unwrap());
}

#[tokio::test]
async fn expiry_is_bounded_and_only_an_administrator_manages_invitations() {
    let w = invite_only();
    let root = admin(&w).await;
    for days in [0, 91, -1] {
        let err = w
            .svc
            .create_server_invitation(
                &root,
                NewServerInvitation {
                    days: Some(days),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{days}");
    }
    let made = w
        .svc
        .create_server_invitation(
            &root,
            NewServerInvitation {
                days: Some(90),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let nobody = identity("nobody");
    assert!(matches!(
        w.svc
            .create_server_invitation(&nobody, NewServerInvitation::default())
            .await,
        Err(PynError::ServerAdminRequired)
    ));
    assert!(matches!(
        w.svc.list_server_invitations(&nobody).await,
        Err(PynError::ServerAdminRequired)
    ));
    assert!(matches!(
        w.svc
            .revoke_server_invitation(&nobody, &made.invitation.id)
            .await,
        Err(PynError::ServerAdminRequired)
    ));
    let missing = pyn_core::InviteId("ffffffffffff".into());
    assert!(matches!(
        w.svc.revoke_server_invitation(&root, &missing).await,
        Err(PynError::ServerInvitationNotFound(_))
    ));
}

#[tokio::test]
async fn a_used_invitation_cannot_be_revoked() {
    let w = invite_only();
    let root = admin(&w).await;
    let made = w
        .svc
        .create_server_invitation(&root, NewServerInvitation::default())
        .await
        .unwrap();
    w.svc
        .register(Registration::new("alice", PASSWORD).invite(&made.code))
        .await
        .unwrap();
    assert!(matches!(
        w.svc
            .revoke_server_invitation(&root, &made.invitation.id)
            .await,
        Err(PynError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn a_repository_or_organization_role_is_given_at_registration() {
    let w = invite_only();
    let root = admin(&w).await;
    let game = RepoId::new("game");
    let org = w.svc.create_org(&root, "studio").await.unwrap().user;

    let repo = w
        .svc
        .create_server_invitation(
            &root,
            NewServerInvitation {
                grant: Some(InvitationGrant::Repository {
                    repo: game.clone(),
                    role: Role::Writer,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let org_invite = w
        .svc
        .create_server_invitation(
            &root,
            NewServerInvitation {
                grant: Some(InvitationGrant::Organization {
                    org: org.clone(),
                    role: OrgRole::Member,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w.svc
        .register(Registration::new("alice", PASSWORD).invite(&repo.code))
        .await
        .unwrap();
    w.svc
        .register(Registration::new("bob", PASSWORD).invite(&org_invite.code))
        .await
        .unwrap();
    assert_eq!(
        w.svc.role_in(&game, &UserId::new("alice")).await.unwrap(),
        Some(Role::Writer)
    );
    assert_eq!(
        w.store.org_role(&org, &UserId::new("bob")).await.unwrap(),
        Some(OrgRole::Member)
    );

    let missing_org = w
        .svc
        .create_server_invitation(
            &root,
            NewServerInvitation {
                grant: Some(InvitationGrant::Organization {
                    org: UserId::new("ghost"),
                    role: OrgRole::Member,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(missing_org, PynError::OrgNotFound(_)));
}

#[tokio::test]
async fn the_code_is_emailed_only_when_an_address_and_a_mailer_exist() {
    let w = invite_only();
    let root = admin(&w).await;
    let made = w
        .svc
        .create_server_invitation(
            &root,
            NewServerInvitation {
                email: Some("Dana@Example.org".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(made.emailed);
    let sent = w.mail.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, "dana@example.org");
    assert_eq!(code_in(&sent[0].body), made.code);
    assert!(sent[0].body.contains("pyn register <name> --invite"));
    let listed = w.svc.list_server_invitations(&root).await.unwrap();
    assert_eq!(listed[0].email.as_deref(), Some("dana@example.org"));

    let none = w
        .svc
        .create_server_invitation(&root, NewServerInvitation::default())
        .await
        .unwrap();
    assert!(!none.emailed);
    assert_eq!(w.mail.sent().len(), 1);

    let bad = w
        .svc
        .create_server_invitation(
            &root,
            NewServerInvitation {
                email: Some("not-an-address".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(bad, PynError::InvalidRequest(_)));

    let quiet = world_with(RegistrationMode::InviteOnly, |_| {});
    let quiet_svc = AccessService::new(
        quiet.store.clone(),
        quiet.clock.clone(),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(AccessConfig {
        registration: RegistrationMode::InviteOnly,
        require_email_verification: false,
        ..AccessConfig::default()
    })
    .with_email(Arc::new(NullEmailSender));
    quiet_svc
        .admin_for_tests(&UserId::new("root"))
        .await
        .unwrap();
    let r = quiet_svc
        .create_server_invitation(
            &identity("root"),
            NewServerInvitation {
                email: Some("dana@example.org".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!r.emailed, "no mailer, nothing sent");
}

#[tokio::test]
async fn an_added_account_has_a_one_time_password_until_it_changes_it() {
    let w = invite_only();
    let root = admin(&w).await;
    let account = w
        .svc
        .admin_add_account(&root, "erin", "one-time password")
        .await
        .unwrap();
    assert_eq!(account.signup, SignupStage::Complete);
    let erin = UserId::new("erin");
    assert!(w.svc.password_change_required(&erin).await.unwrap());
    assert!(!w.svc.password_change_required(&root.user).await.unwrap());

    let (session, cookie) = w
        .svc
        .start_session_with("erin", "one-time password", None, false, None)
        .await
        .unwrap();
    assert_eq!(session.user, erin);
    w.svc
        .change_password(
            &identity("erin"),
            Some("one-time password"),
            "a better password",
            Some(&cookie),
        )
        .await
        .unwrap();
    assert!(!w.svc.password_change_required(&erin).await.unwrap());

    assert!(matches!(
        w.svc
            .admin_add_account(&root, "erin", "another password")
            .await,
        Err(PynError::UserExists(_))
    ));
    assert!(matches!(
        w.svc.admin_add_account(&root, "frank", "short").await,
        Err(PynError::InvalidRequest(_))
    ));
    assert!(matches!(
        w.svc
            .admin_add_account(&identity("erin"), "gina", "one-time password")
            .await,
        Err(PynError::ServerAdminRequired)
    ));
    assert!(
        server_actions(&w)
            .await
            .contains(&AuditAction::AccountAddedByAdmin)
    );
}

#[tokio::test]
async fn a_welcome_message_follows_confirmation_and_never_blocks_it() {
    let w = world_with(RegistrationMode::Open, |c| {
        c.require_email_verification = true
    });
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
    let link = w.mail.sent()[0].body.clone();
    let token = link
        .split_once("verify-email?token=")
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let up = w.svc.verify_email(&token).await.unwrap();
    assert_eq!(up.status, AccountStatus::Active);
    let sent = w.mail.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].to, "alice@example.org");
    assert_eq!(sent[1].subject, "Welcome to pyn");
    assert!(
        sent[1]
            .body
            .contains("https://pyn.example/_/settings/security")
    );
    assert!(sent[1].body.contains("administrator"));
    assert!(!sent[1].body.contains("http://"));

    struct Failing;
    #[async_trait]
    impl EmailSender for Failing {
        async fn send(&self, message: EmailMessage) -> pyn_core::Result<()> {
            if message.subject == "Welcome to pyn" {
                Err(PynError::EmailDeliveryFailed("down".into()))
            } else {
                Ok(())
            }
        }
    }
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let mail = Arc::new(MemoryEmailSender::new());
    struct Tee(Arc<MemoryEmailSender>);
    #[async_trait]
    impl EmailSender for Tee {
        async fn send(&self, message: EmailMessage) -> pyn_core::Result<()> {
            self.0.send(message.clone()).await?;
            Failing.send(message).await
        }
    }
    let svc = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        clock,
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
    .with_config(AccessConfig {
        registration: RegistrationMode::Open,
        public_url: "https://pyn.example".into(),
        ..AccessConfig::default()
    })
    .with_email(Arc::new(Tee(mail.clone())));
    svc.register(Registration::new("bob", PASSWORD).email("bob@example.org"))
        .await
        .unwrap();
    let token = mail.sent()[0]
        .body
        .split_once("verify-email?token=")
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let up = svc.verify_email(&token).await.unwrap();
    assert_eq!(
        up.status,
        AccountStatus::Active,
        "a failed welcome is only logged"
    );
}

#[tokio::test]
async fn an_account_waiting_for_approval_is_told_so() {
    let w = world_with(RegistrationMode::Open, |c| {
        c.require_email_verification = true;
        c.require_approval = true;
    });
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
    let token = w.mail.sent()[0]
        .body
        .split_once("verify-email?token=")
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    w.svc.verify_email(&token).await.unwrap();
    assert!(w.mail.sent()[1].body.contains("approve"));
}

#[tokio::test]
async fn an_administrator_can_resend_a_confirmation_within_the_rate_limit() {
    let w = world_with(RegistrationMode::Open, |c| {
        c.require_email_verification = true
    });
    let root = admin(&w).await;
    w.svc
        .register(Registration::new("alice", PASSWORD).email("alice@example.org"))
        .await
        .unwrap();
    let alice = UserId::new("alice");

    w.svc
        .admin_resend_confirmation(&root, &alice)
        .await
        .unwrap();
    assert_eq!(w.mail.sent().len(), 2);
    assert!(w.mail.sent()[1].body.contains("verify-email?token="));
    w.svc
        .admin_resend_confirmation(&root, &alice)
        .await
        .unwrap();
    let limited = w
        .svc
        .admin_resend_confirmation(&root, &alice)
        .await
        .unwrap_err();
    assert!(matches!(limited, PynError::TooManyAttempts { .. }));
    assert_eq!(w.mail.sent().len(), 3);

    assert!(matches!(
        w.svc
            .admin_resend_confirmation(&identity("alice"), &alice)
            .await,
        Err(PynError::ServerAdminRequired)
    ));
    assert!(matches!(
        w.svc
            .admin_resend_confirmation(&root, &UserId::new("ghost"))
            .await,
        Err(PynError::UserNotFound(_))
    ));
    assert!(
        server_actions(&w)
            .await
            .iter()
            .filter(|a| **a == AuditAction::AccountConfirmationResent)
            .count()
            == 2
    );

    let token = w.mail.sent()[2]
        .body
        .split_once("verify-email?token=")
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    w.svc.verify_email(&token).await.unwrap();
    assert!(matches!(
        w.svc.admin_resend_confirmation(&root, &alice).await,
        Err(PynError::EmailAlreadyConfirmed)
    ));
}

#[tokio::test]
async fn resending_is_refused_when_verification_is_off_and_status_names_the_mailer() {
    let w = world_with(RegistrationMode::Open, |c| {
        c.require_email_verification = false
    });
    let root = admin(&w).await;
    w.svc
        .register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    assert!(matches!(
        w.svc
            .admin_resend_confirmation(&root, &UserId::new("alice"))
            .await,
        Err(PynError::EmailVerificationOff)
    ));
    let status = w.svc.email_status();
    assert_eq!(status.mailer, pyn_core::MailerKind::Log);
    assert!(!status.verification);
}

#[tokio::test]
async fn a_grant_into_a_frozen_organization_is_refused_when_made_and_when_used() {
    let w = invite_only();
    let root = admin(&w).await;
    let org = w.svc.create_org(&root, "studio").await.unwrap().user;
    let grant = NewServerInvitation {
        grant: Some(InvitationGrant::Organization {
            org: org.clone(),
            role: OrgRole::Member,
        }),
        ..Default::default()
    };
    let made = w
        .svc
        .create_server_invitation(&root, grant.clone())
        .await
        .unwrap();
    w.svc.freeze_owner(&root, &org, "audit").await.unwrap();
    assert!(matches!(
        w.svc.create_server_invitation(&root, grant).await,
        Err(PynError::OwnerFrozen { .. })
    ));
    let err = w
        .svc
        .register(Registration::new("alice", PASSWORD).invite(&made.code))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::OwnerFrozen { .. }), "{err}");
    assert!(!w.store.user_exists(&UserId::new("alice")).await.unwrap());
}

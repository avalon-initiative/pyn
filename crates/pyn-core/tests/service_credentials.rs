//! Service credentials: administrative scopes for a trusted external service, no account and no repository roles.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Credential,
    Identity, ManualClock, PynError, Registration, RegistrationMode, ServiceScope, UserId,
};

const PASSWORD: &str = "correct horse battery";

struct World {
    svc: AccessService,
    clock: Arc<ManualClock>,
    audit: Arc<MemoryAuditStore>,
}

fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let cfg = AccessConfig {
        registration: RegistrationMode::Open,
        require_email_verification: false,
        ..AccessConfig::default()
    };
    let svc = AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
        .with_config(cfg)
        .with_audit(audit.clone());
    World { svc, clock, audit }
}

fn session(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

async fn admin(w: &World) -> Identity {
    w.svc.bootstrap_admin(&UserId::new("root")).await.unwrap();
    session("root")
}

fn scopes(list: &[ServiceScope]) -> BTreeSet<ServiceScope> {
    list.iter().copied().collect()
}

async fn service(w: &World, name: &str, list: &[ServiceScope]) -> (Identity, String) {
    let root = admin(w).await;
    let (_, secret) = w
        .svc
        .create_service_credential(&root, name, scopes(list))
        .await
        .unwrap();
    (w.svc.identify(&secret).await.unwrap(), secret)
}

async fn server_log(w: &World) -> Vec<(String, AuditAction, String)> {
    let query = AuditQuery {
        limit: 100,
        ..Default::default()
    };
    let mut events = w.audit.list(&AuditScope::Server, &query).await.unwrap();
    events.reverse();
    events
        .into_iter()
        .map(|e| (e.actor.to_string(), e.action, e.detail))
        .collect()
}

async fn signup(w: &World, name: &str) {
    w.svc
        .register(Registration::new(name, PASSWORD))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_administrator_creates_a_credential_whose_secret_is_stored_hashed() {
    let w = world();
    let root = admin(&w).await;
    let (record, secret) = w
        .svc
        .create_service_credential(
            &root,
            "provisioner",
            scopes(&[ServiceScope::ManageAccounts]),
        )
        .await
        .unwrap();
    assert!(secret.starts_with("pyns_"));
    assert!(!record.secret_hash.contains(&secret));
    assert_eq!(record.created_by, UserId::new("root"));
    let listed = w.svc.list_service_credentials(&root).await.unwrap();
    assert_eq!(listed, [record]);
    assert_eq!(listed[0].last_used_at, None);

    w.clock.advance(chrono::Duration::minutes(5));
    w.svc.identify(&secret).await.unwrap();
    let listed = w.svc.list_service_credentials(&root).await.unwrap();
    assert_eq!(listed[0].last_used_at, Some(w.clock_now()));
}

impl World {
    fn clock_now(&self) -> chrono::DateTime<Utc> {
        use pyn_core::Clock;
        self.clock.now()
    }
}

#[tokio::test]
async fn names_and_scopes_are_validated_and_names_are_never_reused() {
    let w = world();
    let root = admin(&w).await;
    for bad in ["", "Billing", "has space", "-lead", "@service:x"] {
        let err = w
            .svc
            .create_service_credential(&root, bad, scopes(&[ServiceScope::ManageAccounts]))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad}: {err}");
    }
    let err = w
        .svc
        .create_service_credential(&root, "ok", scopes(&[]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    assert!("read_usage".parse::<ServiceScope>().is_err());

    w.svc
        .create_service_credential(&root, "ok", scopes(&[ServiceScope::ManageAccounts]))
        .await
        .unwrap();
    w.svc.revoke_service_credential(&root, "ok").await.unwrap();
    let err = w
        .svc
        .create_service_credential(&root, "ok", scopes(&[ServiceScope::ManageAccounts]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServiceCredentialExists(_)), "{err}");
}

#[tokio::test]
async fn only_a_server_administrator_manages_credentials() {
    let w = world();
    let (svc_id, _) = service(
        &w,
        "provisioner",
        &[
            ServiceScope::ManageAccounts,
            ServiceScope::ManageOrganizations,
        ],
    )
    .await;
    signup(&w, "alice").await;
    for who in [&svc_id, &session("alice")] {
        let err = w
            .svc
            .create_service_credential(who, "other", scopes(&[ServiceScope::ManageAccounts]))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
        let err = w.svc.list_service_credentials(who).await.unwrap_err();
        assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
        let err = w
            .svc
            .revoke_service_credential(who, "provisioner")
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
    }
    assert!(!w.svc.is_server_admin(&svc_id).await.unwrap());
}

#[tokio::test]
async fn a_revoked_credential_is_refused_at_once() {
    let w = world();
    let (_, secret) = service(&w, "provisioner", &[ServiceScope::ManageAccounts]).await;
    let root = session("root");
    w.svc
        .revoke_service_credential(&root, "provisioner")
        .await
        .unwrap();
    let err = w.svc.identify(&secret).await.unwrap_err();
    assert!(matches!(err, PynError::Unauthenticated(_)), "{err}");
    w.svc
        .revoke_service_credential(&root, "provisioner")
        .await
        .unwrap();
    let err = w
        .svc
        .revoke_service_credential(&root, "nobody")
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::ServiceCredentialNotFound(_)),
        "{err}"
    );
    let listed = w.svc.list_service_credentials(&root).await.unwrap();
    assert!(listed[0].revoked_at.is_some());
}

#[tokio::test]
async fn a_wrong_or_malformed_secret_is_refused() {
    let w = world();
    let (_, secret) = service(&w, "provisioner", &[ServiceScope::ManageAccounts]).await;
    let forged = format!("{}{}", &secret[..secret.len() - 4], "0000");
    for bad in [forged.as_str(), "pyns_nonsense", "pyns_"] {
        let err = w.svc.identify(bad).await.unwrap_err();
        assert!(matches!(err, PynError::Unauthenticated(_)), "{bad}: {err}");
    }
}

#[tokio::test]
async fn a_credential_cannot_sign_in_or_hold_a_repository_role() {
    let w = world();
    let (svc_id, _) = service(&w, "provisioner", &[ServiceScope::ManageAccounts]).await;
    for name in ["provisioner", "@service:provisioner"] {
        let err = w.svc.login(name, PASSWORD, None).await.unwrap_err();
        assert!(matches!(err, PynError::Unauthenticated(_)), "{name}: {err}");
    }
    let repo = pyn_core::RepoId::new("alice/game");
    let principal = w.svc.principal_in(&repo, &svc_id).await.unwrap();
    assert!(principal.permissions.is_empty());
    let err = svc_id.require_namespace_management().unwrap_err();
    assert!(
        matches!(err, PynError::ServiceCredentialNotAllowed),
        "{err}"
    );
}

#[tokio::test]
async fn scopes_decide_which_admin_actions_a_credential_may_take() {
    let w = world();
    signup(&w, "alice").await;
    signup(&w, "bob").await;
    let (accounts, _) = service(&w, "accounts-only", &[ServiceScope::ManageAccounts]).await;
    let (orgs, _) = service(&w, "orgs-only", &[ServiceScope::ManageOrganizations]).await;
    let alice = UserId::new("alice");
    let acme = UserId::new("acme");

    assert_eq!(
        w.svc
            .list_accounts(&accounts, None, 10)
            .await
            .unwrap()
            .len(),
        3
    );
    w.svc
        .disable_account(&accounts, &UserId::new("bob"), Some("abuse"))
        .await
        .unwrap();
    w.svc
        .enable_account(&accounts, &UserId::new("bob"))
        .await
        .unwrap();
    let err = w.svc.list_accounts(&orgs, None, 10).await.unwrap_err();
    assert!(
        matches!(
            err,
            PynError::ServiceScopeRequired(ServiceScope::ManageAccounts)
        ),
        "{err}"
    );
    let err = w
        .svc
        .disable_account(&orgs, &alice, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServiceScopeRequired(_)), "{err}");

    let err = w
        .svc
        .admin_create_org(&accounts, "acme", &alice)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            PynError::ServiceScopeRequired(ServiceScope::ManageOrganizations)
        ),
        "{err}"
    );
    w.svc.admin_create_org(&orgs, "acme", &alice).await.unwrap();
    let err = w.svc.admin_delete_org(&accounts, &acme).await.unwrap_err();
    assert!(matches!(err, PynError::ServiceScopeRequired(_)), "{err}");
    w.svc.admin_delete_org(&orgs, &acme).await.unwrap();

    let err = w
        .svc
        .server_audit(&accounts, &AuditQuery::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
}

#[tokio::test]
async fn an_organization_is_created_for_an_existing_active_owner() {
    let w = world();
    let (orgs, _) = service(&w, "provisioner", &[ServiceScope::ManageOrganizations]).await;
    signup(&w, "alice").await;
    let alice = UserId::new("alice");

    let err = w
        .svc
        .admin_create_org(&orgs, "acme", &UserId::new("ghost"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserNotFound(_)), "{err}");
    let err = w
        .svc
        .admin_create_org(&orgs, "alice", &alice)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserExists(_)), "{err}");
    let err = w
        .svc
        .admin_create_org(&orgs, "admin", &alice)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ReservedName(_)), "{err}");

    let acme = UserId::new("acme");
    w.svc.admin_create_org(&orgs, "acme", &alice).await.unwrap();
    assert!(w.svc.org_role(&acme, &alice).await.unwrap().is_some());
    assert!(
        w.svc.org_role(&acme, &orgs.user).await.unwrap().is_none(),
        "the credential owns nothing"
    );
    let err = w
        .svc
        .admin_delete_org(&orgs, &UserId::new("ghost"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::OrgNotFound(_)), "{err}");
}

#[tokio::test]
async fn a_server_administrator_may_use_the_org_routes_too() {
    let w = world();
    let root = admin(&w).await;
    signup(&w, "alice").await;
    w.svc
        .admin_create_org(&root, "acme", &UserId::new("alice"))
        .await
        .unwrap();
    w.svc
        .admin_delete_org(&root, &UserId::new("acme"))
        .await
        .unwrap();
    let err = w
        .svc
        .admin_create_org(&session("alice"), "acme", &UserId::new("alice"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
}

#[tokio::test]
async fn every_service_action_is_logged_with_the_credential_name() {
    let w = world();
    signup(&w, "alice").await;
    let (svc_id, _) = service(
        &w,
        "provisioner",
        &[
            ServiceScope::ManageAccounts,
            ServiceScope::ManageOrganizations,
        ],
    )
    .await;
    let alice = UserId::new("alice");
    w.svc.disable_account(&svc_id, &alice, None).await.unwrap();
    w.svc.enable_account(&svc_id, &alice).await.unwrap();
    w.svc
        .admin_create_org(&svc_id, "acme", &alice)
        .await
        .unwrap();
    w.svc
        .admin_delete_org(&svc_id, &UserId::new("acme"))
        .await
        .unwrap();
    w.svc
        .revoke_service_credential(&session("root"), "provisioner")
        .await
        .unwrap();

    let log = server_log(&w).await;
    let actions: Vec<_> = log.iter().map(|(a, k, _)| (a.as_str(), *k)).collect();
    assert_eq!(
        actions,
        [
            ("root", AuditAction::ServiceCredentialCreated),
            ("@service:provisioner", AuditAction::AccountDisabled),
            ("@service:provisioner", AuditAction::AccountEnabled),
            ("@service:provisioner", AuditAction::OrgCreated),
            ("@service:provisioner", AuditAction::OrgDeleted),
            ("root", AuditAction::ServiceCredentialRevoked),
        ]
    );
    assert!(log[0].2.contains("provisioner") && log[0].2.contains("manage_accounts"));
    let org_log = w
        .audit
        .list(
            &AuditScope::Org(UserId::new("acme")),
            &AuditQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        org_log
            .iter()
            .all(|e| e.actor.as_str() == "@service:provisioner")
    );
    assert_eq!(org_log.len(), 2);
}

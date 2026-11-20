//! Service credentials: administrative scopes for a trusted external service, no account and no repository roles.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Credential,
    Identity, ManualClock, PynError, Registration, RegistrationMode, ServiceCredentialLimits,
    ServiceScope, UserId,
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
    let svc = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        clock.clone(),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
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
    w.svc.admin_for_tests(&UserId::new("root")).await.unwrap();
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

#[tokio::test]
async fn a_service_credential_cannot_grant_or_revoke_administrators() {
    let w = world();
    let (svc, _) = service(
        &w,
        "provisioner",
        &[
            ServiceScope::ManageAccounts,
            ServiceScope::ManageOrganizations,
        ],
    )
    .await;
    signup(&w, "alice").await;
    for err in [
        w.svc
            .grant_admin(&svc, &UserId::new("alice"))
            .await
            .unwrap_err(),
        w.svc
            .revoke_admin(&svc, &UserId::new("root"))
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(err, PynError::ServerAdminRequired), "{err}");
    }
}

fn ip(s: &str) -> std::net::IpAddr {
    s.parse().unwrap()
}

async fn restricted(w: &World, name: &str, limits: ServiceCredentialLimits) -> String {
    let root = admin(w).await;
    let (_, secret) = w
        .svc
        .create_service_credential_with(
            &root,
            name,
            scopes(&[ServiceScope::ManageAccounts]),
            limits,
        )
        .await
        .unwrap();
    secret
}

fn unauthenticated(r: Result<Identity, PynError>) -> bool {
    matches!(r, Err(PynError::Unauthenticated(_)))
}

async fn refusals(w: &World) -> Vec<String> {
    server_log(w)
        .await
        .into_iter()
        .filter(|(_, k, _)| *k == AuditAction::ServiceCredentialRefused)
        .map(|(actor, _, detail)| format!("{actor}: {detail}"))
        .collect()
}

#[tokio::test]
async fn a_credential_without_restrictions_works_from_anywhere_and_is_unchanged() {
    let w = world();
    let (_, secret) = service(&w, "plain", &[ServiceScope::ManageAccounts]).await;
    for source in [Some(ip("203.0.113.9")), Some(ip("2001:db8::1")), None] {
        w.svc.identify_from(&secret, source).await.unwrap();
    }
    let root = admin(&w).await;
    let record = w
        .svc
        .list_service_credentials(&root)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (
            record.expires_at,
            record.allowed_sources.len(),
            record.rate_limit_per_minute
        ),
        (None, 0, None)
    );
    assert_eq!(record.last_used_from, None, "the last call had no address");
}

#[tokio::test]
async fn creation_validates_restrictions_and_canonicalizes_sources() {
    let w = world();
    let root = admin(&w).await;
    let make = |limits: ServiceCredentialLimits| {
        w.svc.create_service_credential_with(
            &root,
            "x",
            scopes(&[ServiceScope::ManageAccounts]),
            limits,
        )
    };
    let past = w.clock_now() - chrono::Duration::seconds(1);
    for bad in [
        ServiceCredentialLimits {
            expires_at: Some(past),
            ..Default::default()
        },
        ServiceCredentialLimits {
            expires_at: Some(w.clock_now()),
            ..Default::default()
        },
        ServiceCredentialLimits {
            rate_limit_per_minute: Some(0),
            ..Default::default()
        },
        ServiceCredentialLimits {
            allowed_sources: vec!["not-an-ip".into()],
            ..Default::default()
        },
        ServiceCredentialLimits {
            allowed_sources: vec!["10.0.0.0/40".into()],
            ..Default::default()
        },
        ServiceCredentialLimits {
            allowed_sources: vec!["10.0.0.1".into(); 65],
            ..Default::default()
        },
    ] {
        assert!(matches!(make(bad).await, Err(PynError::InvalidRequest(_))));
    }
    assert!(
        w.svc
            .list_service_credentials(&root)
            .await
            .unwrap()
            .is_empty(),
        "a refused creation leaves nothing behind"
    );
    let (record, _) = make(ServiceCredentialLimits {
        allowed_sources: vec![
            "10.9.9.9/8".into(),
            "10.0.0.0/8".into(),
            " 192.0.2.7 ".into(),
        ],
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(record.allowed_sources, ["10.0.0.0/8", "192.0.2.7"]);
}

#[tokio::test]
async fn an_expired_credential_is_refused_audited_and_still_listed() {
    let w = world();
    let expires = w.clock_now() + chrono::Duration::hours(1);
    let secret = restricted(
        &w,
        "billing",
        ServiceCredentialLimits {
            expires_at: Some(expires),
            ..Default::default()
        },
    )
    .await;
    w.svc.identify(&secret).await.unwrap();

    w.clock.advance(chrono::Duration::hours(1));
    assert!(
        unauthenticated(w.svc.identify(&secret).await),
        "refused from the instant itself"
    );

    let root = admin(&w).await;
    let listed = w.svc.list_service_credentials(&root).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].expires_at, Some(expires));
    assert_eq!(listed[0].revoked_at, None, "expiry is not revocation");

    let log = refusals(&w).await;
    assert_eq!(log.len(), 1);
    assert!(log[0].starts_with("@service:billing:") && log[0].contains("expired"));
    assert!(!log[0].contains(&secret));
}

#[tokio::test]
async fn a_source_restricted_credential_refuses_other_and_unknown_addresses() {
    let w = world();
    let secret = restricted(
        &w,
        "billing",
        ServiceCredentialLimits {
            allowed_sources: vec!["10.0.0.0/8".into(), "2001:db8::/32".into()],
            ..Default::default()
        },
    )
    .await;
    for ok in ["10.1.2.3", "2001:db8::7", "::ffff:10.1.2.3"] {
        w.svc.identify_from(&secret, Some(ip(ok))).await.unwrap();
    }
    let root = admin(&w).await;
    let used = w
        .svc
        .list_service_credentials(&root)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(used.last_used_from.as_deref(), Some("10.1.2.3"));

    assert!(unauthenticated(
        w.svc.identify_from(&secret, Some(ip("203.0.113.9"))).await
    ));
    assert!(
        unauthenticated(w.svc.identify_from(&secret, None).await),
        "an unknown address fails closed"
    );
    assert!(unauthenticated(w.svc.identify(&secret).await));

    let log = refusals(&w).await;
    assert_eq!(log.len(), 3);
    assert!(log[0].contains("203.0.113.9") && log[0].contains("source"));
    assert!(log[1].contains("unknown address"));
    assert!(log.iter().all(|l| !l.contains(&secret)));
    let after = w
        .svc
        .list_service_credentials(&root)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        after.last_used_from.as_deref(),
        Some("10.1.2.3"),
        "a refusal is not a use"
    );
}

#[tokio::test]
async fn a_wrong_secret_from_a_wrong_address_is_not_audited() {
    let w = world();
    let secret = restricted(
        &w,
        "billing",
        ServiceCredentialLimits {
            allowed_sources: vec!["10.0.0.0/8".into()],
            ..Default::default()
        },
    )
    .await;
    let mut wrong = secret.clone();
    wrong.pop();
    wrong.push(if secret.ends_with('0') { '1' } else { '0' });
    assert!(unauthenticated(
        w.svc.identify_from(&wrong, Some(ip("203.0.113.9"))).await
    ));
    assert!(refusals(&w).await.is_empty());
}

#[tokio::test]
async fn each_credential_has_its_own_rate_limit_that_resets() {
    let w = world();
    let tight = restricted(
        &w,
        "tight",
        ServiceCredentialLimits {
            rate_limit_per_minute: Some(3),
            ..Default::default()
        },
    )
    .await;
    let (_, other) = service(&w, "other", &[ServiceScope::ManageAccounts]).await;
    // `service` identified once already; the default limit is far above these calls.
    for _ in 0..3 {
        w.svc.identify(&tight).await.unwrap();
    }
    let refused = w.svc.identify(&tight).await;
    assert!(
        matches!(refused, Err(PynError::TooManyAttempts { retry_after_secs }) if retry_after_secs > 0)
    );
    w.svc.identify(&other).await.unwrap();

    w.clock.advance(chrono::Duration::seconds(61));
    w.svc.identify(&tight).await.unwrap();
    assert!(
        refusals(&w).await.is_empty(),
        "rate-limit refusals are not audited"
    );
}

#[tokio::test]
async fn the_default_limit_is_configurable_and_zero_turns_it_off() {
    for (default, calls, ok) in [(2, 5, 2), (0, 50, 50)] {
        let clock = Arc::new(ManualClock::new(
            Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
        ));
        let mut cfg = AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        };
        cfg.limits.service_per_credential = default;
        let svc = AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock,
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(cfg);
        svc.admin_for_tests(&UserId::new("root")).await.unwrap();
        let (_, secret) = svc
            .create_service_credential(
                &session("root"),
                "svc",
                scopes(&[ServiceScope::ManageAccounts]),
            )
            .await
            .unwrap();
        let mut accepted = 0;
        for _ in 0..calls {
            accepted += usize::from(svc.identify(&secret).await.is_ok());
        }
        assert_eq!(accepted, ok);
    }
}

#[tokio::test]
async fn refused_source_attempts_do_not_use_up_the_credentials_rate_limit() {
    let w = world();
    let secret = restricted(
        &w,
        "billing",
        ServiceCredentialLimits {
            allowed_sources: vec!["10.0.0.0/8".into()],
            rate_limit_per_minute: Some(2),
            ..Default::default()
        },
    )
    .await;
    for _ in 0..10 {
        assert!(unauthenticated(
            w.svc.identify_from(&secret, Some(ip("203.0.113.9"))).await
        ));
    }
    w.svc
        .identify_from(&secret, Some(ip("10.0.0.1")))
        .await
        .unwrap();
    w.svc
        .identify_from(&secret, Some(ip("10.0.0.1")))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_refusal_for_a_wrong_scope_is_audited_without_the_secret() {
    let w = world();
    let (identity, secret) = service(&w, "billing", &[ServiceScope::ManageAccounts]).await;
    let Credential::Service(record) = &identity.credential else {
        panic!()
    };
    w.svc
        .record_service_refusal(record, "scope manage_limits required", Some(ip("10.0.0.1")))
        .await;
    let log = refusals(&w).await;
    assert_eq!(log.len(), 1);
    assert!(log[0].contains("manage_limits") && log[0].contains("10.0.0.1"));
    assert!(!log[0].contains(&secret));
}

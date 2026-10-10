//! Sign-in and linking through an OpenID Connect provider, against a scripted provider.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditAction, AuditQuery, AuditScope, AuditStore,
    AuthorizationRequest, CodeExchange, Credential, ExternalClaims, ExternalPolicy, ExternalSignIn,
    Identity, ManualClock, OidcBegin, OidcFinish, OidcProvider, OidcStart, PynError, Registration,
    RegistrationMode, UserId, pkce_challenge,
};

const ISSUER: &str = "https://idp.example";
const PASSWORD: &str = "correct horse battery";

#[derive(Default)]
struct Scripted {
    next: Mutex<Option<pyn_core::Result<ExternalClaims>>>,
    requests: Mutex<Vec<(String, String, String)>>,
    exchanges: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl OidcProvider for Scripted {
    fn display_name(&self) -> &str {
        "Acme SSO"
    }

    async fn authorization_url(&self, r: &AuthorizationRequest<'_>) -> pyn_core::Result<String> {
        self.requests.lock().unwrap().push((
            r.state.into(),
            r.nonce.into(),
            r.code_challenge.into(),
        ));
        Ok(format!(
            "{ISSUER}/authorize?state={}&redirect={}",
            r.state, r.redirect_uri
        ))
    }

    async fn exchange(&self, e: &CodeExchange<'_>) -> pyn_core::Result<ExternalClaims> {
        self.exchanges.lock().unwrap().push((
            e.code_verifier.into(),
            e.nonce.into(),
            e.redirect_uri.into(),
        ));
        self.next.lock().unwrap().take().expect("claims scripted")
    }
}

struct World {
    svc: AccessService,
    store: Arc<MemoryAccessStore>,
    provider: Arc<Scripted>,
    audit: Arc<MemoryAuditStore>,
    clock: Arc<ManualClock>,
}

fn world_with(policy: impl FnOnce(&mut ExternalPolicy)) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let store = Arc::new(MemoryAccessStore::new());
    let provider = Arc::new(Scripted::default());
    let audit = Arc::new(MemoryAuditStore::new());
    let mut external = ExternalPolicy::default();
    policy(&mut external);
    let svc = AccessService::new(store.clone(), clock.clone())
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            public_url: "https://pyn.example/".into(),
            external,
            ..AccessConfig::default()
        })
        .with_oidc(provider.clone())
        .with_audit(audit.clone());
    World {
        svc,
        store,
        provider,
        audit,
        clock,
    }
}

fn creating() -> World {
    world_with(|p| p.create_accounts = true)
}

fn claims(subject: &str) -> ExternalClaims {
    ExternalClaims {
        issuer: ISSUER.into(),
        subject: subject.into(),
        email: None,
        email_verified: false,
        username_hint: None,
    }
}

fn who(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

impl World {
    async fn register(&self, name: &str, email: Option<&str>) {
        let mut request = Registration::new(name, PASSWORD);
        if let Some(email) = email {
            request = request.email(email);
        }
        self.svc.register(request).await.unwrap();
    }

    async fn start(&self, link: Option<&Identity>) -> OidcBegin {
        self.svc
            .begin_oidc(OidcStart {
                link,
                ..OidcStart::default()
            })
            .await
            .unwrap()
    }

    /// Runs a whole flow in which the provider vouches for `claims`.
    async fn flow(
        &self,
        link: Option<&Identity>,
        claims: ExternalClaims,
    ) -> pyn_core::Result<ExternalSignIn> {
        let begin = self.start(link).await;
        *self.provider.next.lock().unwrap() = Some(Ok(claims));
        self.finish(&begin).await
    }

    async fn finish(&self, begin: &OidcBegin) -> pyn_core::Result<ExternalSignIn> {
        let state = self
            .provider
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .0
            .clone();
        self.svc
            .finish_oidc(OidcFinish {
                code: "code",
                state: &state,
                binding: Some(&begin.binding),
            })
            .await
    }

    async fn server_events(&self) -> Vec<(AuditAction, String)> {
        self.audit
            .list(
                &AuditScope::Server,
                &AuditQuery {
                    limit: 50,
                    ..AuditQuery::default()
                },
            )
            .await
            .unwrap()
            .into_iter()
            .rev()
            .map(|e| (e.action, e.detail))
            .collect()
    }
}

fn callback<'a>(state: &'a str, binding: Option<&'a str>) -> OidcFinish<'a> {
    OidcFinish {
        code: "c",
        state,
        binding,
    }
}

fn signed_in(outcome: ExternalSignIn) -> (String, String, bool) {
    match outcome {
        ExternalSignIn::SignedIn {
            session,
            cookie,
            created,
            ..
        } => (session.user.to_string(), cookie, created),
        other => panic!("expected a sign-in, got {other:?}"),
    }
}

#[tokio::test]
async fn a_flow_carries_pkce_state_and_nonce_to_the_exchange() {
    let w = creating();
    let begin = w.start(None).await;
    assert!(begin.url.starts_with(ISSUER));
    assert!(
        begin
            .url
            .contains("redirect=https://pyn.example/v1/oidc/callback")
    );
    let (state, nonce, challenge) = w.provider.requests.lock().unwrap()[0].clone();
    assert!(begin.url.contains(&state));
    assert_ne!(begin.binding, state, "the binding is a separate secret");

    *w.provider.next.lock().unwrap() = Some(Ok(claims("s1")));
    w.finish(&begin).await.unwrap();
    let (verifier, sent_nonce, redirect) = w.provider.exchanges.lock().unwrap()[0].clone();
    assert_eq!(pkce_challenge(&verifier), challenge);
    assert_eq!(sent_nonce, nonce);
    assert_eq!(redirect, "https://pyn.example/v1/oidc/callback");
}

#[tokio::test]
async fn nothing_works_without_a_provider() {
    let svc = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        Arc::new(ManualClock::new(Utc::now())),
    );
    assert!(svc.oidc_name().is_none());
    assert!(matches!(
        svc.begin_oidc(OidcStart::default()).await,
        Err(PynError::OidcNotConfigured)
    ));
    assert!(matches!(
        svc.finish_oidc(OidcFinish {
            code: "c",
            state: "s",
            binding: None
        })
        .await,
        Err(PynError::OidcNotConfigured)
    ));
    assert!(svc.password_sign_in_enabled());
}

#[tokio::test]
async fn a_flow_works_once_and_only_in_the_browser_that_started_it() {
    let w = creating();
    let begin = w.start(None).await;
    let state = w.provider.requests.lock().unwrap()[0].0.clone();

    *w.provider.next.lock().unwrap() = Some(Ok(claims("s1")));
    let wrong = w
        .svc
        .finish_oidc(callback(&state, Some("someone-else")))
        .await;
    assert!(matches!(wrong, Err(PynError::ExternalSignInFailed(_))));
    let replay = w
        .svc
        .finish_oidc(callback(&state, Some(&begin.binding)))
        .await;
    assert!(
        matches!(replay, Err(PynError::ExternalSignInFailed(_))),
        "a refused callback uses the flow up"
    );
    assert!(w.provider.exchanges.lock().unwrap().is_empty());

    let begin = w.start(None).await;
    *w.provider.next.lock().unwrap() = Some(Ok(claims("s1")));
    w.finish(&begin).await.unwrap();
    assert!(matches!(
        w.finish(&begin).await,
        Err(PynError::ExternalSignInFailed(_))
    ));

    let begin = w.start(None).await;
    let state = w
        .provider
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .0
        .clone();
    w.clock.advance(Duration::minutes(11));
    let late = w
        .svc
        .finish_oidc(OidcFinish {
            code: "c",
            state: &state,
            binding: Some(&begin.binding),
        })
        .await;
    assert!(matches!(late, Err(PynError::ExternalSignInFailed(_))));

    let none = w
        .svc
        .finish_oidc(OidcFinish {
            code: "c",
            state: "unknown",
            binding: Some("x"),
        })
        .await;
    assert!(matches!(none, Err(PynError::ExternalSignInFailed(_))));
}

#[tokio::test]
async fn return_to_must_be_a_path_in_the_app() {
    let w = creating();
    for bad in ["https://evil.example/", "//evil.example"] {
        let r = w
            .svc
            .begin_oidc(OidcStart {
                return_to: Some(bad),
                ..OidcStart::default()
            })
            .await;
        assert!(matches!(r, Err(PynError::InvalidRequest(_))), "{bad}");
    }
    let begin = w
        .svc
        .begin_oidc(OidcStart {
            return_to: Some("/repos/acme"),
            ..OidcStart::default()
        })
        .await
        .unwrap();
    *w.provider.next.lock().unwrap() = Some(Ok(claims("s1")));
    match w.finish(&begin).await.unwrap() {
        ExternalSignIn::SignedIn { return_to, .. } => {
            assert_eq!(return_to.as_deref(), Some("/repos/acme"));
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn without_create_accounts_only_linked_identities_sign_in() {
    let w = world_with(|_| {});
    let refused = w.flow(None, claims("s1")).await;
    assert!(matches!(refused, Err(PynError::ExternalNotLinked)));

    w.register("alice", None).await;
    let linked = w.flow(Some(&who("alice")), claims("s1")).await.unwrap();
    assert!(matches!(
        linked,
        ExternalSignIn::Linked { ref user, .. } if user.as_str() == "alice"
    ));

    let (user, cookie, created) = signed_in(w.flow(None, claims("s1")).await.unwrap());
    assert_eq!((user.as_str(), created), ("alice", false));
    let (identity, _) = w.svc.authenticate_session(&cookie).await.unwrap();
    assert_eq!(identity.user.as_str(), "alice");
}

#[tokio::test]
async fn a_matching_verified_email_never_links_an_existing_account() {
    let w = world_with(|_| {});
    w.register("alice", Some("alice@example.org")).await;
    let mut c = claims("s-other");
    c.email = Some("alice@example.org".into());
    c.email_verified = true;
    let refused = w.flow(None, c.clone()).await;
    assert!(matches!(refused, Err(PynError::ExternalNotLinked)));

    let w = creating();
    w.register("alice", Some("alice@example.org")).await;
    w.store
        .complete_verification(
            &UserId::new("alice"),
            pyn_core::SignupStage::Complete,
            Utc::now(),
        )
        .await
        .unwrap();
    c.username_hint = Some("alice".into());
    let (user, _, created) = signed_in(w.flow(None, c).await.unwrap());
    assert!(created);
    assert_eq!(
        user, "alice-2",
        "a taken name gets a suffix, never the account"
    );
    let account = w.store.account(&UserId::new(&user)).await.unwrap().unwrap();
    assert!(
        account.email.is_none(),
        "an address another account verified is not copied"
    );
}

#[tokio::test]
async fn first_sign_in_creates_an_active_account_with_a_derived_name() {
    let w = creating();
    let mut c = claims("s1");
    c.username_hint = Some("Maya.Okafor".into());
    c.email = Some("Maya@Example.org".into());
    c.email_verified = true;
    let (user, cookie, created) = signed_in(w.flow(None, c).await.unwrap());
    assert!(created);
    assert_eq!(user, "maya-okafor");

    let account = w.store.account(&UserId::new(&user)).await.unwrap().unwrap();
    assert_eq!(account.status(), pyn_core::AccountStatus::Active);
    assert_eq!(account.email.as_deref(), Some("maya@example.org"));
    assert!(account.email_verified_at.is_some());
    assert!(
        w.store
            .password_hash(&account.user)
            .await
            .unwrap()
            .is_none()
    );
    w.svc.authenticate_session(&cookie).await.unwrap();

    let again = signed_in(w.flow(None, claims("s1")).await.unwrap());
    assert_eq!((again.0.as_str(), again.2), ("maya-okafor", false));

    let events = w.server_events().await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, AuditAction::ExternalAccountCreated);
    assert!(events[0].1.contains("maya-okafor") && events[0].1.contains(ISSUER));
}

#[tokio::test]
async fn an_unverified_email_is_neither_trusted_nor_stored() {
    let w = creating();
    let mut c = claims("s1");
    c.username_hint = Some("zoe".into());
    c.email = Some("zoe@example.org".into());
    let (user, ..) = signed_in(w.flow(None, c).await.unwrap());
    let account = w.store.account(&UserId::new(&user)).await.unwrap().unwrap();
    assert!(account.email.is_none() && account.email_verified_at.is_none());
}

#[tokio::test]
async fn derived_names_skip_reserved_ones_and_fall_back_to_the_email() {
    let w = creating();
    let mut c = claims("s1");
    c.username_hint = Some("admin".into());
    let (user, ..) = signed_in(w.flow(None, c).await.unwrap());
    assert_eq!(user, "admin-2", "a reserved name is never handed out");

    let mut c = claims("s2");
    c.email = Some("jo.smith@example.org".into());
    let (user, ..) = signed_in(w.flow(None, c).await.unwrap());
    assert_eq!(user, "jo-smith");

    let (user, ..) = signed_in(w.flow(None, claims("s3")).await.unwrap());
    assert_eq!(user, "member");
    let (user, ..) = signed_in(w.flow(None, claims("s4")).await.unwrap());
    assert_eq!(user, "member-2");
}

#[tokio::test]
async fn an_account_created_by_the_provider_has_no_password_to_sign_in_with() {
    let w = creating();
    let mut c = claims("s1");
    c.username_hint = Some("maya".into());
    w.flow(None, c).await.unwrap();
    let r = w.svc.login("maya", PASSWORD, None).await;
    assert!(matches!(r, Err(PynError::Unauthenticated(_))));
}

#[tokio::test]
async fn disabled_accounts_cannot_sign_in_through_the_provider() {
    let w = creating();
    let mut c = claims("s1");
    c.username_hint = Some("maya".into());
    w.flow(None, c).await.unwrap();
    w.store
        .set_disabled(&UserId::new("maya"), Some((Utc::now(), None)))
        .await
        .unwrap();
    let r = w.flow(None, claims("s1")).await;
    assert!(matches!(
        r,
        Err(PynError::AccountInactive(pyn_core::AccountStatus::Disabled))
    ));
}

#[tokio::test]
async fn a_provider_identity_links_to_one_account() {
    let w = world_with(|_| {});
    w.register("alice", None).await;
    w.register("bob", None).await;
    w.flow(Some(&who("alice")), claims("s1")).await.unwrap();
    w.flow(Some(&who("alice")), claims("s1")).await.unwrap();
    let r = w.flow(Some(&who("bob")), claims("s1")).await;
    assert!(matches!(r, Err(PynError::ExternalIdentityTaken)));

    let links = w.svc.external_identities(&who("alice")).await.unwrap();
    assert_eq!(links.len(), 1, "linking twice is harmless");
    assert!(
        w.svc
            .external_identities(&who("bob"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn provider_failures_create_nothing() {
    let w = creating();
    let begin = w.start(None).await;
    *w.provider.next.lock().unwrap() =
        Some(Err(PynError::ExternalSignInFailed("bad token".into())));
    let r = w.finish(&begin).await;
    assert!(matches!(r, Err(PynError::ExternalSignInFailed(_))));
    assert!(w.store.list_accounts(None, 10).await.unwrap().is_empty());
    assert!(w.server_events().await.is_empty());
}

#[tokio::test]
async fn linking_and_unlinking_are_audited_and_the_last_way_in_stays() {
    let w = creating();
    let mut c = claims("s1");
    c.username_hint = Some("maya".into());
    w.flow(None, c).await.unwrap();
    let maya = who("maya");
    let only = w.svc.external_identities(&maya).await.unwrap().remove(0);

    let r = w.svc.unlink_external_identity(&maya, &only.id).await;
    assert!(matches!(r, Err(PynError::LastSignInMethod)));
    let r = w.svc.unlink_external_identity(&maya, "nope").await;
    assert!(matches!(r, Err(PynError::ExternalIdentityNotFound(_))));

    w.flow(Some(&maya), claims("s2")).await.unwrap();
    w.svc
        .unlink_external_identity(&maya, &only.id)
        .await
        .unwrap();

    let actions: Vec<_> = w.server_events().await.into_iter().map(|e| e.0).collect();
    assert_eq!(
        actions,
        [
            AuditAction::ExternalAccountCreated,
            AuditAction::ExternalIdentityLinked,
            AuditAction::ExternalIdentityUnlinked
        ]
    );
}

#[tokio::test]
async fn password_sign_in_can_be_turned_off_except_for_administrators() {
    let w = world_with(|p| p.password_sign_in = false);
    w.register("alice", None).await;
    w.register("root", None).await;
    w.store.set_admin(&UserId::new("root"), true).await.unwrap();
    assert!(!w.svc.password_sign_in_enabled());

    for attempt in [
        w.svc.login("alice", PASSWORD, None).await.map(|_| ()),
        w.svc
            .start_session("alice", PASSWORD, None)
            .await
            .map(|_| ()),
    ] {
        assert!(matches!(attempt, Err(PynError::PasswordSignInDisabled)));
    }
    let wrong = w.svc.login("alice", "wrong password here", None).await;
    assert!(
        matches!(wrong, Err(PynError::Unauthenticated(_))),
        "a wrong password still looks like one"
    );
    w.svc.login("root", PASSWORD, None).await.unwrap();
    w.svc.start_session("root", PASSWORD, None).await.unwrap();

    w.flow(Some(&who("alice")), claims("s1")).await.unwrap();
    signed_in(w.flow(None, claims("s1")).await.unwrap());
}

#[tokio::test]
async fn the_password_switch_means_nothing_without_a_provider() {
    let svc = AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        Arc::new(ManualClock::new(Utc::now())),
    )
    .with_config(AccessConfig {
        require_email_verification: false,
        registration: RegistrationMode::Open,
        external: ExternalPolicy {
            password_sign_in: false,
            ..ExternalPolicy::default()
        },
        ..AccessConfig::default()
    });
    svc.register(Registration::new("alice", PASSWORD))
        .await
        .unwrap();
    assert!(svc.password_sign_in_enabled());
    svc.login("alice", PASSWORD, None).await.unwrap();
}

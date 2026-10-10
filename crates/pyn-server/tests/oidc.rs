//! The HTTP contract of external sign-in, with a browser that visits an in-process identity provider.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, ExternalPolicy, RegistrationMode, Repositories,
    Rules, SystemClock, UserId,
};
use pyn_oidc::fake::{CLIENT_ID, CLIENT_SECRET, FakeIdp, Fault, Person};
use pyn_oidc::{HttpOidcProvider, OidcConfig};
use pyn_proto as api;
use pyn_server::auth::BearerAuth;
use pyn_server::{AppState, router};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const WEB: &str = "https://pyn.example";

struct Server {
    app: Router,
    idp: FakeIdp,
    store: Arc<MemoryAccessStore>,
}

async fn server_with(external: impl FnOnce(&mut ExternalPolicy)) -> Server {
    let idp = FakeIdp::start().await;
    let mut config = OidcConfig::new(idp.issuer(), CLIENT_ID, CLIENT_SECRET);
    config.display_name = "Acme SSO".into();
    let provider = Arc::new(HttpOidcProvider::new(config).unwrap());
    build(Some(provider), idp, external)
}

fn build(
    provider: Option<Arc<HttpOidcProvider>>,
    idp: FakeIdp,
    external: impl FnOnce(&mut ExternalPolicy),
) -> Server {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    let mut policy = ExternalPolicy::default();
    external(&mut policy);
    let mut service = AccessService::new(store.clone(), clock.clone())
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            public_url: WEB.into(),
            external: policy,
            ..AccessConfig::default()
        })
        .with_audit(audit.clone());
    if let Some(provider) = provider {
        service = service.with_oidc(provider);
    }
    let access = Arc::new(service);
    let repos = Arc::new(Repositories::new(
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    ));
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: None,
        trust_forwarded: false,
    });
    Server { app, idp, store }
}

struct Reply {
    status: StatusCode,
    location: Option<String>,
    set_cookies: Vec<String>,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }

    fn code(&self) -> String {
        self.json::<api::ErrorBody>().code
    }
}

/// Keeps cookies the way a browser does and follows nothing on its own.
#[derive(Default)]
struct Browser {
    cookies: BTreeMap<String, String>,
}

impl Browser {
    async fn send(&mut self, app: &Router, mut req: Request<Body>) -> Reply {
        if !self.cookies.is_empty() {
            let header = self
                .cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ");
            req.headers_mut().insert("cookie", header.parse().unwrap());
        }
        let r = app.clone().oneshot(req).await.unwrap();
        let set_cookies: Vec<String> = r
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|v| v.to_str().unwrap().to_string())
            .collect();
        for line in &set_cookies {
            let (pair, _) = line.split_once(';').unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            if value.is_empty() {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.into(), value.into());
            }
        }
        Reply {
            status: r.status(),
            location: r
                .headers()
                .get("location")
                .map(|v| v.to_str().unwrap().to_string()),
            set_cookies,
            body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
        }
    }

    async fn get(&mut self, app: &Router, uri: &str) -> Reply {
        self.send(app, Request::get(uri).body(Body::empty()).unwrap())
            .await
    }

    /// Signs in with the provider as `person`; returns the reply to the callback.
    async fn sign_in_with(&mut self, s: &Server, person: &Person, return_to: &str) -> Reply {
        let start = self
            .get(&s.app, &format!("/v1/oidc/authorize?return_to={return_to}"))
            .await;
        assert_eq!(start.status, StatusCode::FOUND, "{}", start.code());
        self.callback(s, start.location.as_deref().unwrap(), person)
            .await
    }

    async fn callback(&mut self, s: &Server, authorization_url: &str, person: &Person) -> Reply {
        let back = s.idp.authorize(authorization_url, person).unwrap();
        let path = back.strip_prefix(WEB).unwrap();
        self.get(&s.app, path).await
    }

    async fn password_sign_in(&mut self, s: &Server, user: &str) -> String {
        let body = serde_json::json!({"username": user, "password": PASSWORD});
        let r = self
            .send(
                &s.app,
                Request::post("/v1/session")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        r.json::<api::SessionInfo>().csrf_token
    }

    async fn signed_in_user(&mut self, s: &Server) -> Option<String> {
        let r = self.get(&s.app, "/v1/session").await;
        (r.status == StatusCode::OK).then(|| r.json::<api::SessionInfo>().user)
    }
}

async fn register(s: &Server, name: &str) {
    let body = serde_json::json!({"username": name, "password": PASSWORD});
    let r = Browser::default()
        .send(
            &s.app,
            Request::post("/v1/register")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
}

fn authed(
    method: &str,
    uri: &str,
    csrf: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(csrf) = csrf {
        req = req.header(api::CSRF_HEADER, csrf);
    }
    req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap()
}

#[tokio::test]
async fn sign_in_options_name_the_provider() {
    let s = server_with(|_| {}).await;
    let r = Browser::default().get(&s.app, "/v1/sign-in-options").await;
    assert_eq!(
        r.json::<api::SignInOptions>(),
        api::SignInOptions {
            password: true,
            external: Some(api::ExternalProviderInfo {
                name: "Acme SSO".into(),
                sign_in_url: "/v1/oidc/authorize".into(),
            }),
        }
    );
}

#[tokio::test]
async fn a_server_without_a_provider_is_unchanged() {
    let s = build(None, FakeIdp::start().await, |_| {});
    let mut b = Browser::default();
    let options = b
        .get(&s.app, "/v1/sign-in-options")
        .await
        .json::<api::SignInOptions>();
    assert_eq!(
        options,
        api::SignInOptions {
            password: true,
            external: None
        }
    );
    let r = b.get(&s.app, "/v1/oidc/authorize").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "oidc_not_configured")
    );
    let r = b.get(&s.app, "/v1/oidc/callback?code=c&state=s").await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert_eq!(
        r.location.as_deref(),
        Some("https://pyn.example/sign-in?error=oidc_not_configured")
    );
    register(&s, "alice").await;
    b.password_sign_in(&s, "alice").await;
}

#[tokio::test]
async fn first_sign_in_creates_an_account_and_opens_the_usual_session() {
    let s = server_with(|p| p.create_accounts = true).await;
    let mut b = Browser::default();
    let start = b.get(&s.app, "/v1/oidc/authorize?return_to=/repos").await;
    assert_eq!(start.status, StatusCode::FOUND);
    assert!(
        start
            .location
            .as_deref()
            .unwrap()
            .starts_with(s.idp.issuer())
    );
    let binding = &start.set_cookies[0];
    for attr in ["pyn_oidc=", "HttpOnly", "SameSite=Lax", "Path=/v1/oidc"] {
        assert!(binding.contains(attr), "{binding}");
    }
    assert!(!binding.contains("Secure"));

    let person = Person::new("sub-maya")
        .username("maya")
        .email("maya@example.org", true);
    let done = b
        .callback(&s, start.location.as_deref().unwrap(), &person)
        .await;
    assert_eq!(done.status, StatusCode::FOUND);
    assert_eq!(done.location.as_deref(), Some("https://pyn.example/repos"));
    let session = done
        .set_cookies
        .iter()
        .find(|c| c.starts_with("pyn_session="))
        .expect("a session cookie");
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(session.contains(attr), "{session}");
    }
    assert!(
        done.set_cookies
            .iter()
            .any(|c| c.starts_with("pyn_oidc=;") && c.contains("Max-Age=0")),
        "the binding cookie is cleared"
    );

    assert_eq!(b.signed_in_user(&s).await.as_deref(), Some("maya"));
    let me = b.get(&s.app, "/v1/me").await;
    assert_eq!(me.json::<api::Account>().user, "maya");

    let no_csrf = b
        .send(
            &s.app,
            authed(
                "POST",
                "/v1/keys",
                None,
                Some(serde_json::json!({"title": "t", "key": "k"})),
            ),
        )
        .await;
    assert_eq!(
        (no_csrf.status, no_csrf.code().as_str()),
        (StatusCode::FORBIDDEN, "csrf_failed")
    );

    let identities = b
        .get(&s.app, "/v1/me/identities")
        .await
        .json::<Vec<api::ExternalIdentityInfo>>();
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].issuer, s.idp.issuer());
    assert_eq!(identities[0].subject, "sub-maya");
    assert_eq!(identities[0].email.as_deref(), Some("maya@example.org"));

    let again = Browser::default().sign_in_with(&s, &person, "/").await;
    assert_eq!(again.location.as_deref(), Some("https://pyn.example/"));
    assert_eq!(s.store.list_accounts(None, 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_binding_cookie_is_secure_behind_https() {
    let s = server_with(|_| {}).await;
    let req = Request::get("/v1/oidc/authorize")
        .header("x-forwarded-proto", "https")
        .body(Body::empty())
        .unwrap();
    let r = Browser::default().send(&s.app, req).await;
    assert!(r.set_cookies[0].contains("Secure"));
}

#[tokio::test]
async fn a_callback_from_another_browser_or_with_no_code_signs_nobody_in() {
    let s = server_with(|p| p.create_accounts = true).await;
    let mut victim = Browser::default();
    let start = victim.get(&s.app, "/v1/oidc/authorize").await;
    let person = Person::new("sub-attacker").username("mallory");
    let back = s
        .idp
        .authorize(start.location.as_deref().unwrap(), &person)
        .unwrap();
    let path = back.strip_prefix(WEB).unwrap();

    let mut other = Browser::default();
    let r = other.get(&s.app, path).await;
    assert_eq!(
        r.location.as_deref(),
        Some("https://pyn.example/sign-in?error=external_sign_in_failed")
    );
    assert!(other.signed_in_user(&s).await.is_none());
    assert!(
        victim
            .get(&s.app, path)
            .await
            .location
            .unwrap()
            .contains("error="),
        "the refused attempt used the flow up"
    );
    assert!(s.store.list_accounts(None, 10).await.unwrap().is_empty());

    for path in [
        "/v1/oidc/callback",
        "/v1/oidc/callback?error=access_denied&state=x",
        "/v1/oidc/callback?code=c&state=unknown",
    ] {
        let r = Browser::default().get(&s.app, path).await;
        assert_eq!(
            r.location.as_deref(),
            Some("https://pyn.example/sign-in?error=external_sign_in_failed"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn a_provider_identity_with_no_account_is_refused_unless_accounts_are_created() {
    let s = server_with(|_| {}).await;
    let r = Browser::default()
        .sign_in_with(&s, &Person::new("sub-1").username("maya"), "/")
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some("https://pyn.example/sign-in?error=external_account_not_linked")
    );
    assert!(!r.set_cookies.iter().any(|c| c.starts_with("pyn_session=")));
    assert!(s.store.list_accounts(None, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn bad_tokens_and_provider_outages_end_on_the_sign_in_page_with_a_code() {
    let s = server_with(|p| p.create_accounts = true).await;
    let person = Person::new("sub-1").username("maya");
    for (fault, code) in [
        (Fault::WrongNonce, "external_sign_in_failed"),
        (Fault::SignedByStranger, "external_sign_in_failed"),
        (Fault::Expired, "external_sign_in_failed"),
    ] {
        s.idp.misbehave(fault);
        let r = Browser::default().sign_in_with(&s, &person, "/").await;
        assert_eq!(
            r.location.as_deref(),
            Some(format!("https://pyn.example/sign-in?error={code}").as_str()),
            "{fault:?}"
        );
    }
    s.idp.misbehave(Fault::None);
    s.idp.reject_tokens_with(503);
    let r = Browser::default().sign_in_with(&s, &person, "/").await;
    assert_eq!(
        r.location.as_deref(),
        Some("https://pyn.example/sign-in?error=external_provider_unavailable")
    );
    assert!(s.store.list_accounts(None, 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn return_to_cannot_leave_the_web_app() {
    let s = server_with(|_| {}).await;
    for bad in ["https://evil.example/", "//evil.example"] {
        let r = Browser::default()
            .get(&s.app, &format!("/v1/oidc/authorize?return_to={bad}"))
            .await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::BAD_REQUEST, "invalid_request"),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn a_signed_in_person_links_the_provider_and_can_unlink_it() {
    let s = server_with(|_| {}).await;
    register(&s, "alice").await;
    let mut b = Browser::default();
    let csrf = b.password_sign_in(&s, "alice").await;

    let refused = b
        .send(&s.app, authed("POST", "/v1/oidc/link", None, None))
        .await;
    assert_eq!(refused.code(), "csrf_failed");
    let anonymous = Browser::default()
        .send(&s.app, authed("POST", "/v1/oidc/link", None, None))
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let link = b
        .send(
            &s.app,
            authed(
                "POST",
                "/v1/oidc/link?return_to=/settings",
                Some(&csrf),
                None,
            ),
        )
        .await;
    assert_eq!(link.status, StatusCode::OK);
    assert!(link.set_cookies[0].starts_with("pyn_oidc="));
    let url = link.json::<api::OidcLink>().url;
    let person = Person::new("sub-alice").email("alice@corp.example", true);
    let done = b.callback(&s, &url, &person).await;
    assert_eq!(
        done.location.as_deref(),
        Some("https://pyn.example/settings")
    );
    assert!(
        !done
            .set_cookies
            .iter()
            .any(|c| c.starts_with("pyn_session=")),
        "linking keeps the session it was started from"
    );

    let identities = b
        .get(&s.app, "/v1/me/identities")
        .await
        .json::<Vec<api::ExternalIdentityInfo>>();
    assert_eq!(identities.len(), 1);

    let mut fresh = Browser::default();
    fresh.sign_in_with(&s, &person, "/").await;
    assert_eq!(fresh.signed_in_user(&s).await.as_deref(), Some("alice"));

    let unlinked = b
        .send(
            &s.app,
            authed(
                "DELETE",
                &format!("/v1/me/identities/{}", identities[0].id),
                Some(&csrf),
                None,
            ),
        )
        .await;
    assert_eq!(unlinked.status, StatusCode::NO_CONTENT);
    let after = Browser::default().sign_in_with(&s, &person, "/").await;
    assert!(
        after
            .location
            .unwrap()
            .ends_with("error=external_account_not_linked")
    );
    let missing = b
        .send(
            &s.app,
            authed("DELETE", "/v1/me/identities/nope", Some(&csrf), None),
        )
        .await;
    assert_eq!(
        (missing.status, missing.code().as_str()),
        (StatusCode::NOT_FOUND, "external_identity_not_found")
    );
}

#[tokio::test]
async fn one_provider_identity_cannot_link_to_two_accounts() {
    let s = server_with(|_| {}).await;
    for name in ["alice", "bob"] {
        register(&s, name).await;
    }
    let person = Person::new("sub-shared");
    for (name, ok) in [("alice", true), ("bob", false)] {
        let mut b = Browser::default();
        let csrf = b.password_sign_in(&s, name).await;
        let link = b
            .send(&s.app, authed("POST", "/v1/oidc/link", Some(&csrf), None))
            .await;
        let done = b
            .callback(&s, &link.json::<api::OidcLink>().url, &person)
            .await;
        let location = done.location.unwrap();
        assert_eq!(location == "https://pyn.example/", ok, "{name}: {location}");
        if !ok {
            assert!(location.ends_with("error=external_identity_taken"));
        }
    }
}

#[tokio::test]
async fn an_account_made_by_the_provider_cannot_drop_its_only_way_in() {
    let s = server_with(|p| p.create_accounts = true).await;
    let mut b = Browser::default();
    b.sign_in_with(&s, &Person::new("sub-1").username("maya"), "/")
        .await;
    let csrf = b
        .get(&s.app, "/v1/session")
        .await
        .json::<api::SessionInfo>()
        .csrf_token;
    let id = b
        .get(&s.app, "/v1/me/identities")
        .await
        .json::<Vec<api::ExternalIdentityInfo>>()[0]
        .id
        .clone();
    let r = b
        .send(
            &s.app,
            authed(
                "DELETE",
                &format!("/v1/me/identities/{id}"),
                Some(&csrf),
                None,
            ),
        )
        .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "last_sign_in_method")
    );
}

#[tokio::test]
async fn turning_password_sign_in_off_spares_administrators() {
    let s = server_with(|p| p.password_sign_in = false).await;
    register(&s, "alice").await;
    register(&s, "root").await;
    s.store.set_admin(&UserId::new("root"), true).await.unwrap();

    let options = Browser::default()
        .get(&s.app, "/v1/sign-in-options")
        .await
        .json::<api::SignInOptions>();
    assert!(!options.password && options.external.is_some());

    let body = serde_json::json!({"username": "alice", "password": PASSWORD});
    for uri in ["/v1/session", "/v1/login"] {
        let r = Browser::default()
            .send(&s.app, authed("POST", uri, None, Some(body.clone())))
            .await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::FORBIDDEN, "password_sign_in_disabled"),
            "{uri}"
        );
    }
    Browser::default().password_sign_in(&s, "root").await;
}

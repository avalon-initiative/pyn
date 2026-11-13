use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, RegistrationMode, Repositories, Rules, SystemClock,
    UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const EMAIL: &str = "secret.person@mail.test";

async fn server() -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    ));
    router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    })
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
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

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", |v| v.to_str().unwrap())
    }
}

/// `who` is a bearer token, a development user name, or empty for no credentials.
async fn send(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", content_type);
    if who.starts_with("pyn_") {
        b = b.header("authorization", format!("Bearer {who}"));
    } else if !who.is_empty() {
        b = b.header(api::DEV_USER_HEADER, who);
    }
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let r = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let (parts, body) = r.into_parts();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn call(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    send(app, who, method, uri, "application/json", bytes, &[]).await
}

async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD, "email": EMAIL});
    let r = call(app, "", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = call(app, "", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.resize(100, 3);
    b
}

fn jpeg() -> Vec<u8> {
    let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0];
    b.resize(100, 9);
    b
}

async fn put_avatar(app: &Router, who: &str, ct: &str, bytes: Vec<u8>) -> Reply {
    send(app, who, "PUT", "/v1/me/avatar", ct, bytes, &[]).await
}

#[tokio::test]
async fn me_reports_whether_the_account_has_a_password() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let me = call(&app, &alice, "GET", "/v1/me", None)
        .await
        .json::<api::Account>();
    assert!(me.has_password);
    assert_eq!(me.profile, api::Profile::default());

    let me = call(&app, "dev-user", "GET", "/v1/me", None)
        .await
        .json::<api::Account>();
    assert!(
        !me.has_password,
        "no password has been set for this account"
    );
}

#[tokio::test]
async fn the_owner_edits_the_profile_and_me_and_the_public_page_show_it() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let body = json!({
        "display_name": "  Alice A.  ",
        "bio": "builds tools",
        "location": "Lisbon",
        "company": "Acme",
        "links": ["https://alice.example", "http://blog.alice.example/x"],
    });
    let r = call(&app, &alice, "PUT", "/v1/me/profile", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let saved = r.json::<api::Profile>();
    assert_eq!(saved.display_name.as_deref(), Some("Alice A."));
    assert_eq!(saved.avatar_url, None);

    let me = call(&app, &alice, "GET", "/v1/me", None)
        .await
        .json::<api::Account>();
    assert_eq!(me.profile, saved);

    let page = call(&app, "", "GET", "/v1/users/alice", None).await;
    assert_eq!(page.status, StatusCode::OK, "signed out");
    let page = page.json::<api::PublicProfile>();
    assert_eq!((page.user.as_str(), page.profile), ("alice", saved));
    assert_eq!(page.id, me.id);

    let r = call(&app, &alice, "PUT", "/v1/me/profile", Some(json!({}))).await;
    assert_eq!(
        r.json::<api::Profile>(),
        api::Profile::default(),
        "replaces all"
    );
}

#[tokio::test]
async fn invalid_profile_fields_are_refused_and_change_nothing() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let good = json!({"display_name": "Alice"});
    call(&app, &alice, "PUT", "/v1/me/profile", Some(good)).await;
    let cases = [
        json!({"display_name": "x".repeat(65)}),
        json!({"display_name": "a\u{1b}b"}),
        json!({"bio": "x".repeat(161)}),
        json!({"location": "x".repeat(65)}),
        json!({"company": "x".repeat(65)}),
        json!({"links": ["javascript:alert(1)"]}),
        json!({"links": ["ftp://x.example"]}),
        json!({"links": ["https://u:p@x.example"]}),
        json!({"links": (0..6).map(|i| format!("https://e{i}.example")).collect::<Vec<_>>()}),
    ];
    for body in cases {
        let r = call(&app, &alice, "PUT", "/v1/me/profile", Some(body.clone())).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::BAD_REQUEST, "invalid_request"),
            "{body}"
        );
    }
    let page = call(&app, "", "GET", "/v1/users/alice", None)
        .await
        .json::<api::PublicProfile>();
    assert_eq!(page.profile.display_name.as_deref(), Some("Alice"));

    let r = call(&app, "", "PUT", "/v1/me/profile", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

async fn repo(app: &Router, who: &str, name: &str, visibility: &str) {
    let body = json!({"name": name, "visibility": visibility});
    let r = call(app, who, "POST", "/v1/repos", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}

fn repo_names(p: &api::PublicProfile) -> Vec<&str> {
    p.repositories.iter().map(|r| r.name.as_str()).collect()
}

#[tokio::test]
async fn a_stranger_never_sees_a_private_repository_anywhere_in_a_profile() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    let carol = token_for(&app, "carol").await;
    repo(&app, &alice, "open", "public").await;
    repo(&app, &alice, "hidden-plans", "private").await;
    repo(&app, &alice, "team-only", "private").await;
    let r = call(
        &app,
        &alice,
        "PUT",
        "/v1/repos/alice/team-only/members/bob",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());

    for viewer in ["", carol.as_str()] {
        let r = call(&app, viewer, "GET", "/v1/users/alice", None).await;
        let text = r.text();
        assert!(
            !text.contains("hidden-plans") && !text.contains("team-only"),
            "{text}"
        );
        let p = r.json::<api::PublicProfile>();
        assert_eq!(repo_names(&p), ["open"]);
        assert_eq!(p.repository_count, 1, "counts leak nothing either");
    }
    let p = call(&app, &bob, "GET", "/v1/users/alice", None)
        .await
        .json::<api::PublicProfile>();
    assert_eq!(
        repo_names(&p),
        ["open", "team-only"],
        "a member sees theirs"
    );
    assert_eq!(p.repository_count, 2);
    assert_eq!(p.repositories[1].role.as_deref(), Some("reader"));
    assert!(!p.repositories.iter().any(|r| r.name == "hidden-plans"));

    let p = call(&app, &alice, "GET", "/v1/users/alice", None)
        .await
        .json::<api::PublicProfile>();
    assert_eq!(repo_names(&p), ["hidden-plans", "open", "team-only"]);
    assert_eq!(p.repository_count, 3);

    let hidden = call(&app, &carol, "GET", "/v1/repos/alice/hidden-plans", None).await;
    assert_eq!(
        (hidden.status, hidden.code().as_str()),
        (StatusCode::NOT_FOUND, "repo_not_found")
    );
}

#[tokio::test]
async fn organizations_show_only_to_viewers_who_share_them() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    let carol = token_for(&app, "carol").await;
    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    call(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/members",
        Some(json!({"user": "bob", "role": "member"})),
    )
    .await;
    let r = call(&app, &bob, "POST", "/v1/me/invitations/acme/accept", None).await;
    assert!(r.status.is_success(), "{}", r.text());

    let orgs = |p: Reply| -> Vec<String> {
        p.json::<api::PublicProfile>()
            .organizations
            .into_iter()
            .map(|o| o.name)
            .collect()
    };
    assert!(orgs(call(&app, "", "GET", "/v1/users/alice", None).await).is_empty());
    assert!(orgs(call(&app, &carol, "GET", "/v1/users/alice", None).await).is_empty());
    assert_eq!(
        orgs(call(&app, &bob, "GET", "/v1/users/alice", None).await),
        ["acme"]
    );
    assert_eq!(
        orgs(call(&app, &alice, "GET", "/v1/users/alice", None).await),
        ["acme"]
    );
}

#[tokio::test]
async fn a_disabled_missing_or_organization_account_has_no_profile() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    token_for(&app, "bob").await;
    call(
        &app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    put_avatar(&app, &alice, "image/png", png()).await;
    let r = call(
        &app,
        "root",
        "POST",
        "/v1/admin/users/bob/disable",
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let missing = call(&app, "", "GET", "/v1/users/nobody", None).await;
    assert_eq!(
        (missing.status, missing.code().as_str()),
        (StatusCode::NOT_FOUND, "user_not_found")
    );
    for name in ["bob", "acme"] {
        let r = call(&app, &alice, "GET", &format!("/v1/users/{name}"), None).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::NOT_FOUND, "user_not_found"),
            "{name}"
        );
        let r = call(&app, "", "GET", &format!("/v1/users/{name}/avatar"), None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{name}");
    }
}

#[tokio::test]
async fn an_avatar_is_uploaded_served_cached_replaced_and_removed() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;

    let r = call(&app, "", "GET", "/v1/users/alice/avatar", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "avatar_not_found")
    );

    let r = put_avatar(&app, &alice, "image/png", png()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let url = r.json::<api::Profile>().avatar_url.expect("avatar url");
    assert!(url.starts_with("/v1/users/alice/avatar?v="), "{url}");

    let r = call(&app, "", "GET", &url, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, png());
    assert_eq!(r.header("content-type"), "image/png");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert!(r.header("cache-control").contains("immutable"));
    let etag = r.header("etag").to_string();
    assert!(etag.starts_with('"') && etag.len() == 66, "{etag}");

    let r = call(&app, "", "GET", "/v1/users/alice/avatar?v=stale", None).await;
    assert!(!r.header("cache-control").contains("immutable"));
    assert!(r.header("cache-control").contains("max-age"));
    let r = send(
        &app,
        "",
        "GET",
        "/v1/users/alice/avatar",
        "",
        vec![],
        &[("if-none-match", &etag)],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);

    let r = put_avatar(&app, &alice, "image/png", jpeg()).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the header is not trusted: {}",
        r.text()
    );
    let new_url = r.json::<api::Profile>().avatar_url.unwrap();
    assert_ne!(new_url, url, "a new image is a new url");
    let r = call(&app, "", "GET", &new_url, None).await;
    assert_eq!(r.header("content-type"), "image/jpeg");
    assert_eq!(r.body, jpeg());

    let r = call(&app, &alice, "DELETE", "/v1/me/avatar", None).await;
    assert_eq!(r.json::<api::Profile>().avatar_url, None);
    let r = call(&app, "", "GET", "/v1/users/alice/avatar", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(&app, &alice, "DELETE", "/v1/me/avatar", None).await;
    assert_eq!(r.status, StatusCode::OK, "removing none is harmless");
}

#[tokio::test]
async fn avatars_are_sniffed_and_bounded() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    for (name, bytes) in [
        ("gif", b"GIF89a\x01\x00\x01\x00\x80\x00\x00".to_vec()),
        (
            "svg",
            b"<svg xmlns='http://www.w3.org/2000/svg'><script>1</script></svg>".to_vec(),
        ),
        ("html", b"<html></html>".to_vec()),
        ("truncated png", b"\x89PNG\r\n\x1a\n".to_vec()),
    ] {
        let r = put_avatar(&app, &alice, "image/png", bytes).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_image"),
            "{name}"
        );
    }
    let r = put_avatar(&app, &alice, "image/png", vec![]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let mut big = png();
    big.resize((1 << 20) + 1, 0);
    let r = put_avatar(&app, &alice, "image/png", big).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, "object_too_large")
    );

    let mut max = png();
    max.resize(1 << 20, 0);
    let r = put_avatar(&app, &alice, "image/png", max).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "exactly 1 MiB is allowed: {}",
        r.text()
    );

    let r = put_avatar(&app, "", "image/png", png()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn profile_responses_never_carry_an_email_address() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    put_avatar(&app, &alice, "image/png", png()).await;
    repo(&app, &alice, "open", "public").await;
    for (who, uri) in [
        ("", "/v1/users/alice"),
        (alice.as_str(), "/v1/users/alice"),
        (alice.as_str(), "/v1/me"),
    ] {
        let text = call(&app, who, "GET", uri, None).await.text();
        assert!(
            !text.contains("secret.person") && !text.contains("mail.test"),
            "{uri}: {text}"
        );
        assert!(!text.contains("\"email"), "{uri}: {text}");
    }
}

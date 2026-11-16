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
    assert_eq!(me.has_password, Some(true));
    assert_eq!(
        me.profile,
        api::Profile {
            visibility: Some(api::ProfileVisibility::default()),
            ..Default::default()
        }
    );

    let me = call(&app, "dev-user", "GET", "/v1/me", None)
        .await
        .json::<api::Account>();
    assert!(
        me.has_password == Some(false),
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
    let public = api::Profile {
        visibility: None,
        ..saved.clone()
    };
    assert_eq!((page.user.as_str(), page.profile), ("alice", public));
    assert_eq!(saved.visibility, Some(api::ProfileVisibility::default()));
    assert_eq!(page.id, me.id);

    let r = call(&app, &alice, "PUT", "/v1/me/profile", Some(json!({}))).await;
    assert_eq!(
        r.json::<api::Profile>(),
        api::Profile {
            visibility: Some(api::ProfileVisibility::default()),
            ..Default::default()
        },
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

#[tokio::test]
async fn lists_of_people_carry_their_public_identity() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    call(
        &app,
        &alice,
        "PUT",
        "/v1/me/profile",
        Some(json!({"display_name": "Alice A."})),
    )
    .await;
    assert!(
        put_avatar(&app, &alice, "image/png", png())
            .await
            .status
            .is_success()
    );
    let org = Some(json!({"name": "acme"}));
    call(&app, &alice, "POST", "/v1/orgs", org).await;
    let invite = json!({"user": "bob", "role": "member"});
    call(&app, &alice, "POST", "/v1/orgs/acme/members", Some(invite)).await;
    call(&app, &bob, "POST", "/v1/me/invitations/acme/accept", None).await;

    let r = call(&app, &alice, "GET", "/v1/orgs/acme/members", None).await;
    let members = r.json::<Vec<api::OrgMember>>();
    let of = |n: &str| {
        members
            .iter()
            .find(|m| m.user == n)
            .unwrap()
            .identity
            .clone()
    };
    let a = of("alice").expect("alice has an identity");
    assert_eq!(a.display_name.as_deref(), Some("Alice A."));
    assert!(
        a.avatar_url
            .unwrap()
            .starts_with("/v1/users/alice/avatar?v=")
    );
    let b = of("bob").unwrap();
    assert_eq!((b.display_name, b.avatar_url), (None, None));

    repo(&app, &alice, "r", "private").await;
    let r = call(&app, &alice, "GET", "/v1/repos/alice/r/members", None).await;
    let members = r.json::<Vec<api::Member>>();
    assert_eq!(members[0].identity.as_ref().unwrap().user, "alice");

    let r = call(&app, &alice, "GET", "/v1/orgs/acme/audit", None).await;
    let page = r.json::<api::AuditPage>();
    let first = page.entries.iter().find(|e| e.actor == "alice").unwrap();
    let who = first.actor_public.as_ref().expect("actor identity");
    assert_eq!(who.display_name.as_deref(), Some("Alice A."));
}

#[tokio::test]
async fn hidden_fields_are_absent_for_others_and_in_lists_but_the_owner_sees_them() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    let body = json!({
        "display_name": "Alice A.", "bio": "builds tools", "location": "Lisbon",
        "links": ["https://alice.example"],
        "visibility": {"display_name": false, "bio": false, "links": false, "avatar": false},
    });
    let r = call(&app, &alice, "PUT", "/v1/me/profile", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let vis = r.json::<api::Profile>().visibility.unwrap();
    assert!(!vis.display_name && !vis.avatar && vis.location && vis.company);
    put_avatar(&app, &alice, "image/png", png()).await;

    for viewer in ["", bob.as_str(), "root"] {
        let page = call(&app, viewer, "GET", "/v1/users/alice", None).await;
        let p = page.json::<api::PublicProfile>().profile;
        assert_eq!((p.display_name, p.bio), (None, None), "viewer {viewer:?}");
        assert!(p.links.is_empty() && p.avatar_url.is_none() && p.visibility.is_none());
        assert_eq!(p.location.as_deref(), Some("Lisbon"));
        let r = call(&app, viewer, "GET", "/v1/users/alice/avatar", None).await;
        assert_eq!(r.code(), "avatar_not_found", "viewer {viewer:?}");
    }

    let own = call(&app, &alice, "GET", "/v1/users/alice", None).await;
    let own = own.json::<api::PublicProfile>().profile;
    assert_eq!(own.display_name.as_deref(), Some("Alice A."));
    assert!(own.avatar_url.is_some() && own.visibility == Some(vis.clone()));
    let r = call(&app, &alice, "GET", "/v1/users/alice/avatar", None).await;
    assert_eq!(r.status, StatusCode::OK);

    call(
        &app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    let invite = json!({"user": "bob", "role": "member"});
    call(&app, &alice, "POST", "/v1/orgs/acme/members", Some(invite)).await;
    call(&app, &bob, "POST", "/v1/me/invitations/acme/accept", None).await;
    let r = call(&app, &bob, "GET", "/v1/orgs/acme/members", None).await;
    let members = r.json::<Vec<api::OrgMember>>();
    let a = members.iter().find(|m| m.user == "alice").unwrap();
    let a = a.identity.clone().expect("alice is still an account");
    assert_eq!((a.display_name, a.avatar_url), (None, None));

    let r = call(
        &app,
        &alice,
        "PUT",
        "/v1/me/profile",
        Some(json!({"bio": "x"})),
    )
    .await;
    assert_eq!(
        r.json::<api::Profile>().visibility,
        Some(vis),
        "kept when omitted"
    );
}

async fn put_org_avatar(app: &Router, who: &str, bytes: Vec<u8>) -> Reply {
    send(
        app,
        who,
        "PUT",
        "/v1/orgs/acme/avatar",
        "image/png",
        bytes,
        &[],
    )
    .await
}

async fn put_org_profile(app: &Router, who: &str, body: serde_json::Value) -> Reply {
    call(app, who, "PUT", "/v1/orgs/acme/profile", Some(body)).await
}

async fn org_with_owner_and_member(app: &Router) -> (String, String, String) {
    let (alice, bob, carol) = (
        token_for(app, "alice").await,
        token_for(app, "bob").await,
        token_for(app, "carol").await,
    );
    let r = call(
        app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let invite = json!({"user": "bob", "role": "member"});
    call(app, &alice, "POST", "/v1/orgs/acme/members", Some(invite)).await;
    call(app, &bob, "POST", "/v1/me/invitations/acme/accept", None).await;
    (alice, bob, carol)
}

#[tokio::test]
async fn an_organization_profile_is_edited_by_owners_and_read_by_signed_in_accounts() {
    let app = server().await;
    let (alice, bob, carol) = org_with_owner_and_member(&app).await;
    let uri = "/v1/orgs/acme/profile";

    let r = call(&app, &carol, "GET", uri, None).await;
    let p = r.json::<api::OrgProfile>();
    assert_eq!(
        (p.name.as_str(), p.description, p.avatar_url),
        ("acme", None, None)
    );
    assert_eq!(
        call(&app, "", "GET", uri, None).await.status,
        StatusCode::UNAUTHORIZED
    );

    let r = put_org_profile(
        &app,
        &alice,
        json!({"description": "  We make tools\nand more "}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let p = call(&app, &carol, "GET", uri, None)
        .await
        .json::<api::OrgProfile>();
    assert_eq!(p.description.as_deref(), Some("We make tools\nand more"));

    for who in [&bob, &carol] {
        let r = put_org_profile(&app, who, json!({"description": "x"})).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::FORBIDDEN, "not_org_owner")
        );
    }
    let r = put_org_profile(&app, &alice, json!({"description": "d".repeat(257)})).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );
    let p = call(&app, &carol, "GET", uri, None)
        .await
        .json::<api::OrgProfile>();
    assert_eq!(
        p.description.as_deref(),
        Some("We make tools\nand more"),
        "refused edits change nothing"
    );

    let r = put_org_profile(&app, &alice, json!({"description": " "})).await;
    assert_eq!(r.json::<api::OrgProfile>().description, None);
    for name in ["nobody", "alice"] {
        let r = call(
            &app,
            &carol,
            "GET",
            &format!("/v1/orgs/{name}/profile"),
            None,
        )
        .await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::NOT_FOUND, "org_not_found")
        );
    }
}

#[tokio::test]
async fn an_organization_avatar_is_uploaded_served_and_removed_by_owners_only() {
    let app = server().await;
    let (alice, bob, _) = org_with_owner_and_member(&app).await;

    let r = call(&app, "", "GET", "/v1/orgs/acme/avatar", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "avatar_not_found")
    );
    let r = put_org_avatar(&app, &bob, png()).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "not_org_owner")
    );

    let r = put_org_avatar(&app, &alice, png()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let url = r.json::<api::OrgProfile>().avatar_url.expect("avatar url");
    assert!(url.starts_with("/v1/orgs/acme/avatar?v="), "{url}");
    let r = call(&app, "", "GET", &url, None).await;
    assert_eq!(r.status, StatusCode::OK, "signed out");
    assert_eq!(r.body, png());
    assert_eq!(r.header("content-type"), "image/png");
    assert!(r.header("cache-control").contains("immutable"));
    let etag = r.header("etag").to_string();
    let r = send(
        &app,
        "",
        "GET",
        &url,
        "",
        vec![],
        &[("if-none-match", &etag)],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);

    for (bytes, status, code) in [
        (
            b"GIF89a....".to_vec(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_image",
        ),
        (vec![], StatusCode::BAD_REQUEST, "invalid_request"),
        (
            vec![0; (1 << 20) + 1],
            StatusCode::PAYLOAD_TOO_LARGE,
            "object_too_large",
        ),
    ] {
        let r = put_org_avatar(&app, &alice, bytes).await;
        assert_eq!((r.status, r.code().as_str()), (status, code));
    }
    let r = put_org_avatar(&app, &alice, jpeg()).await;
    assert_ne!(r.json::<api::OrgProfile>().avatar_url, Some(url));

    let r = call(&app, &bob, "DELETE", "/v1/orgs/acme/avatar", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(&app, &alice, "DELETE", "/v1/orgs/acme/avatar", None).await;
    assert_eq!(r.json::<api::OrgProfile>().avatar_url, None);
    let r = call(&app, "", "GET", "/v1/orgs/acme/avatar", None).await;
    assert_eq!(r.code(), "avatar_not_found");
    let r = call(&app, "", "GET", "/v1/orgs/nobody/avatar", None).await;
    assert_eq!(r.code(), "org_not_found");
}

#[tokio::test]
async fn org_owned_repositories_carry_the_organizations_identity() {
    let app = server().await;
    let (alice, _, _) = org_with_owner_and_member(&app).await;
    let body = json!({"owner": "acme", "name": "tools"});
    let r = call(&app, &alice, "POST", "/v1/repos", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        r.json::<api::RepoInfo>().owner_identity.unwrap().avatar_url,
        None
    );
    let own = json!({"name": "mine"});
    let r = call(&app, &alice, "POST", "/v1/repos", Some(own)).await;
    assert_eq!(
        r.json::<api::RepoInfo>().owner_identity,
        None,
        "a user owner has none"
    );

    send(
        &app,
        &alice,
        "PUT",
        "/v1/orgs/acme/avatar",
        "image/png",
        png(),
        &[],
    )
    .await;
    let r = call(&app, &alice, "GET", "/v1/repos/acme/tools", None).await;
    let id = r
        .json::<api::RepoInfo>()
        .owner_identity
        .expect("org identity");
    assert_eq!(id.user, "acme");
    assert_eq!(id.display_name, None);
    assert!(
        id.avatar_url
            .unwrap()
            .starts_with("/v1/orgs/acme/avatar?v=")
    );

    let r = call(&app, &alice, "GET", "/v1/repos", None).await;
    let repos = r.json::<Vec<api::RepoInfo>>();
    let acme: Vec<_> = repos.iter().filter(|r| r.owner == "acme").collect();
    assert!(acme.iter().all(|r| r.owner_identity.is_some()));
    assert!(
        repos
            .iter()
            .filter(|r| r.owner == "alice")
            .all(|r| r.owner_identity.is_none())
    );
}

#[tokio::test]
async fn org_audit_entries_and_members_carry_public_identities() {
    let app = server().await;
    let (alice, _, _) = org_with_owner_and_member(&app).await;
    let put = json!({"display_name": "Alice A", "links": []});
    call(&app, &alice, "PUT", "/v1/me/profile", Some(put)).await;
    let r = call(&app, &alice, "GET", "/v1/orgs/acme/audit", None).await;
    let page = r.json::<api::AuditPage>();
    assert!(!page.entries.is_empty());
    for e in page.entries.iter().filter(|e| e.actor == "alice") {
        let who = e.actor_public.clone().expect("annotated");
        assert_eq!(who.display_name.as_deref(), Some("Alice A"));
    }
    let r = call(&app, &alice, "GET", "/v1/orgs/acme/members", None).await;
    let members = r.json::<Vec<api::OrgMember>>();
    assert!(members.iter().all(|m| m.identity.is_some()));
}

async fn people(app: &Router, who: &str, query: &str) -> Reply {
    call(app, who, "GET", &format!("/v1/users{query}"), None).await
}

fn people_names(r: &Reply) -> Vec<String> {
    r.json::<api::PeoplePage>()
        .people
        .into_iter()
        .map(|p| p.identity.user)
        .collect()
}

#[tokio::test]
async fn the_people_directory_needs_a_sign_in_and_lists_active_accounts_by_name() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    token_for(&app, "bob").await;
    token_for(&app, "carol").await;
    call(
        &app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    call(
        &app,
        "root",
        "POST",
        "/v1/admin/users/carol/disable",
        Some(json!({})),
    )
    .await;

    let r = people(&app, "", "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = people(&app, "pyn_bogus", "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    let r = people(&app, &alice, "").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let names = people_names(&r);
    assert!(
        names.iter().all(|n| n != "carol" && n != "acme"),
        "{names:?}"
    );
    assert!(names.contains(&"alice".to_string()) && names.contains(&"bob".to_string()));
    assert!(names.windows(2).all(|w| w[0] < w[1]), "ordered by name");
    assert!(
        !r.text().contains("mail.test") && !r.text().contains("email"),
        "never an email address: {}",
        r.text()
    );

    let first = people(&app, &alice, "?limit=1")
        .await
        .json::<api::PeoplePage>();
    assert_eq!(first.people.len(), 1);
    let cursor = first.next_after.expect("more pages");
    let rest = people(&app, &alice, &format!("?limit=100&after={cursor}")).await;
    assert!(people_names(&rest).iter().all(|n| *n > cursor));
    assert_eq!(
        people(&app, &alice, &format!("?q={}", "x".repeat(65)))
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn a_hidden_display_name_is_neither_listed_nor_searchable() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    let body = |name: &str, show: bool| {
        json!({"display_name": name, "bio": "Quartz fan", "links": [],
               "visibility": {"display_name": show, "bio": false}})
    };
    call(
        &app,
        &alice,
        "PUT",
        "/v1/me/profile",
        Some(body("Ada Lovelace", true)),
    )
    .await;
    call(
        &app,
        &bob,
        "PUT",
        "/v1/me/profile",
        Some(body("Hidden Hopper", false)),
    )
    .await;

    let r = people(&app, &alice, "?q=lovelace").await;
    let page = r.json::<api::PeoplePage>();
    assert_eq!(page.people.len(), 1);
    assert_eq!(page.people[0].identity.user, "alice");
    assert_eq!(
        page.people[0].identity.display_name.as_deref(),
        Some("Ada Lovelace")
    );

    for viewer in [alice.as_str(), bob.as_str()] {
        for q in ["?q=hopper", "?q=hidden%20hopper", "?q=quartz"] {
            let r = people(&app, viewer, q).await;
            assert!(people_names(&r).is_empty(), "{q}: {}", r.text());
        }
    }
    let all = people(&app, &alice, "").await;
    assert!(!all.text().contains("Hopper") && !all.text().contains("Quartz"));
    let bob_entry = all
        .json::<api::PeoplePage>()
        .people
        .into_iter()
        .find(|p| p.identity.user == "bob")
        .unwrap();
    assert_eq!(
        (bob_entry.identity.display_name, bob_entry.bio),
        (None, None)
    );
    assert_eq!(people_names(&people(&app, &alice, "?q=BOB").await), ["bob"]);
}

#[tokio::test]
async fn a_deleted_or_scheduled_account_leaves_the_directory() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    assert!(people_names(&people(&app, &alice, "?q=bob").await).contains(&"bob".to_string()));
    let r = call(
        &app,
        &bob,
        "POST",
        "/v1/account/deletion",
        Some(json!({"password": PASSWORD})),
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.text());
    assert!(people_names(&people(&app, &alice, "?q=bob").await).is_empty());
    assert!(!people_names(&people(&app, &alice, "").await).contains(&"bob".to_string()));

    token_for(&app, "dora").await;
    call(
        &app,
        "root",
        "POST",
        "/v1/admin/users/dora/disable",
        Some(json!({})),
    )
    .await;
    let r = call(
        &app,
        "root",
        "POST",
        "/v1/admin/users/dora/delete",
        Some(json!({"reason": "spam"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert!(people_names(&people(&app, &alice, "?q=dora").await).is_empty());
}

#[tokio::test]
async fn a_private_repository_is_in_no_list_search_or_count_and_its_error_matches_a_missing_one() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let carol = token_for(&app, "carol").await;
    repo(&app, &alice, "open", "public").await;
    repo(&app, &alice, "vault-zq", "private").await;

    for viewer in ["", carol.as_str()] {
        for uri in [
            "/v1/users/alice",
            "/v1/repos?owner=alice",
            "/v1/explore/repos",
            "/v1/users?q=vault",
            "/v1/users?q=alice",
        ] {
            let r = call(&app, viewer, "GET", uri, None).await;
            if r.status == StatusCode::UNAUTHORIZED {
                continue;
            }
            assert!(r.status.is_success(), "{uri}: {}", r.text());
            assert!(!r.text().contains("vault-zq"), "{uri}: {}", r.text());
        }
    }
    let page = call(&app, &carol, "GET", "/v1/users/alice", None)
        .await
        .json::<api::PublicProfile>();
    assert_eq!(
        (repo_names(&page), page.repository_count),
        (vec!["open"], 1)
    );
    let entry = people(&app, &carol, "?q=alice").await.text();
    assert!(
        !entry.contains("repositor"),
        "no repository data in the directory: {entry}"
    );

    for uri in ["", "/files", "/locks", "/summary", "/branches"] {
        for viewer in ["", carol.as_str()] {
            let hidden = call(
                &app,
                viewer,
                "GET",
                &format!("/v1/repos/alice/vault-zq{uri}"),
                None,
            )
            .await;
            let missing = call(
                &app,
                viewer,
                "GET",
                &format!("/v1/repos/alice/nope-zq{uri}"),
                None,
            )
            .await;
            let (h, m) = (
                hidden.json::<api::ErrorBody>(),
                missing.json::<api::ErrorBody>(),
            );
            assert_eq!(hidden.status, missing.status, "{uri}");
            assert_eq!(h.code, m.code, "{uri}");
            assert_eq!(
                h.message.replace("vault-zq", "X"),
                m.message.replace("nope-zq", "X"),
                "{uri}"
            );
            assert_eq!(
                hidden.header("content-type"),
                missing.header("content-type")
            );
        }
    }
}

#[tokio::test]
async fn the_people_directory_needs_a_full_scope_credential() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    repo(&app, &alice, "game", "private").await;
    let make = |body: serde_json::Value| {
        let (app, alice) = (app.clone(), alice.clone());
        async move {
            let r = call(&app, &alice, "POST", "/v1/tokens", Some(body)).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text());
            r.json::<api::CreatedToken>().token
        }
    };
    let scoped =
        make(json!({"name": "ci", "permissions": ["read"], "repos": ["alice/game"]})).await;
    let every_permission = make(json!({"name": "all", "repos": ["alice/game"], "permissions": ["read", "lock", "checkin", "restore", "force_unlock", "edit_policy", "manage_users", "manage_roles", "view_audit"]})).await;
    let r = call(
        &app,
        "root",
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": "svc", "scopes": ["manage_accounts"]})),
    )
    .await;
    let svc = r.json::<api::CreatedServiceCredential>().secret;
    let body = json!({"username": "alice", "password": PASSWORD});
    let r = call(&app, "", "POST", "/v1/session", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let set = r.header("set-cookie").to_string();
    let cookie = set.split(';').next().unwrap().to_string();

    let session = send(
        &app,
        "",
        "GET",
        "/v1/users",
        "application/json",
        vec![],
        &[("cookie", &cookie)],
    )
    .await;
    for (kind, r) in [
        ("user session", session),
        ("full-scope token", people(&app, &alice, "").await),
        ("dev token", people(&app, "alice", "").await),
    ] {
        assert_eq!(r.status, StatusCode::OK, "{kind}: {}", r.text());
        assert!(people_names(&r).contains(&"alice".to_string()), "{kind}");
    }

    for (kind, who, code) in [
        ("repository-scoped token", &scoped, "forbidden"),
        (
            "every permission on one repository",
            &every_permission,
            "forbidden",
        ),
    ] {
        let r = people(&app, who, "").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{kind}: {}", r.text());
        assert_eq!(r.code(), code, "{kind}");
    }
    let bearer = format!("Bearer {svc}");
    let r = send(
        &app,
        "",
        "GET",
        "/v1/users",
        "application/json",
        vec![],
        &[("authorization", &bearer)],
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.code(), "service_credential_not_allowed");
    for (kind, who) in [("signed out", ""), ("invalid token", "pyn_bogus")] {
        let r = people(&app, who, "").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{kind}");
    }
}

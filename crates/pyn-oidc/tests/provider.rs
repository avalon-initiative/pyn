//! The OpenID Connect client against an in-process provider on a loopback port.

use pyn_core::{
    AuthorizationRequest, CodeExchange, ExternalClaims, OidcProvider, PynError, pkce_challenge,
};
use pyn_oidc::fake::{CLIENT_ID, CLIENT_SECRET, FakeIdp, Fault, KeyKind, Person};
use pyn_oidc::{HttpOidcProvider, OidcConfig};

const REDIRECT: &str = "https://pyn.example/v1/oidc/callback";
const VERIFIER: &str = "verifier-0123456789-0123456789-0123456789-0123456789";
const NONCE: &str = "nonce-1";

fn provider(idp: &FakeIdp) -> HttpOidcProvider {
    HttpOidcProvider::new(OidcConfig::new(idp.issuer(), CLIENT_ID, CLIENT_SECRET)).unwrap()
}

async fn code_for(idp: &FakeIdp, p: &HttpOidcProvider, person: &Person) -> String {
    let url = p
        .authorization_url(&AuthorizationRequest {
            state: "state-1",
            nonce: NONCE,
            code_challenge: &pkce_challenge(VERIFIER),
            redirect_uri: REDIRECT,
        })
        .await
        .unwrap();
    let back = idp.authorize(&url, person).unwrap();
    let back = reqwest::Url::parse(&back).unwrap();
    let pairs: std::collections::HashMap<_, _> = back.query_pairs().into_owned().collect();
    assert_eq!(pairs["state"], "state-1");
    pairs["code"].clone()
}

async fn sign_in(
    idp: &FakeIdp,
    p: &HttpOidcProvider,
    person: &Person,
) -> pyn_core::Result<ExternalClaims> {
    let code = code_for(idp, p, person).await;
    p.exchange(&CodeExchange {
        code: &code,
        code_verifier: VERIFIER,
        redirect_uri: REDIRECT,
        nonce: NONCE,
    })
    .await
}

#[tokio::test]
async fn a_valid_id_token_yields_its_claims_with_either_key_type() {
    for kind in [KeyKind::Rsa, KeyKind::Ec] {
        let idp = FakeIdp::start_with(kind).await;
        let p = provider(&idp);
        let person = Person::new("sub-1")
            .username("maya")
            .email("maya@example.org", true);
        let claims = sign_in(&idp, &p, &person).await.unwrap();
        assert_eq!(
            claims,
            ExternalClaims {
                issuer: idp.issuer().into(),
                subject: "sub-1".into(),
                email: Some("maya@example.org".into()),
                email_verified: true,
                username_hint: Some("maya".into()),
            },
            "{kind:?}"
        );
    }
}

#[tokio::test]
async fn email_verified_may_be_a_string_and_is_false_when_absent() {
    let idp = FakeIdp::start().await;
    let p = provider(&idp);
    let mut person = Person::new("s").email("a@example.org", false);
    person.email_verified = serde_json::json!("true");
    assert!(sign_in(&idp, &p, &person).await.unwrap().email_verified);
    person.email_verified = serde_json::Value::Null;
    assert!(!sign_in(&idp, &p, &person).await.unwrap().email_verified);
}

#[tokio::test]
async fn the_authorization_request_asks_for_code_pkce_state_and_nonce() {
    let idp = FakeIdp::start().await;
    let p = provider(&idp);
    let url = p
        .authorization_url(&AuthorizationRequest {
            state: "st",
            nonce: "no",
            code_challenge: "ch",
            redirect_uri: REDIRECT,
        })
        .await
        .unwrap();
    let url = reqwest::Url::parse(&url).unwrap();
    let q: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.path(), "/authorize");
    for (k, v) in [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT),
        ("state", "st"),
        ("nonce", "no"),
        ("code_challenge", "ch"),
        ("code_challenge_method", "S256"),
        ("scope", "openid profile email"),
    ] {
        assert_eq!(q[k], v, "{k}");
    }
}

#[tokio::test]
async fn every_flaw_in_an_id_token_is_refused_for_its_own_reason() {
    for (fault, reason) in [
        (Fault::WrongNonce, "does not answer"),
        (Fault::NoNonce, "does not answer"),
        (Fault::WrongAudience, "InvalidAudience"),
        (Fault::WrongIssuer, "InvalidIssuer"),
        (Fault::Expired, "ExpiredSignature"),
        (Fault::SignedByStranger, "InvalidSignature"),
        (Fault::AlgNone, "malformed"),
        (Fault::NoSubject, "MissingRequiredClaim"),
        (Fault::NoIdToken, "no ID token"),
        (Fault::MultipleAudiences, "another client"),
    ] {
        let idp = FakeIdp::start().await;
        let p = provider(&idp);
        idp.misbehave(fault);
        let r = sign_in(&idp, &p, &Person::new("sub-1")).await;
        assert!(
            matches!(&r, Err(PynError::ExternalSignInFailed(m)) if m.contains(reason)),
            "{fault:?}: {r:?}"
        );
    }
}

#[tokio::test]
async fn the_token_endpoint_checks_pkce_and_a_code_works_once() {
    let idp = FakeIdp::start().await;
    let p = provider(&idp);
    let code = code_for(&idp, &p, &Person::new("s")).await;
    let exchange = |code: &str, verifier: &str| {
        let (code, verifier) = (code.to_string(), verifier.to_string());
        let p = &p;
        async move {
            p.exchange(&CodeExchange {
                code: &code,
                code_verifier: &verifier,
                redirect_uri: REDIRECT,
                nonce: NONCE,
            })
            .await
        }
    };
    let r = exchange(&code, "a-different-verifier-0123456789-0123456789-0123").await;
    assert!(matches!(r, Err(PynError::ExternalSignInFailed(m)) if m.contains("invalid_grant")));
    let r = exchange(&code, VERIFIER).await;
    assert!(
        matches!(r, Err(PynError::ExternalSignInFailed(_))),
        "a refused attempt uses the code up"
    );
}

#[tokio::test]
async fn provider_outages_are_reported_as_unavailable() {
    let idp = FakeIdp::start().await;
    let p = provider(&idp);
    let code = code_for(&idp, &p, &Person::new("s")).await;
    idp.reject_tokens_with(503);
    let r = p
        .exchange(&CodeExchange {
            code: &code,
            code_verifier: VERIFIER,
            redirect_uri: REDIRECT,
            nonce: NONCE,
        })
        .await;
    assert!(
        matches!(r, Err(PynError::ExternalProviderUnavailable(_))),
        "{r:?}"
    );

    let gone = {
        let idp = FakeIdp::start().await;
        provider(&idp)
    };
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let r = gone
        .authorization_url(&AuthorizationRequest {
            state: "s",
            nonce: "n",
            code_challenge: "c",
            redirect_uri: REDIRECT,
        })
        .await;
    assert!(
        matches!(r, Err(PynError::ExternalProviderUnavailable(_))),
        "{r:?}"
    );
}

#[tokio::test]
async fn a_provider_that_names_another_issuer_is_refused() {
    let idp = FakeIdp::start().await;
    let p = HttpOidcProvider::new(OidcConfig::new(
        &format!("{}/", idp.issuer()),
        CLIENT_ID,
        CLIENT_SECRET,
    ))
    .unwrap();
    let r = p
        .authorization_url(&AuthorizationRequest {
            state: "s",
            nonce: "n",
            code_challenge: "c",
            redirect_uri: REDIRECT,
        })
        .await;
    assert!(matches!(r, Err(PynError::ExternalSignInFailed(_))), "{r:?}");
}

#[test]
fn configuration_must_be_complete_and_secure() {
    for (issuer, id, secret) in [
        ("http://idp.example", "id", "secret"),
        ("not a url", "id", "secret"),
        ("https://idp.example", "", "secret"),
        ("https://idp.example", "id", ""),
    ] {
        let r = HttpOidcProvider::new(OidcConfig::new(issuer, id, secret));
        assert!(matches!(r, Err(PynError::InvalidRequest(_))), "{issuer}");
    }
    HttpOidcProvider::new(OidcConfig::new("https://idp.example/realm", "id", "secret")).unwrap();
}

//! The host-level commands, run as the real binary: no database refuses, and (with `PYN_DATABASE_URL`) they work on a scratch schema.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

use chrono::Utc;
use pyn_core::{
    AccessService, AccessStore, AccountId, AccountKind, NewAccount, SecretKey, ServerSettings,
    SignupStage, SystemClock, UserId, WebhookStore, reveal_secret,
};
use pyn_postgres::PgMetadataStore;

fn pyn_server(args: &[&str], env: &[(&str, &str)], stdin: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pyn-server"));
    command
        .args(args)
        .env_remove("PYN_DATABASE_URL")
        .env_remove("PYN_SECRET_KEY")
        .env_remove("PYN_NEW_SECRET_KEY")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(stdin.unwrap_or("").as_bytes()).unwrap();
    drop(input);
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn recovery_refuses_the_in_memory_store_with_a_clear_message() {
    let out = pyn_server(&["recover-admin", "alice"], &[], None);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("PYN_DATABASE_URL is not set"));
    assert!(out.stdout.is_empty(), "no password is printed");
}

#[test]
fn rotation_refuses_the_in_memory_store_and_a_missing_new_key() {
    let out = pyn_server(&["rotate-secret-key"], &[], None);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("PYN_NEW_SECRET_KEY is not set"));
    let out = pyn_server(
        &["rotate-secret-key"],
        &[("PYN_NEW_SECRET_KEY", &"ab".repeat(32))],
        None,
    );
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("PYN_DATABASE_URL is not set"));
}

#[test]
fn a_malformed_command_line_is_refused_before_anything_starts() {
    let out = pyn_server(&["recover-admin"], &[], None);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("usage:"));
}

async fn scratch() -> (Arc<PgMetadataStore>, String) {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let store = Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    );
    let schema = store.scratch_schema().unwrap().to_string();
    let joiner = if url.contains('?') { '&' } else { '?' };
    let scoped = format!("{url}{joiner}options=-csearch_path%3D{schema}");
    (store, scoped)
}

fn service(store: &Arc<PgMetadataStore>) -> AccessService {
    AccessService::new(
        store.clone(),
        Arc::new(SystemClock),
        Arc::new(pyn_core::MemoryLimitEvents::new()),
    )
}

async fn set_up(store: &PgMetadataStore) {
    let admin = NewAccount {
        id: AccountId::generate(AccountKind::User).unwrap(),
        user: UserId::new("root"),
        email: None,
        password_hash: "unused".into(),
        signup: SignupStage::Complete,
        created_at: Utc::now(),
    };
    let settings = ServerSettings {
        initialised_at: Utc::now(),
        server_name: None,
        public_url: None,
        registration: None,
    };
    assert!(store.complete_setup(admin, settings).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn recover_admin_creates_resets_and_audits_on_postgres() {
    let (store, url) = scratch().await;
    let env = [("PYN_DATABASE_URL", url.as_str())];

    let out = pyn_server(&["recover-admin", "alice"], &env, None);
    assert!(
        !out.status.success(),
        "not set up yet: {}",
        text(&out.stderr)
    );
    assert!(text(&out.stderr).contains("not set up"));

    set_up(&store).await;
    let out = pyn_server(&["recover-admin", "alice"], &env, None);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let generated = text(&out.stdout).trim().to_string();
    assert!(generated.len() >= 40, "a strong password is printed once");
    let svc = service(&store);
    svc.login("alice", &generated, None).await.unwrap();
    assert!(
        store
            .account(&UserId::new("alice"))
            .await
            .unwrap()
            .unwrap()
            .is_admin
    );

    let out = pyn_server(
        &["recover-admin", "alice", "--password-stdin"],
        &env,
        Some("my own long password\n"),
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    assert!(svc.login("alice", &generated, None).await.is_err());
    svc.login("alice", "my own long password", None)
        .await
        .unwrap();

    let root = store.account(&UserId::new("root")).await.unwrap().unwrap();
    assert!(root.is_admin);
    assert_eq!(store.active_admin_count().await.unwrap(), 2);

    let log = pyn_core::AuditStore::list(
        store.as_ref(),
        &pyn_core::AuditScope::Server,
        &pyn_core::AuditQuery {
            limit: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(log.len(), 2);
    assert!(log.iter().all(|e| e.actor.as_str() == "@host-recovery"));
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn rotate_secret_key_reencrypts_every_secret_on_postgres() {
    let (store, url) = scratch().await;
    let (old, new) = ("01".repeat(32), "02".repeat(32));
    let (old_key, new_key) = (
        SecretKey::from_hex(&old).unwrap(),
        SecretKey::from_hex(&new).unwrap(),
    );
    let sub = store
        .create_subscription(
            pyn_core::NewSubscription {
                name: "hook".into(),
                url: "https://example.test/h".into(),
                event_types: Vec::new(),
                enabled: true,
                created_by: UserId::new("root"),
            },
            Utc::now(),
        )
        .await
        .unwrap();
    let sealed = old_key.seal("whsec_abc").unwrap();
    store.add_secret(sub.id, sealed, Utc::now()).await.unwrap();

    let out = pyn_server(
        &["rotate-secret-key"],
        &[
            ("PYN_DATABASE_URL", &url),
            ("PYN_SECRET_KEY", &"09".repeat(32)),
            ("PYN_NEW_SECRET_KEY", &new),
        ],
        None,
    );
    assert!(!out.status.success(), "the wrong current key is refused");
    let row = &store.list_active_secrets(sub.id).await.unwrap()[0];
    assert_eq!(reveal_secret(Some(&old_key), row).unwrap(), b"whsec_abc");

    let out = pyn_server(
        &["rotate-secret-key"],
        &[
            ("PYN_DATABASE_URL", &url),
            ("PYN_SECRET_KEY", &old),
            ("PYN_NEW_SECRET_KEY", &new),
        ],
        None,
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let row = &store.list_active_secrets(sub.id).await.unwrap()[0];
    assert_eq!(reveal_secret(Some(&new_key), row).unwrap(), b"whsec_abc");
}

//! Two real `pyn-server` processes on one PostgreSQL schema and one in-process S3 bucket (`PYN_INSTANCES=2`, shared
//! secret key and setup token), checked from the outside (docs/concepts/scale-out.md). Needs `PYN_DATABASE_URL`.

use std::fs::File;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pyn_postgres::PgMetadataStore;
use pyn_s3::testing::{ACCESS_KEY, BUCKET, SECRET_KEY, Server as Bucket};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const SETUP_TOKEN: &str = "two-copies-setup-token";
const PASSWORD: &str = "correct horse battery";
const ORIGIN: &str = "http://localhost:5173";
const POLICY: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Copy {
    child: Child,
    url: String,
    dir: tempfile::TempDir,
}

impl Copy {
    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("err.log")).unwrap_or_default()
    }

    fn sigterm(&self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn exit(&mut self, within: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "no exit: {}", self.log());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Two copies on one scratch schema and one bucket. Field order matters: the copies stop before the schema is dropped.
struct Copies {
    a: Copy,
    b: Copy,
    http: reqwest::Client,
    bucket: Option<Bucket>,
    _store: PgMetadataStore,
}

fn spawn(vars: &[(String, String)]) -> Copy {
    let dir = tempfile::tempdir().unwrap();
    let addr = format!("127.0.0.1:{}", free_port());
    let policy = dir.path().join("policy.toml");
    std::fs::write(&policy, POLICY).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .env("PYN_ADDR", &addr)
        .env("PYN_PUBLIC_URL", ORIGIN)
        .env("PYN_REGISTRATION", "open")
        .env("PYN_EMAIL_VERIFICATION", "false")
        .env("PYN_CONFIG", &policy)
        .env_remove("PYN_DATA_DIR")
        .env_remove("PYN_DEV_AUTH")
        .env_remove("PYN_MAILER")
        .env_remove("PYN_SHUTDOWN_GRACE_SECS")
        .env_remove("PYN_SHUTDOWN_DRAIN_DELAY_SECS")
        .envs(vars.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(File::create(dir.path().join("out.log")).unwrap())
        .stderr(File::create(dir.path().join("err.log")).unwrap())
        .spawn()
        .unwrap();
    Copy {
        child,
        url: format!("http://{addr}"),
        dir,
    }
}

async fn wait_ready(http: &reqwest::Client, c: &Copy) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(r) = http.get(format!("{}/readyz", c.url)).send().await
            && r.status() == StatusCode::OK
        {
            return;
        }
        assert!(Instant::now() < deadline, "not ready: {}", c.log());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

impl Copies {
    async fn start() -> Self {
        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        let schema = store.scratch_schema().unwrap().to_string();
        let joiner = if url.contains('?') { '&' } else { '?' };
        let scoped = format!("{url}{joiner}options=-csearch_path%3D{schema}");
        let bucket = Bucket::start();
        let vars: Vec<(String, String)> = [
            ("PYN_DATABASE_URL", scoped),
            ("PYN_INSTANCES", "2".into()),
            ("PYN_SECRET_KEY", "ab".repeat(32)),
            ("PYN_SETUP_TOKEN", SETUP_TOKEN.into()),
            ("PYN_S3_ENDPOINT", format!("http://{}", bucket.addr)),
            ("PYN_S3_BUCKET", BUCKET.into()),
            ("PYN_S3_PATH_STYLE", "true".into()),
            ("PYN_S3_ACCESS_KEY_ID", ACCESS_KEY.into()),
            ("PYN_S3_SECRET_ACCESS_KEY", SECRET_KEY.into()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        let (a, b) = (spawn(&vars), spawn(&vars));
        let http = reqwest::Client::new();
        wait_ready(&http, &a).await;
        wait_ready(&http, &b).await;
        Copies {
            a,
            b,
            http,
            bucket: Some(bucket),
            _store: store,
        }
    }

    /// Starts both copies, sets the server up through A, and creates `alice/game`; returns alice's token from A.
    async fn with_game() -> (Self, String) {
        let c = Self::start().await;
        let setup = json!({
            "token": SETUP_TOKEN, "username": "root", "password": PASSWORD, "registration": "open"
        });
        let (status, body) = c
            .call(&c.a, Method::POST, "/v1/setup", None, Some(setup))
            .await;
        assert!(status.is_success(), "{body}");
        let alice = c.sign_up(&c.a, "alice").await;
        let (status, body) = c
            .call(
                &c.a,
                Method::POST,
                "/v1/repos",
                Some(&alice),
                Some(json!({"starter_policy": false, "name": "game"})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        (c, alice)
    }

    async fn call(
        &self,
        copy: &Copy,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self.http.request(method, format!("{}{path}", copy.url));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Registers `name` and signs in on `copy`; returns a token.
    async fn sign_up(&self, copy: &Copy, name: &str) -> String {
        let body = json!({"username": name, "password": PASSWORD});
        let (status, reply) = self
            .call(copy, Method::POST, "/v1/register", None, Some(body.clone()))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{reply}");
        self.login(copy, name).await
    }

    async fn login(&self, copy: &Copy, name: &str) -> String {
        let body = json!({"username": name, "password": PASSWORD});
        let (status, reply) = self
            .call(copy, Method::POST, "/v1/login", None, Some(body))
            .await;
        assert!(status.is_success(), "{reply}");
        reply["token"].as_str().unwrap().to_string()
    }

    async fn add_writer(&self, owner_token: &str, user: &str) {
        let (status, body) = self
            .call(
                &self.a,
                Method::PUT,
                &format!("/v1/repos/alice/game/members/{user}"),
                Some(owner_token),
                Some(json!({"role": "writer"})),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn setup_and_accounts_on_one_copy_are_visible_on_the_other() {
    let c = Copies::start().await;
    let (_, before) = c.call(&c.b, Method::GET, "/v1/setup", None, None).await;
    assert_eq!(before["initialised"], false, "{before}");

    let setup = json!({
        "token": SETUP_TOKEN, "username": "root", "password": PASSWORD, "registration": "open"
    });
    let (status, body) = c
        .call(&c.a, Method::POST, "/v1/setup", None, Some(setup))
        .await;
    assert!(status.is_success(), "{body}");

    let (_, after) = c.call(&c.b, Method::GET, "/v1/setup", None, None).await;
    assert_eq!(
        after["initialised"], true,
        "B still sees an open setup: {after}"
    );
    let root = c.login(&c.b, "root").await;
    let (status, _) = c
        .call(&c.b, Method::GET, "/v1/admin/settings", Some(&root), None)
        .await;
    assert_eq!(status, StatusCode::OK);

    c.sign_up(&c.a, "alice").await;
    c.login(&c.b, "alice").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_token_and_a_cookie_session_from_one_copy_are_accepted_by_the_other() {
    let (c, alice) = Copies::with_game().await;
    let (status, body) = c
        .call(
            &c.b,
            Method::GET,
            "/v1/repos/alice/game/locks",
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let r = c
        .http
        .post(format!("{}/v1/session", c.a.url))
        .header("origin", ORIGIN)
        .json(&json!({"username": "alice", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let set = r.headers().get("set-cookie").unwrap().to_str().unwrap();
    let cookie = set.split(';').next().unwrap().to_string();
    let r = c
        .http
        .get(format!("{}/v1/session", c.b.url))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let info: Value = r.json().await.unwrap();
    assert_eq!(info["user"], "alice", "{info}");

    // A token made on B works on A too.
    let bob = c.sign_up(&c.b, "bob").await;
    let (status, _) = c
        .call(&c.a, Method::GET, "/v1/repos", Some(&bob), None)
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_lock_taken_on_one_copy_is_refused_and_listed_on_the_other() {
    let (c, alice) = Copies::with_game().await;
    let bob = c.sign_up(&c.b, "bob").await;
    c.add_writer(&alice, "bob").await;
    let take = |path: &str| Some(json!({"path": path}));

    let (status, body) = c
        .call(
            &c.a,
            Method::POST,
            "/v1/repos/alice/game/checkout",
            Some(&alice),
            take("Content/x.umap"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) = c
        .call(
            &c.b,
            Method::POST,
            "/v1/repos/alice/game/checkout",
            Some(&bob),
            take("Content/x.umap"),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"].as_str().or(body["code"].as_str()),
        Some("lock_held"),
        "{body}"
    );

    let (_, locks) = c
        .call(
            &c.b,
            Method::GET,
            "/v1/repos/alice/game/locks",
            Some(&bob),
            None,
        )
        .await;
    let held: Vec<_> = locks
        .as_array()
        .unwrap()
        .iter()
        .map(|l| (l["path"].as_str().unwrap(), l["owner"].as_str().unwrap()))
        .collect();
    assert_eq!(held, [("Content/x.umap", "alice")]);

    let (status, body) = c
        .call(
            &c.b,
            Method::POST,
            "/v1/repos/alice/game/release",
            Some(&alice),
            take("Content/x.umap"),
        )
        .await;
    assert!(status.is_success(), "{body}");
    let (status, body) = c
        .call(
            &c.a,
            Method::POST,
            "/v1/repos/alice/game/checkout",
            Some(&bob),
            take("Content/x.umap"),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "released on B, free on A: {body}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn the_lock_stream_on_one_copy_receives_a_lock_taken_on_the_other() {
    let (c, alice) = Copies::with_game().await;
    let base = "/v1/repos/alice/game/locks/events";
    let (_, head) = c.call(&c.b, Method::GET, base, Some(&alice), None).await;
    let cursor = head["cursor"].as_str().unwrap().to_string();

    let waiting = {
        let (http, url, token) = (c.http.clone(), c.b.url.clone(), alice.clone());
        tokio::spawn(async move {
            let r = http
                .get(format!("{url}{base}?after={cursor}&wait=20"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap();
            r.json::<Value>().await.unwrap()
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    let (status, body) = c
        .call(
            &c.a,
            Method::POST,
            "/v1/repos/alice/game/checkout",
            Some(&alice),
            Some(json!({"path": "Content/y.umap"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let page = tokio::time::timeout(Duration::from_secs(15), waiting)
        .await
        .expect("the stream on B never woke")
        .unwrap();
    let events = page["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{page}");
    assert_eq!(events[0]["event"]["type"], "lock.acquired", "{page}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

fn oid(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_resumable_upload_started_on_one_copy_finishes_on_the_other() {
    let (c, alice) = Copies::with_game().await;
    let bytes: Vec<u8> = (0..10_000).map(|i| (i * 7 % 251) as u8).collect();
    let path = format!("/v1/repos/alice/game/info/lfs/objects/{}/tus", oid(&bytes));
    let tus = |copy: &Copy, method: Method| {
        c.http
            .request(method, format!("{}{path}", copy.url))
            .bearer_auth(&alice)
            .header("tus-resumable", "1.0.0")
    };

    let r = tus(&c.a, Method::POST)
        .header("upload-length", bytes.len())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let patch = |copy: &Copy, at: usize, part: &[u8]| {
        tus(copy, Method::PATCH)
            .header("content-type", "application/offset+octet-stream")
            .header("upload-offset", at)
            .body(part.to_vec())
            .send()
    };
    let r = patch(&c.a, 0, &bytes[..4_000]).await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);

    let head = tus(&c.b, Method::HEAD).send().await.unwrap();
    assert_eq!(head.status(), StatusCode::OK, "B does not know the upload");
    assert_eq!(head.headers()["upload-offset"], "4000");
    let r = patch(&c.b, 4_000, &bytes[4_000..]).await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT, "{:?}", r.text().await);

    let object = format!("/v1/repos/alice/game/info/lfs/objects/{}", oid(&bytes));
    for copy in [&c.a, &c.b] {
        let r = c
            .http
            .get(format!("{}{object}", copy.url))
            .bearer_auth(&alice)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.bytes().await.unwrap(), bytes);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_stopped_copy_exits_cleanly_while_the_other_keeps_serving() {
    let (mut c, alice) = Copies::with_game().await;
    c.a.sigterm();
    let status = c.a.exit(Duration::from_secs(30));
    assert!(status.success(), "{status}: {}", c.a.log());

    let r = c
        .http
        .get(format!("{}/readyz", c.b.url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let (status, body) = c
        .call(
            &c.b,
            Method::GET,
            "/v1/repos/alice/game/locks",
            Some(&alice),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = c
        .call(
            &c.b,
            Method::POST,
            "/v1/repos/alice/game/checkout",
            Some(&alice),
            Some(json!({"path": "Content/z.umap"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_copy_turns_unready_when_the_bucket_stops_answering() {
    let (mut c, _) = Copies::with_game().await;
    drop(c.bucket.take());
    // The bucket probe runs every 30 seconds.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let r = c
            .http
            .get(format!("{}/readyz", c.a.url))
            .send()
            .await
            .unwrap();
        if r.status() == StatusCode::SERVICE_UNAVAILABLE {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "A stayed ready without its bucket"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let r = c
        .http
        .get(format!("{}/healthz", c.a.url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK, "liveness is not readiness");
}

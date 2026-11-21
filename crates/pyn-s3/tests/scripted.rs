//! Wire behaviour against a scripted HTTP server: retries, error text, the headers sent, multipart sequencing.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pyn_core::{Keying, ObjectStore, PynError};
use pyn_s3::{S3ObjectStore, S3Settings};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Debug)]
struct Seen {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

type Reply = (u16, Vec<(&'static str, String)>, Vec<u8>);
type Script = dyn Fn(&Seen) -> Reply + Send + Sync;

struct Fake {
    port: u16,
    log: Arc<Mutex<Vec<Seen>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn reply(status: u16) -> Reply {
    (status, Vec::new(), Vec::new())
}

async fn serve(script: Arc<Script>) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let log = log.clone();
        async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    continue;
                };
                let (script, log) = (script.clone(), log.clone());
                tokio::spawn(async move {
                    loop {
                        let Some(seen) = read_request(&mut sock).await else {
                            return;
                        };
                        let (status, headers, body) = script(&seen);
                        let head_only = seen.method == "HEAD";
                        log.lock().unwrap().push(seen);
                        let mut out = format!("HTTP/1.1 {status} X\r\n");
                        if !headers.iter().any(|(k, _)| *k == "content-length") {
                            out.push_str(&format!("content-length: {}\r\n", body.len()));
                        }
                        for (k, v) in headers {
                            out.push_str(&format!("{k}: {v}\r\n"));
                        }
                        out.push_str("\r\n");
                        let mut bytes = out.into_bytes();
                        if !head_only {
                            bytes.extend_from_slice(&body);
                        }
                        if sock.write_all(&bytes).await.is_err() {
                            return;
                        }
                    }
                });
            }
        }
    });
    Fake { port, log, task }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<Seen> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if sock.read(&mut byte).await.ok()? == 0 {
            return None;
        }
        buf.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0usize);
    let mut body = vec![0u8; len];
    sock.read_exact(&mut body).await.ok()?;
    Some(Seen {
        method,
        target,
        headers,
        body,
    })
}

fn settings(port: u16, dir: &std::path::Path) -> S3Settings {
    let mut s = S3Settings::new(
        format!("http://127.0.0.1:{port}"),
        "bkt",
        "AKID",
        "scripted-secret",
    );
    s.path_style = true;
    s.spool_dir = dir.to_owned();
    s.retry_base = Duration::from_millis(2);
    s.retry_max = Duration::from_millis(10);
    s
}

fn slow_down() -> Reply {
    (
        503,
        vec![("content-type", "application/xml".into())],
        b"<Error><Code>SlowDown</Code><Message>Reduce your request rate.</Message></Error>"
            .to_vec(),
    )
}

#[tokio::test]
async fn a_new_object_is_one_head_then_one_put_with_only_the_headers_we_choose() {
    let dir = tempfile::tempdir().unwrap();
    let fake = serve(Arc::new(|r| {
        if r.method == "HEAD" {
            reply(404)
        } else {
            reply(200)
        }
    }))
    .await;
    let store = S3ObjectStore::open(settings(fake.port, dir.path()))
        .await
        .unwrap();
    let id = store.put(b"hello".to_vec()).await.unwrap();
    let log = fake.log.lock().unwrap().clone();
    assert_eq!(log.len(), 2, "{log:?}");
    let target = format!("/bkt/{}/{}", &id.to_hex()[..2], &id.to_hex()[2..]);
    assert_eq!(
        (log[0].method.as_str(), log[0].target.as_str()),
        ("HEAD", target.as_str())
    );
    assert_eq!(
        (log[1].method.as_str(), log[1].target.as_str()),
        ("PUT", target.as_str())
    );
    let put = &log[1];
    assert_eq!(put.body, b"hello");
    assert_eq!(
        put.header("x-amz-content-sha256").unwrap(),
        hex::encode(Sha256::digest(b"hello"))
    );
    assert!(
        put.header("authorization")
            .unwrap()
            .starts_with("AWS4-HMAC-SHA256 Credential=AKID/")
    );
    assert!(
        !put.header("authorization")
            .unwrap()
            .contains("scripted-secret")
    );
    for (name, _) in &put.headers {
        assert!(!name.starts_with("x-amz-checksum"), "{name}");
        assert!(!name.starts_with("x-amz-sdk"), "{name}");
        assert_ne!(name, "expect");
        assert_ne!(name, "content-encoding");
        assert_ne!(name, "x-amz-decoded-content-length");
    }
}

#[tokio::test]
async fn an_existing_object_is_not_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let fake = serve(Arc::new(|_| (200, vec![], Vec::new()))).await;
    let store = S3ObjectStore::open(settings(fake.port, dir.path()))
        .await
        .unwrap();
    let stored = store
        .put_stream_tracked(
            Box::pin(std::io::Cursor::new(b"hello".to_vec())),
            Keying::Blob { len: 5 },
            None,
        )
        .await
        .unwrap();
    assert!(stored.created.is_empty());
    let log = fake.log.lock().unwrap().clone();
    assert_eq!(
        log.iter().map(|s| s.method.as_str()).collect::<Vec<_>>(),
        ["HEAD"]
    );
}

#[tokio::test]
async fn throttling_and_server_errors_are_retried_with_backoff() {
    let dir = tempfile::tempdir().unwrap();
    let heads = Arc::new(Mutex::new(0));
    let fake = serve(Arc::new({
        let heads = heads.clone();
        move |r| {
            if r.method != "HEAD" {
                return reply(200);
            }
            let mut n = heads.lock().unwrap();
            *n += 1;
            match *n {
                1 | 2 => slow_down(),
                3 => reply(500),
                _ => reply(404),
            }
        }
    }))
    .await;
    let store = S3ObjectStore::open(settings(fake.port, dir.path()))
        .await
        .unwrap();
    store.put(b"retry me".to_vec()).await.unwrap();
    assert_eq!(*heads.lock().unwrap(), 4);
}

#[tokio::test]
async fn retries_stop_at_the_attempt_limit_and_name_the_operation_status_and_code() {
    let dir = tempfile::tempdir().unwrap();
    let fake = serve(Arc::new(|_| slow_down())).await;
    let mut s = settings(fake.port, dir.path());
    s.max_attempts = 3;
    let store = S3ObjectStore::open(s).await.unwrap();
    let err = store.size(&unknown()).await.unwrap_err();
    assert_eq!(fake.log.lock().unwrap().len(), 3);
    let PynError::Storage(text) = err else {
        panic!("{err}")
    };
    // A HEAD answer carries no body, so only the status is known there.
    assert!(
        text.contains("HeadObject") && text.contains("503"),
        "{text}"
    );

    let err = store.get_manifest(&unknown()).await.unwrap_err();
    let PynError::Storage(text) = err else {
        panic!("{err}")
    };
    assert!(
        text.contains("GetObject") && text.contains("503") && text.contains("SlowDown"),
        "{text}"
    );
}

#[tokio::test]
async fn a_refusal_is_not_retried() {
    let dir = tempfile::tempdir().unwrap();
    let fake = serve(Arc::new(|r| {
        if r.method == "HEAD" {
            reply(404)
        } else {
            (
                403,
                vec![],
                b"<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>"
                    .to_vec(),
            )
        }
    }))
    .await;
    let store = S3ObjectStore::open(settings(fake.port, dir.path()))
        .await
        .unwrap();
    let err = store.put(b"x".to_vec()).await.unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("PutObject") && text.contains("403") && text.contains("AccessDenied"),
        "{text}"
    );
    assert_eq!(fake.log.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn an_unreachable_store_is_a_clear_storage_error_after_the_retries() {
    let dir = tempfile::tempdir().unwrap();
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut s = settings(port, dir.path());
    s.max_attempts = 2;
    let store = S3ObjectStore::open(s).await.unwrap();
    let err = store.exists(&unknown()).await.unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, PynError::Storage(_)));
    assert!(
        text.contains("HeadObject") && text.contains("cannot reach 127.0.0.1"),
        "{text}"
    );
    assert!(!text.contains("scripted-secret"));
}

#[tokio::test]
async fn a_service_that_ignores_range_still_yields_the_range() {
    let dir = tempfile::tempdir().unwrap();
    let body: Vec<u8> = (0..100u8).collect();
    let fake = serve(Arc::new(move |r| {
        if r.method == "HEAD" {
            (200, vec![], Vec::new())
        } else {
            (200, vec![], body.clone())
        }
    }))
    .await;
    let store = S3ObjectStore::open(settings(fake.port, dir.path()))
        .await
        .unwrap();
    let found = store.read(&unknown(), Some(10..20)).await.unwrap().unwrap();
    assert_eq!((found.total, found.range.clone()), (100, 10..20));
    let mut got = Vec::new();
    let mut reader = found.reader;
    reader.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, (10..20u8).collect::<Vec<_>>());
    let log = fake.log.lock().unwrap().clone();
    assert_eq!(log[0].header("range"), Some("bytes=10-19"));
}

#[tokio::test]
async fn an_unsatisfiable_range_asks_for_the_length_and_clamps() {
    let dir = tempfile::tempdir().unwrap();
    let fake = serve(Arc::new(|r| {
        if r.method == "HEAD" {
            (200, vec![("content-length", "80".into())], Vec::new())
        } else {
            (
                416,
                vec![],
                b"<Error><Code>InvalidRange</Code></Error>".to_vec(),
            )
        }
    }))
    .await;
    let store = S3ObjectStore::open(settings(fake.port, dir.path()))
        .await
        .unwrap();
    let found = store
        .read(&unknown(), Some(90..100))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((found.total, found.range), (80, 80..80));
}

#[tokio::test]
async fn multipart_is_create_parts_complete_and_failure_aborts() {
    let dir = tempfile::tempdir().unwrap();
    let fail_part = Arc::new(Mutex::new(false));
    let fake = serve(Arc::new({
        let fail_part = fail_part.clone();
        move |r| {
            let q = r.target.split_once('?').map(|(_, q)| q).unwrap_or("");
            match (r.method.as_str(), q) {
                ("HEAD", _) => reply(404),
                ("POST", "uploads=") => (200, vec![], b"<InitiateMultipartUploadResult><UploadId>up-1</UploadId></InitiateMultipartUploadResult>".to_vec()),
                ("PUT", q) if q.contains("partNumber=2") && *fail_part.lock().unwrap() => (
                    403,
                    vec![],
                    b"<Error><Code>AccessDenied</Code></Error>".to_vec(),
                ),
                ("PUT", q) if q.starts_with("partNumber=") => {
                    let n = q.split('&').next().unwrap().trim_start_matches("partNumber=");
                    (200, vec![("etag", format!("\"etag-{n}\""))], Vec::new())
                }
                ("POST", "uploadId=up-1") => (200, vec![], b"<CompleteMultipartUploadResult/>".to_vec()),
                ("DELETE", "uploadId=up-1") => reply(204),
                _ => reply(500),
            }
        }
    }))
    .await;
    let mut s = settings(fake.port, dir.path());
    s.spool_threshold = 10;
    s.multipart_threshold = 100;
    s.part_size = 60;
    let store = S3ObjectStore::open(s).await.unwrap();
    let data = vec![9u8; 150];

    let stored = store
        .put_stream_tracked(
            Box::pin(std::io::Cursor::new(data.clone())),
            Keying::Raw,
            None,
        )
        .await
        .unwrap();
    let log = fake.log.lock().unwrap().clone();
    let steps: Vec<_> = log.iter().map(|s| s.method.clone()).collect();
    assert_eq!(steps, ["HEAD", "POST", "PUT", "PUT", "PUT", "POST"]);
    let final_key = format!(
        "/bkt/{}/{}",
        &stored.id.to_hex()[..2],
        &stored.id.to_hex()[2..]
    );
    assert!(
        log[1].target.starts_with(&format!("{final_key}?")),
        "the final key, not a temporary one"
    );
    let parts: Vec<usize> = log[2..5].iter().map(|s| s.body.len()).collect();
    assert_eq!(parts, [60, 60, 30]);
    assert_eq!(
        log[2].header("x-amz-content-sha256").unwrap(),
        hex::encode(Sha256::digest(&data[..60]))
    );
    let complete = String::from_utf8(log[5].body.clone()).unwrap();
    assert!(
        complete.contains("<PartNumber>3</PartNumber><ETag>\"etag-3\"</ETag>"),
        "{complete}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    *fail_part.lock().unwrap() = true;
    fake.log.lock().unwrap().clear();
    let other = vec![8u8; 150];
    let err = store
        .put_stream(Box::pin(std::io::Cursor::new(other)), Keying::Raw, None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("UploadPart"), "{err}");
    let log = fake.log.lock().unwrap().clone();
    assert_eq!(
        log.last().unwrap().method,
        "DELETE",
        "the upload is aborted"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

fn unknown() -> pyn_core::ObjectId {
    pyn_core::ObjectId::from_hex(pyn_core::REPO_ALGORITHM, &"ab".repeat(32)).unwrap()
}

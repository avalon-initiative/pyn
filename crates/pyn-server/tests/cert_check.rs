//! The certificate watch: real certificates made with the `openssl` command, probed on a local TLS listener.
//! The loopback skip is a rule of `Target::from_public_url`; `probe` and `check` take a target directly.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{AccessConfig, AccessService, AuthProvider, ManualClock, Repositories, Rules};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::certificate::{
    CertificateReport, ProbeOptions, Target, check, parse_certificate, probe, spawn_monitor,
};
use pyn_server::shutdown::Drain;
use pyn_server::{AppState, router};
use rustls::ServerConfig;
use rustls::crypto::aws_lc_rs;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

fn openssl(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("openssl")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the openssl command is needed for these tests");
    assert!(
        out.status.success(),
        "openssl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[derive(Clone, Copy)]
enum Key {
    Ecdsa,
    Rsa,
}

struct Pki {
    dir: tempfile::TempDir,
    ca: Vec<u8>,
    issued: std::cell::Cell<u32>,
}

struct Leaf {
    cert: Vec<u8>,
    key: Vec<u8>,
}

fn stamp(t: DateTime<Utc>) -> String {
    t.format("%Y%m%d%H%M%SZ").to_string()
}

impl Pki {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::write(
            d.join("ca.cnf"),
            "[ca]\ndefault_ca=c\n[c]\ndatabase=index.txt\nnew_certs_dir=.\nserial=serial\ndefault_md=sha256\npolicy=p\nunique_subject=no\n[p]\ncommonName=supplied\n",
        )
        .unwrap();
        std::fs::write(d.join("index.txt"), "").unwrap();
        std::fs::write(d.join("serial"), "0A\n").unwrap();
        openssl(
            d,
            &[
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-keyout",
                "ca.key",
                "-out",
                "ca.pem",
                "-subj",
                "/CN=Pyn Test CA",
                "-days",
                "3650",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
                "-addext",
                "keyUsage=critical,keyCertSign",
            ],
        );
        let ca = openssl(d, &["x509", "-in", "ca.pem", "-outform", "DER"]);
        Self {
            dir,
            ca,
            issued: Default::default(),
        }
    }

    fn issue(&self, key: Key, san: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Leaf {
        let d = self.dir.path();
        let n = self.issued.get() + 1;
        self.issued.set(n);
        let (key_file, csr, pem) = (
            format!("k{n}.key"),
            format!("k{n}.csr"),
            format!("c{n}.pem"),
        );
        let newkey: &[&str] = match key {
            Key::Ecdsa => &["-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1"],
            Key::Rsa => &["-newkey", "rsa:2048"],
        };
        let mut req = vec!["req", "-new", "-nodes", "-keyout", &key_file, "-out", &csr];
        req.extend_from_slice(newkey);
        req.extend_from_slice(&["-subj", "/CN=leaf.test"]);
        openssl(d, &req);
        std::fs::write(
            d.join("ext.cnf"),
            format!("[x]\nsubjectAltName={san}\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n"),
        )
        .unwrap();
        let (start, end) = (stamp(from), stamp(to));
        openssl(
            d,
            &[
                "ca",
                "-batch",
                "-config",
                "ca.cnf",
                "-cert",
                "ca.pem",
                "-keyfile",
                "ca.key",
                "-in",
                &csr,
                "-out",
                &pem,
                "-notext",
                "-startdate",
                &start,
                "-enddate",
                &end,
                "-extfile",
                "ext.cnf",
                "-extensions",
                "x",
            ],
        );
        Leaf {
            cert: openssl(d, &["x509", "-in", &pem, "-outform", "DER"]),
            key: openssl(
                d,
                &[
                    "pkcs8", "-topk8", "-nocrypt", "-in", &key_file, "-outform", "DER",
                ],
            ),
        }
    }

    fn options(&self) -> ProbeOptions {
        ProbeOptions {
            connect_timeout: Duration::from_secs(2),
            handshake_timeout: Duration::from_secs(2),
            extra_roots: vec![CertificateDer::from(self.ca.clone())],
        }
    }
}

async fn tls_server(leaf: &Leaf) -> Target {
    let key = PrivateKeyDer::try_from(leaf.key.clone()).unwrap();
    let config = ServerConfig::builder_with_provider(Arc::new(aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![CertificateDer::from(leaf.cert.clone())], key)
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let _ = acceptor.accept(tcp).await;
            });
        }
    });
    Target {
        host: "127.0.0.1".into(),
        port,
    }
}

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(Utc::now().timestamp(), 0).unwrap()
}

const SAN: &str = "DNS:pyn.test,DNS:www.pyn.test,IP:127.0.0.1,IP:2001:db8::1";

fn days(n: i64) -> chrono::Duration {
    chrono::Duration::days(n)
}

#[tokio::test]
async fn a_valid_certificate_reports_its_days_and_that_it_is_trusted() {
    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(
        Key::Ecdsa,
        SAN,
        n - days(1),
        n + days(30) + chrono::Duration::hours(1),
    );
    let target = tls_server(&leaf).await;
    let (report, critical) = check(
        &target,
        &pki.options(),
        &CertificateReport::default().settings(),
    )
    .await;
    assert_eq!(report.state, api::CertificateState::Ok);
    assert_eq!(
        (report.days_left, report.trusted, critical),
        (Some(30), Some(true), false)
    );
    assert_eq!(
        report.names,
        ["pyn.test", "www.pyn.test", "127.0.0.1", "2001:db8::1"]
    );
    assert_eq!(report.issuer.as_deref(), Some("Pyn Test CA"));
    assert_eq!(
        report.not_after,
        Some(n + days(30) + chrono::Duration::hours(1))
    );
    assert!(report.last_error.is_none() && report.last_checked.is_some());
}

#[tokio::test]
async fn an_expired_certificate_is_read_and_reported_expired() {
    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(
        Key::Ecdsa,
        SAN,
        n - days(10),
        n - chrono::Duration::hours(18),
    );
    let target = tls_server(&leaf).await;
    let (report, critical) = check(
        &target,
        &pki.options(),
        &CertificateReport::default().settings(),
    )
    .await;
    assert_eq!(report.state, api::CertificateState::Expired);
    assert_eq!(report.days_left, Some(-1));
    assert!(critical);
    assert_eq!(
        report.trusted,
        Some(false),
        "an expired chain does not verify"
    );
}

#[tokio::test]
async fn a_certificate_two_days_from_expiry_is_expiring_and_critical() {
    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(
        Key::Rsa,
        SAN,
        n - days(88),
        n + days(2) + chrono::Duration::hours(1),
    );
    let target = tls_server(&leaf).await;
    let (report, critical) = check(
        &target,
        &pki.options(),
        &CertificateReport::default().settings(),
    )
    .await;
    assert_eq!(report.state, api::CertificateState::Expiring);
    assert_eq!(report.days_left, Some(2));
    assert!(critical);
}

#[tokio::test]
async fn an_untrusted_certificate_still_has_its_expiry_read() {
    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(Key::Ecdsa, SAN, n - days(1), n + days(60));
    let target = tls_server(&leaf).await;
    let opts = ProbeOptions {
        extra_roots: Vec::new(),
        ..pki.options()
    };
    let seen = probe(&target, &opts).await.unwrap();
    assert!(!seen.trusted);
    assert_eq!(seen.facts.not_after, n + days(60));
}

#[tokio::test]
async fn a_certificate_for_another_name_is_untrusted() {
    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(Key::Ecdsa, "DNS:other.test", n - days(1), n + days(60));
    let target = tls_server(&leaf).await;
    assert!(!probe(&target, &pki.options()).await.unwrap().trusted);
}

#[tokio::test]
async fn a_twelve_hour_certificate_is_ok_and_is_read_exactly() {
    let pki = Pki::new();
    let n = now();
    for key in [Key::Ecdsa, Key::Rsa] {
        let leaf = pki.issue(
            key,
            SAN,
            n - chrono::Duration::hours(1),
            n + chrono::Duration::hours(11),
        );
        let facts = parse_certificate(&leaf.cert).unwrap();
        assert_eq!(facts.not_before, n - chrono::Duration::hours(1));
        assert_eq!(facts.not_after, n + chrono::Duration::hours(11));
        let target = tls_server(&leaf).await;
        let (report, critical) = check(
            &target,
            &pki.options(),
            &CertificateReport::default().settings(),
        )
        .await;
        assert_eq!(report.state, api::CertificateState::Ok);
        assert_eq!((report.days_left, critical), (Some(0), false));
    }
}

#[test]
fn names_come_from_the_subject_when_there_is_no_alt_name_and_ip_names_are_read() {
    let pki = Pki::new();
    let n = now();
    let ip = pki.issue(Key::Ecdsa, "IP:192.0.2.7,IP:::1", n, n + days(5));
    let f = parse_certificate(&ip.cert).unwrap();
    assert_eq!(f.names, ["192.0.2.7", "::1"]);
    let d = pki.dir.path();
    openssl(
        d,
        &[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:prime256v1",
            "-nodes",
            "-keyout",
            "s.key",
            "-out",
            "s.pem",
            "-subj",
            "/CN=only-subject.test",
            "-days",
            "1",
        ],
    );
    let der = openssl(d, &["x509", "-in", "s.pem", "-outform", "DER"]);
    let f = parse_certificate(&der).unwrap();
    assert_eq!(f.names, ["only-subject.test"]);
    assert_eq!(f.issuer.as_deref(), Some("only-subject.test"));
}

#[tokio::test]
async fn a_refused_connection_is_unreachable_and_keeps_the_last_good_read() {
    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(Key::Ecdsa, SAN, n - days(1), n + days(60));
    let good = tls_server(&leaf).await;
    let (first, _) = check(
        &good,
        &pki.options(),
        &CertificateReport::default().settings(),
    )
    .await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gone = Target {
        host: "127.0.0.1".into(),
        port: closed.local_addr().unwrap().port(),
    };
    drop(closed);
    let (second, critical) = check(&gone, &pki.options(), &first).await;
    assert_eq!(second.state, api::CertificateState::Unreachable);
    assert!(
        second
            .last_error
            .as_deref()
            .unwrap()
            .starts_with("connect failed")
    );
    assert_eq!(second.not_after, first.not_after);
    assert!(!critical);
}

#[tokio::test]
async fn a_silent_server_times_out_and_a_non_tls_server_fails_the_handshake() {
    let quiet = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent = Target {
        host: "127.0.0.1".into(),
        port: quiet.local_addr().unwrap().port(),
    };
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((tcp, _)) = quiet.accept().await {
            held.push(tcp);
        }
    });
    let quick = ProbeOptions {
        handshake_timeout: Duration::from_millis(300),
        ..ProbeOptions::default()
    };
    let started = std::time::Instant::now();
    let err = probe(&silent, &quick).await.unwrap_err();
    assert!(err.contains("timed out"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(3));
    held.abort();

    let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plain = Target {
        host: "127.0.0.1".into(),
        port: http.local_addr().unwrap().port(),
    };
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = http.accept().await {
            let _ = tcp.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            let _ = tcp.shutdown().await;
        }
    });
    let err = probe(&plain, &ProbeOptions::default()).await.unwrap_err();
    assert!(err.starts_with("TLS handshake failed"), "{err}");
}

#[tokio::test]
async fn the_monitor_records_a_check_and_ends_when_draining_starts() {
    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(Key::Ecdsa, SAN, n - days(1), n + days(60));
    let target = tls_server(&leaf).await;
    let report = Arc::new(CertificateReport::default());
    assert_eq!(report.settings().state, api::CertificateState::Unchecked);
    let drain = Drain::default();
    let handle = spawn_monitor(
        target,
        report.clone(),
        Duration::from_secs(3600),
        Duration::ZERO,
        drain.clone(),
    );
    for _ in 0..100 {
        if report.settings().state != api::CertificateState::Unchecked {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let seen = report.settings();
    assert_eq!(seen.state, api::CertificateState::Ok);
    assert_eq!(
        seen.trusted,
        Some(false),
        "the test authority is not a system root"
    );
    drain.start();
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .unwrap()
        .unwrap();

    let late = spawn_monitor(
        Target {
            host: "127.0.0.1".into(),
            port: 9,
        },
        report,
        Duration::from_secs(3600),
        Duration::from_secs(3600),
        drain,
    );
    tokio::time::timeout(Duration::from_secs(2), late)
        .await
        .unwrap()
        .unwrap();
}

fn app(certificate: Arc<CertificateReport>) -> Router {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig::default())
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
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
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        certificate,
        ready: Default::default(),
    })
}

async fn get(app: &Router, uri: &str, admin: bool) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().method("GET").uri(uri);
    if admin {
        req = req.cred(&Cred::dev("root"));
    }
    let r = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    (
        r.status(),
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

#[tokio::test]
async fn the_admin_settings_report_the_certificate_and_readiness_ignores_it() {
    let unchecked = app(Arc::default());
    let (status, body) = get(&unchecked, "/v1/admin/settings", true).await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["certificate"]["state"], "unchecked");
    assert!(json["certificate"]["not_after"].is_null());
    assert_eq!(json["certificate"]["names"], serde_json::json!([]));

    let pki = Pki::new();
    let n = now();
    let leaf = pki.issue(
        Key::Ecdsa,
        SAN,
        n - days(10),
        n - chrono::Duration::hours(18),
    );
    let target = tls_server(&leaf).await;
    let report = Arc::new(CertificateReport::default());
    let (read, _) = check(&target, &pki.options(), &report.settings()).await;
    report.set(read.clone());
    let watched = app(report);
    let (_, body) = get(&watched, "/v1/admin/settings", true).await;
    let settings: api::AdminSettings = serde_json::from_slice(&body).unwrap();
    assert_eq!(settings.certificate, read);
    assert_eq!(settings.certificate.state, api::CertificateState::Expired);
    let (status, _) = get(&watched, "/v1/admin/settings", false).await;
    assert_ne!(status, StatusCode::OK, "administrators only");
    let (status, _) = get(&watched, "/readyz", false).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "an expired certificate is not a readiness problem"
    );
}

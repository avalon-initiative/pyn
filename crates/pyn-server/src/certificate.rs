//! Watches the TLS certificate served at `PYN_PUBLIC_URL`: the certificate lives in the web container or a balancer, so
//! the server reads its expiry from outside with a periodic handshake and reports it in the admin settings.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use pyn_proto as api;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, aws_lc_rs};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use rustls_platform_verifier::Verifier;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::shutdown::Drain;

/// The default and the accepted range of `PYN_CERT_CHECK_INTERVAL_SECS` (0 disables).
pub const DEFAULT_INTERVAL_SECS: u64 = 6 * 3600;
pub const MIN_INTERVAL_SECS: u64 = 60;
pub const MAX_INTERVAL_SECS: u64 = 7 * 86_400;

const WARN_DAYS: i64 = 14;
const CRITICAL_DAYS: i64 = 3;
const MAX_NAMES: usize = 100;

/// Parses `PYN_CERT_CHECK_INTERVAL_SECS`: unset is the default, 0 is off, otherwise 60 seconds to 7 days.
pub fn interval_from(value: Option<&str>) -> Result<Option<Duration>, String> {
    let secs = match value {
        None => DEFAULT_INTERVAL_SECS,
        Some(v) => v
            .trim()
            .parse::<u64>()
            .map_err(|_| "PYN_CERT_CHECK_INTERVAL_SECS must be a non-negative whole number")?,
    };
    match secs {
        0 => Ok(None),
        MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS => Ok(Some(Duration::from_secs(secs))),
        _ => Err(format!(
            "PYN_CERT_CHECK_INTERVAL_SECS must be 0 (off) or between {MIN_INTERVAL_SECS} and {MAX_INTERVAL_SECS}"
        )),
    }
}

/// Why a public address is not checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    NotHttps,
    Loopback,
    Invalid,
}

impl Skip {
    pub fn reason(self) -> &'static str {
        match self {
            Self::NotHttps => "PYN_PUBLIC_URL is not https",
            Self::Loopback => "PYN_PUBLIC_URL names this machine (localhost or a loopback address)",
            Self::Invalid => "PYN_PUBLIC_URL has no usable host",
        }
    }
}

/// The host and port a handshake goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub host: String,
    pub port: u16,
}

impl Target {
    /// https only; localhost and loopback addresses are skipped because nothing public can be learned from them.
    pub fn from_public_url(url: &str) -> Result<Self, Skip> {
        let url = reqwest::Url::parse(url).map_err(|_| Skip::Invalid)?;
        if url.scheme() != "https" {
            return Err(Skip::NotHttps);
        }
        let raw = url.host_str().ok_or(Skip::Invalid)?;
        let host = raw.trim_start_matches('[').trim_end_matches(']');
        let host = match host.parse::<std::net::IpAddr>() {
            Ok(ip) if ip.is_loopback() || ip.is_unspecified() => return Err(Skip::Loopback),
            Ok(ip) => ip.to_string(),
            Err(_) => {
                let d = host.trim_end_matches('.').to_ascii_lowercase();
                if d == "localhost" || d.ends_with(".localhost") {
                    return Err(Skip::Loopback);
                }
                d
            }
        };
        Ok(Self {
            host,
            port: url.port_or_known_default().unwrap_or(443),
        })
    }
}

/// Timeouts and extra trust anchors of a probe; the defaults suit production.
#[derive(Clone)]
pub struct ProbeOptions {
    pub connect_timeout: Duration,
    pub handshake_timeout: Duration,
    pub extra_roots: Vec<CertificateDer<'static>>,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(5),
            extra_roots: Vec::new(),
        }
    }
}

/// The public facts of a leaf certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertFacts {
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub names: Vec<String>,
    pub issuer: Option<String>,
}

/// A certificate read from a handshake and whether its chain verified.
#[derive(Debug, Clone)]
pub struct Seen {
    pub facts: CertFacts,
    pub trusted: bool,
}

/// Reads the certificate but never refuses it: trust is recorded for the report instead of failing the handshake.
///
/// A private authority (Caddy's internal one) is a normal deployment, and its expiry must still be readable.
/// This is sound only because the probe sends no credentials and no request and closes right after the handshake.
/// Handshake signatures are still checked against the certificate's key.
#[derive(Debug)]
struct Observing {
    inner: Verifier,
    trusted: Arc<AtomicBool>,
}

impl ServerCertVerifier for Observing {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let verdict = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        );
        self.trusted.store(verdict.is_ok(), Ordering::Relaxed);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

fn client_config(
    provider: Arc<CryptoProvider>,
    extra_roots: Vec<CertificateDer<'static>>,
) -> Result<(ClientConfig, Arc<AtomicBool>), String> {
    let inner = Verifier::new_with_extra_roots(extra_roots, provider.clone())
        .map_err(|e| format!("system trust roots unavailable: {e}"))?;
    let trusted = Arc::new(AtomicBool::new(false));
    let verifier = Observing {
        inner,
        trusted: trusted.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok((config, trusted))
}

/// Connects, handshakes and reads the leaf certificate; errors are short and carry no addresses.
pub async fn probe(target: &Target, opts: &ProbeOptions) -> Result<Seen, String> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let roots = opts.extra_roots.clone();
    let (config, trusted) = tokio::task::spawn_blocking(move || client_config(provider, roots))
        .await
        .map_err(|_| "internal error".to_string())??;
    let name = ServerName::try_from(target.host.clone()).map_err(|_| "invalid host name")?;
    let tcp = tokio::time::timeout(
        opts.connect_timeout,
        TcpStream::connect((target.host.as_str(), target.port)),
    )
    .await
    .map_err(|_| "connect timed out".to_string())?
    .map_err(|e| format!("connect failed: {}", io_kind(&e)))?;
    let tls = tokio::time::timeout(
        opts.handshake_timeout,
        TlsConnector::from(Arc::new(config)).connect(name, tcp),
    )
    .await
    .map_err(|_| "TLS handshake timed out (is this a TLS port?)".to_string())?
    .map_err(|e| match e.get_ref() {
        Some(inner) => format!("TLS handshake failed: {inner}"),
        None => format!("TLS handshake failed: {}", io_kind(&e)),
    })?;
    let leaf = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .ok_or("the server presented no certificate")?;
    let facts = parse_certificate(leaf.as_ref())?;
    Ok(Seen {
        facts,
        trusted: trusted.load(Ordering::Relaxed),
    })
}

fn io_kind(e: &std::io::Error) -> String {
    e.kind().to_string()
}

/// What a check concluded, with whether the remaining time is in the critical band.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub state: api::CertificateState,
    pub days_left: i64,
    pub critical: bool,
}

/// `expiring` is less remaining than the smaller of 14 days and a third of the lifetime, so a 12 hour
/// certificate is not flagged for being short-lived; critical is the smaller of 3 days and a quarter of that.
pub fn assess(facts: &CertFacts, now: DateTime<Utc>) -> Assessment {
    let remaining = (facts.not_after - now).num_seconds();
    let lifetime = (facts.not_after - facts.not_before).num_seconds().max(0);
    let warn = (WARN_DAYS * 86_400).min(lifetime / 3);
    let critical = (CRITICAL_DAYS * 86_400).min(warn / 4);
    let state = if remaining <= 0 {
        api::CertificateState::Expired
    } else if remaining < warn {
        api::CertificateState::Expiring
    } else {
        api::CertificateState::Ok
    };
    Assessment {
        state,
        days_left: remaining.div_euclid(86_400),
        critical: remaining < critical || state == api::CertificateState::Expired,
    }
}

/// One check: the report to publish and whether it is critical.
pub async fn check(
    target: &Target,
    opts: &ProbeOptions,
    previous: &api::CertificateSettings,
) -> (api::CertificateSettings, bool) {
    let now = Utc::now();
    let mut report = api::CertificateSettings {
        host: Some(target.host.clone()),
        last_checked: Some(now),
        ..previous.clone()
    };
    match probe(target, opts).await {
        Ok(seen) => {
            let a = assess(&seen.facts, now);
            report.state = a.state;
            report.trusted = Some(seen.trusted);
            report.not_before = Some(seen.facts.not_before);
            report.not_after = Some(seen.facts.not_after);
            report.days_left = Some(a.days_left);
            report.names = seen.facts.names;
            report.issuer = seen.facts.issuer;
            report.last_error = None;
            (report, a.critical)
        }
        Err(e) => {
            report.state = api::CertificateState::Unreachable;
            report.last_error = Some(e);
            (report, false)
        }
    }
}

/// What `GET /v1/admin/settings` reports about the certificate.
pub struct CertificateReport {
    current: Mutex<api::CertificateSettings>,
}

impl Default for CertificateReport {
    fn default() -> Self {
        Self {
            current: Mutex::new(api::CertificateSettings {
                state: api::CertificateState::Unchecked,
                host: None,
                trusted: None,
                not_before: None,
                not_after: None,
                days_left: None,
                names: Vec::new(),
                issuer: None,
                last_checked: None,
                last_error: None,
            }),
        }
    }
}

impl CertificateReport {
    pub fn settings(&self) -> api::CertificateSettings {
        self.current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn set(&self, next: api::CertificateSettings) {
        *self.current.lock().unwrap_or_else(|p| p.into_inner()) = next;
    }
}

/// Logs a check: every non-ok result each pass (so once per interval), a recovery once.
fn log_outcome(before: &api::CertificateSettings, now: &api::CertificateSettings, critical: bool) {
    use api::CertificateState as S;
    let host = now.host.as_deref().unwrap_or_default();
    let days = now.days_left.unwrap_or_default();
    match now.state {
        S::Expired => tracing::error!(host, "the TLS certificate has expired"),
        S::Expiring if critical => {
            tracing::error!(
                host,
                days_left = days,
                "the TLS certificate is about to expire"
            )
        }
        S::Expiring => {
            tracing::warn!(
                host,
                days_left = days,
                "the TLS certificate is expiring soon"
            )
        }
        S::Unreachable => tracing::warn!(
            host,
            error = now.last_error.as_deref().unwrap_or_default(),
            "the TLS certificate check failed"
        ),
        S::Ok | S::Unchecked => {
            if matches!(before.state, S::Expired | S::Expiring | S::Unreachable) {
                tracing::info!(
                    host,
                    days_left = days,
                    "the TLS certificate is healthy again"
                );
            }
        }
    }
    if now.trusted == Some(false) && before.trusted != Some(false) {
        tracing::warn!(
            host,
            "the TLS certificate is not trusted by the system roots (a private authority or a name mismatch)"
        );
    }
}

/// Checks after `first_after`, then every `every`, until draining starts.
pub fn spawn_monitor(
    target: Target,
    report: Arc<CertificateReport>,
    every: Duration,
    first_after: Duration,
    drain: Drain,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if !drain.sleep(first_after).await {
            return;
        }
        let opts = ProbeOptions::default();
        let mut tick = tokio::time::interval(every);
        tick.tick().await;
        loop {
            let before = report.settings();
            let (now, critical) = check(&target, &opts, &before).await;
            log_outcome(&before, &now, critical);
            report.set(now);
            if !drain.tick(&mut tick).await {
                break;
            }
        }
    })
}

#[derive(Debug)]
struct Malformed;

impl From<Malformed> for String {
    fn from(_: Malformed) -> Self {
        "the certificate could not be read".into()
    }
}

/// A cursor over DER elements: `(tag, content)` pairs. Only short-form tags and lengths up to 4 bytes are accepted.
struct Der<'a>(&'a [u8]);

impl<'a> Der<'a> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn peek_tag(&self) -> Option<u8> {
        self.0.first().copied()
    }

    fn next(&mut self) -> Result<(u8, &'a [u8]), Malformed> {
        let (&tag, rest) = self.0.split_first().ok_or(Malformed)?;
        if tag & 0x1f == 0x1f {
            return Err(Malformed);
        }
        let (&first, rest) = rest.split_first().ok_or(Malformed)?;
        let (len, rest) = if first < 0x80 {
            (usize::from(first), rest)
        } else {
            let n = usize::from(first & 0x7f);
            if n == 0 || n > 4 || rest.len() < n {
                return Err(Malformed);
            }
            let len = rest[..n]
                .iter()
                .fold(0usize, |a, b| (a << 8) | usize::from(*b));
            (len, &rest[n..])
        };
        if rest.len() < len {
            return Err(Malformed);
        }
        self.0 = &rest[len..];
        Ok((tag, &rest[..len]))
    }

    fn expect(&mut self, tag: u8) -> Result<&'a [u8], Malformed> {
        match self.next()? {
            (t, c) if t == tag => Ok(c),
            _ => Err(Malformed),
        }
    }
}

const SEQUENCE: u8 = 0x30;
const OID_COMMON_NAME: &[u8] = &[0x55, 0x04, 0x03];
const OID_SUBJECT_ALT_NAME: &[u8] = &[0x55, 0x1d, 0x11];

/// Reads the validity, the names and the issuer's common name from a DER X.509 certificate; nothing is verified.
pub fn parse_certificate(der: &[u8]) -> Result<CertFacts, String> {
    Ok(parse(der)?)
}

fn parse(der: &[u8]) -> Result<CertFacts, Malformed> {
    let mut cert = Der(der);
    let mut tbs = Der(cert.expect(SEQUENCE)?);
    let mut tbs = Der(tbs.expect(SEQUENCE)?);
    if tbs.peek_tag() == Some(0xa0) {
        tbs.next()?;
    }
    tbs.expect(0x02)?;
    tbs.expect(SEQUENCE)?;
    let issuer = tbs.expect(SEQUENCE)?;
    let mut validity = Der(tbs.expect(SEQUENCE)?);
    let not_before = time(validity.next()?)?;
    let not_after = time(validity.next()?)?;
    let subject = tbs.expect(SEQUENCE)?;
    tbs.expect(SEQUENCE)?;
    let mut names = Vec::new();
    while !tbs.is_empty() {
        let (tag, content) = tbs.next()?;
        if tag != 0xa3 {
            continue;
        }
        let mut extensions = Der(Der(content).expect(SEQUENCE)?);
        while !extensions.is_empty() {
            let mut ext = Der(extensions.expect(SEQUENCE)?);
            let oid = ext.expect(0x06)?;
            if ext.peek_tag() == Some(0x01) {
                ext.next()?;
            }
            if oid == OID_SUBJECT_ALT_NAME {
                names = alt_names(ext.expect(0x04)?)?;
            }
        }
    }
    if names.is_empty() {
        names.extend(common_name(subject)?);
    }
    names.truncate(MAX_NAMES);
    Ok(CertFacts {
        not_before,
        not_after,
        names,
        issuer: common_name(issuer)?,
    })
}

fn alt_names(value: &[u8]) -> Result<Vec<String>, Malformed> {
    let mut list = Der(Der(value).expect(SEQUENCE)?);
    let mut names = Vec::new();
    while !list.is_empty() {
        match list.next()? {
            (0x82, dns) => names.push(text(dns)),
            (0x87, ip) => match ip.len() {
                4 => names.push(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]).to_string()),
                16 => {
                    let octets: [u8; 16] = ip.try_into().map_err(|_| Malformed)?;
                    names.push(Ipv6Addr::from(octets).to_string());
                }
                _ => {}
            },
            _ => {}
        }
    }
    Ok(names)
}

fn common_name(name: &[u8]) -> Result<Option<String>, Malformed> {
    let mut rdns = Der(name);
    while !rdns.is_empty() {
        let mut set = Der(rdns.expect(0x31)?);
        while !set.is_empty() {
            let mut pair = Der(set.expect(SEQUENCE)?);
            let oid = pair.expect(0x06)?;
            let (tag, value) = pair.next()?;
            if oid == OID_COMMON_NAME && matches!(tag, 0x0c | 0x13 | 0x14 | 0x16) {
                return Ok(Some(text(value)));
            }
        }
    }
    Ok(None)
}

/// Certificate text is the CA's input: control characters are dropped and the length is bounded.
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control())
        .take(253)
        .collect()
}

/// A UTCTime (two-digit years from 50 are 19xx) or a GeneralizedTime in `Z` form.
fn time((tag, value): (u8, &[u8])) -> Result<DateTime<Utc>, Malformed> {
    let digits = |s: &[u8]| -> Result<u32, Malformed> {
        if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
            return Err(Malformed);
        }
        Ok(s.iter().fold(0, |a, d| a * 10 + u32::from(d - b'0')))
    };
    let (year, rest) = match (tag, value) {
        (0x17, v) if v.len() == 13 => {
            let yy = digits(&v[..2])?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &v[2..])
        }
        (0x18, v) if v.len() == 15 => (digits(&v[..4])?, &v[4..]),
        _ => return Err(Malformed),
    };
    if rest.last() != Some(&b'Z') {
        return Err(Malformed);
    }
    let field = |i: usize| digits(&rest[i * 2..i * 2 + 2]);
    let (h, mi, sec) = (field(2)?, field(3)?, field(4)?);
    NaiveDate::from_ymd_opt(year as i32, field(0)?, field(1)?)
        .and_then(|d| d.and_hms_opt(h, mi, sec))
        .map(|d| d.and_utc())
        .ok_or(Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(url: &str) -> Result<Target, Skip> {
        Target::from_public_url(url)
    }

    #[test]
    fn only_a_public_https_host_is_checked() {
        let t = target("https://Pyn.Example/").unwrap();
        assert_eq!((t.host.as_str(), t.port), ("pyn.example", 443));
        assert_eq!(target("https://pyn.example:8443").unwrap().port, 8443);
        assert_eq!(target("https://203.0.113.9").unwrap().host, "203.0.113.9");
        assert_eq!(target("https://[2001:db8::1]").unwrap().host, "2001:db8::1");
        assert_eq!(target("http://pyn.example"), Err(Skip::NotHttps));
        for local in [
            "https://localhost",
            "https://app.localhost:8443",
            "https://127.0.0.1:9",
            "https://127.5.5.5",
            "https://[::1]",
            "https://0.0.0.0",
        ] {
            assert_eq!(target(local), Err(Skip::Loopback), "{local}");
        }
        assert_eq!(target("not a url"), Err(Skip::Invalid));
    }

    #[test]
    fn the_interval_has_a_default_an_off_switch_and_bounds() {
        assert_eq!(interval_from(None), Ok(Some(Duration::from_secs(21_600))));
        assert_eq!(interval_from(Some("0")), Ok(None));
        assert_eq!(interval_from(Some("60")), Ok(Some(Duration::from_secs(60))));
        assert!(interval_from(Some("59")).is_err());
        assert!(interval_from(Some("604801")).is_err());
        assert!(interval_from(Some("soon")).is_err());
    }

    fn facts(days_total: i64, days_in: i64) -> (CertFacts, DateTime<Utc>) {
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let f = CertFacts {
            not_before: start,
            not_after: start + chrono::Duration::days(days_total),
            names: vec![],
            issuer: None,
        };
        (f, start + chrono::Duration::days(days_in))
    }

    #[test]
    fn a_ninety_day_certificate_expires_at_fourteen_days_and_is_critical_at_three() {
        use api::CertificateState as S;
        let at = |left: i64| {
            let (f, now) = facts(90, 90 - left);
            assess(&f, now)
        };
        assert_eq!((at(20).state, at(20).critical), (S::Ok, false));
        assert_eq!((at(14).state, at(14).critical), (S::Ok, false));
        assert_eq!((at(13).state, at(13).critical), (S::Expiring, false));
        assert_eq!((at(2).state, at(2).critical), (S::Expiring, true));
        assert_eq!((at(0).state, at(0).critical), (S::Expired, true));
        assert_eq!(at(13).days_left, 13);
    }

    #[test]
    fn a_short_lived_certificate_is_not_expiring_for_being_short_lived() {
        use api::CertificateState as S;
        let start = DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let f = CertFacts {
            not_before: start,
            not_after: start + chrono::Duration::hours(12),
            names: vec![],
            issuer: None,
        };
        let at = |h: i64, m: i64| {
            assess(
                &f,
                start + chrono::Duration::hours(h) + chrono::Duration::minutes(m),
            )
        };
        assert_eq!(at(1, 0).state, S::Ok);
        assert_eq!(at(7, 0).state, S::Ok);
        assert_eq!(at(9, 0).state, S::Expiring);
        assert!(!at(9, 0).critical);
        assert!(at(11, 30).critical);
        assert_eq!(at(12, 1).state, S::Expired);
        assert_eq!(at(12, 1).days_left, -1);
    }

    #[test]
    fn days_left_rounds_down() {
        let (f, now) = facts(90, 0);
        assert_eq!(assess(&f, now).days_left, 90);
        let (f, now) = facts(90, 1);
        let later = now + chrono::Duration::hours(1);
        assert_eq!(assess(&f, later).days_left, 88);
    }

    #[test]
    fn times_follow_the_two_digit_year_rule() {
        let t = |tag, s: &str| time((tag, s.as_bytes())).unwrap();
        assert_eq!(
            t(0x17, "491231235959Z").to_rfc3339(),
            "2049-12-31T23:59:59+00:00"
        );
        assert_eq!(
            t(0x17, "500101000000Z").to_rfc3339(),
            "1950-01-01T00:00:00+00:00"
        );
        assert_eq!(
            t(0x18, "20511231235959Z").to_rfc3339(),
            "2051-12-31T23:59:59+00:00"
        );
        assert!(time((0x17, b"2501011200+0100")).is_err());
        assert!(time((0x17, b"251301000000Z")).is_err());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for bad in [
            &[][..],
            &[0x30],
            &[0x30, 0x84, 0xff, 0xff, 0xff, 0xff],
            b"hello",
            &[0x30, 0x02, 0x30, 0x05],
        ] {
            assert!(parse_certificate(bad).is_err());
        }
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Capture {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn logged(
        before: api::CertificateState,
        state: api::CertificateState,
        critical: bool,
    ) -> String {
        let out = Capture::default();
        let writer = out.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        let mut report = CertificateReport::default().settings();
        report.host = Some("pyn.example".into());
        report.days_left = Some(2);
        let mut earlier = report.clone();
        earlier.state = before;
        report.state = state;
        tracing::subscriber::with_default(subscriber, || log_outcome(&earlier, &report, critical));
        String::from_utf8(out.0.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn warnings_are_warn_then_error_and_recovery_is_logged_once() {
        use api::CertificateState as S;
        assert!(logged(S::Ok, S::Expiring, false).contains("WARN"));
        assert!(logged(S::Expiring, S::Expiring, true).contains("ERROR"));
        assert!(logged(S::Expiring, S::Expired, true).contains("ERROR"));
        assert!(logged(S::Ok, S::Unreachable, false).contains("check failed"));
        assert!(logged(S::Expiring, S::Ok, false).contains("healthy again"));
        assert_eq!(logged(S::Ok, S::Ok, false), "");
    }
}

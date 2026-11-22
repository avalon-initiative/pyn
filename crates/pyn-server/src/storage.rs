use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pyn_proto as api;
use pyn_s3::{S3ObjectStore, S3Settings};

use crate::shutdown::Drain;

const S3_VARIABLES: [&str; 13] = [
    "PYN_S3_ENDPOINT",
    "PYN_S3_REGION",
    "PYN_S3_BUCKET",
    "PYN_S3_ACCESS_KEY_ID",
    "PYN_S3_SECRET_ACCESS_KEY",
    "PYN_S3_PREFIX",
    "PYN_S3_PATH_STYLE",
    "PYN_S3_SPOOL_DIR",
    "PYN_S3_SPOOL_THRESHOLD",
    "PYN_S3_REQUEST_TIMEOUT_SECS",
    "PYN_S3_MAX_RETRIES",
    "PYN_S3_ACCESS_KEY_ID_FILE",
    "PYN_S3_SECRET_ACCESS_KEY_FILE",
];

/// How often a running S3 store is probed for the health report.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(30);

const MAX_RETRIES: u32 = 20;

/// Where file content lives, as selected by the environment.
#[derive(Debug)]
pub enum ObjectStoreSetting {
    Memory,
    Filesystem(PathBuf),
    S3(Box<S3Settings>),
}

impl ObjectStoreSetting {
    /// Reads `PYN_DATA_DIR` and `PYN_S3_*` through `get`; `read` loads the file behind a `_FILE` variable.
    pub fn from_env(
        get: impl Fn(&str) -> Option<String>,
        read: impl Fn(&str) -> std::io::Result<String>,
    ) -> Result<Self, String> {
        let data_dir = get("PYN_DATA_DIR");
        let set = |name: &str| get(name).filter(|v| !v.is_empty());
        if set("PYN_S3_BUCKET").is_none() {
            if let Some(stray) = S3_VARIABLES.iter().find(|n| set(n).is_some()) {
                return Err(format!(
                    "{stray} is set without PYN_S3_BUCKET; set the bucket to use the S3 store, or remove the PYN_S3_* settings"
                ));
            }
            return Ok(data_dir.map_or(Self::Memory, |d| Self::Filesystem(d.into())));
        }
        if data_dir.is_some() {
            return Err(
                "PYN_S3_BUCKET and PYN_DATA_DIR are both set; file content lives in the bucket or in the data directory, not both (the server keeps nothing else in the data directory)"
                    .into(),
            );
        }
        s3_settings(&set, &read).map(|s| Self::S3(Box::new(s)))
    }

    /// Whether several copies of the server can use it at once.
    pub fn is_shared(&self) -> bool {
        !matches!(self, Self::Memory)
    }
}

/// The bucket settings from `PYN_S3_*` alone, for commands that name the data directory themselves.
pub fn s3_from_env(
    get: impl Fn(&str) -> Option<String>,
    read: impl Fn(&str) -> std::io::Result<String>,
) -> Result<S3Settings, String> {
    let set = |name: &str| get(name).filter(|v| !v.is_empty());
    if set("PYN_S3_BUCKET").is_none() {
        return Err(
            "PYN_S3_BUCKET is not set; the bucket is named by the PYN_S3_* settings".into(),
        );
    }
    s3_settings(&set, &read)
}

/// `NAME` or the file named by `NAME_FILE`, never both; a file's one trailing newline is dropped. Errors never carry a value.
fn secret(
    name: &str,
    set: &impl Fn(&str) -> Option<String>,
    read: &impl Fn(&str) -> std::io::Result<String>,
) -> Result<String, String> {
    let file = format!("{name}_FILE");
    match (set(name), set(&file)) {
        (Some(_), Some(_)) => Err(format!("{name} and {file} are both set; use one")),
        (Some(value), None) => Ok(value),
        (None, Some(path)) => {
            let text =
                read(&path).map_err(|e| format!("{file}: cannot read {path}: {}", e.kind()))?;
            let text = text.strip_suffix('\n').unwrap_or(&text);
            let text = text.strip_suffix('\r').unwrap_or(text);
            if text.is_empty() {
                return Err(format!("{file}: {path} is empty"));
            }
            Ok(text.to_string())
        }
        (None, None) => Err(format!(
            "PYN_S3_BUCKET needs {name} (or {file}, a file holding it)"
        )),
    }
}

fn s3_settings(
    set: &impl Fn(&str) -> Option<String>,
    read: &impl Fn(&str) -> std::io::Result<String>,
) -> Result<S3Settings, String> {
    let endpoint = set("PYN_S3_ENDPOINT").ok_or(
        "PYN_S3_BUCKET needs PYN_S3_ENDPOINT, such as https://nyc3.digitaloceanspaces.com",
    )?;
    let bucket = set("PYN_S3_BUCKET").unwrap_or_default();
    check_endpoint(&endpoint)?;
    let mut settings = S3Settings::new(
        endpoint,
        bucket,
        secret("PYN_S3_ACCESS_KEY_ID", set, read)?,
        secret("PYN_S3_SECRET_ACCESS_KEY", set, read)?,
    );
    if let Some(region) = set("PYN_S3_REGION") {
        settings.region = region;
    }
    if let Some(prefix) = set("PYN_S3_PREFIX") {
        settings.prefix = prefix;
    }
    settings.path_style = match set("PYN_S3_PATH_STYLE").as_deref() {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return Err("PYN_S3_PATH_STYLE must be true or false".into()),
    };
    if let Some(dir) = set("PYN_S3_SPOOL_DIR") {
        settings.spool_dir = dir.into();
    }
    if let Some(bytes) = set("PYN_S3_SPOOL_THRESHOLD") {
        let bytes = whole("PYN_S3_SPOOL_THRESHOLD", &bytes)?;
        settings.spool_threshold = usize::try_from(bytes)
            .map_err(|_| "PYN_S3_SPOOL_THRESHOLD is too large".to_string())?;
    }
    if let Some(secs) = set("PYN_S3_REQUEST_TIMEOUT_SECS") {
        let secs = whole("PYN_S3_REQUEST_TIMEOUT_SECS", &secs)?;
        if secs == 0 {
            return Err("PYN_S3_REQUEST_TIMEOUT_SECS must be at least 1".into());
        }
        settings.request_timeout = Duration::from_secs(secs);
    }
    if let Some(retries) = set("PYN_S3_MAX_RETRIES") {
        let retries = whole("PYN_S3_MAX_RETRIES", &retries)?;
        if retries > u64::from(MAX_RETRIES) {
            return Err(format!("PYN_S3_MAX_RETRIES must be at most {MAX_RETRIES}"));
        }
        settings.max_attempts = retries as u32 + 1;
    }
    Ok(settings)
}

/// Rejects a value that is not an http(s) address; the value is not echoed, as it could carry a password.
fn check_endpoint(endpoint: &str) -> Result<(), String> {
    let bad = |why: &str| format!("PYN_S3_ENDPOINT {why}");
    let url = reqwest::Url::parse(endpoint)
        .map_err(|_| bad("must be an address such as https://nyc3.digitaloceanspaces.com"))?;
    if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
        return Err(bad(
            "must start with https:// (or http:// for a local server)",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(bad(
            "must not contain a user name or password; use PYN_S3_ACCESS_KEY_ID and PYN_S3_SECRET_ACCESS_KEY",
        ));
    }
    Ok(())
}

fn whole(name: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} must be a non-negative whole number"))
}

/// Several copies need a PostgreSQL database and content storage they all reach: a bucket or a shared data directory.
pub fn check_instances(instances: u32, postgres: bool, shared_content: bool) -> Result<(), String> {
    if instances == 1 || (postgres && shared_content) {
        return Ok(());
    }
    Err("PYN_INSTANCES above 1 needs PYN_DATABASE_URL and shared content storage: PYN_S3_BUCKET, or PYN_DATA_DIR on storage all copies share".into())
}

/// What `GET /v1/admin/settings` reports about the object store.
pub struct StorageReport {
    kind: api::StorageKind,
    bucket: Option<String>,
    degraded: AtomicBool,
}

impl Default for StorageReport {
    fn default() -> Self {
        Self::new(api::StorageKind::Memory, None)
    }
}

impl StorageReport {
    pub fn new(kind: api::StorageKind, bucket: Option<String>) -> Self {
        Self {
            kind,
            bucket,
            degraded: AtomicBool::new(false),
        }
    }

    pub fn set_degraded(&self, degraded: bool) {
        self.degraded.store(degraded, Ordering::Relaxed);
    }

    pub fn settings(&self) -> api::StorageSettings {
        api::StorageSettings {
            kind: self.kind,
            bucket: self.bucket.clone(),
            status: if self.degraded.load(Ordering::Relaxed) {
                api::StorageStatus::Degraded
            } else {
                api::StorageStatus::Ok
            },
        }
    }
}

/// Opens the store, sweeps stale spool files and probes it once; a failure stops the server at start.
pub async fn open_s3(settings: S3Settings) -> anyhow::Result<Arc<S3ObjectStore>> {
    let (bucket, host) = (settings.bucket.clone(), settings.endpoint.clone());
    let store = S3ObjectStore::open(settings)
        .await
        .map_err(|e| anyhow::anyhow!("PYN_S3_*: {e}"))?;
    store.probe().await.map_err(|e| {
        anyhow::anyhow!(
            "the S3 store (bucket {bucket}, endpoint host {}) failed its start-up check: {e}",
            endpoint_host(&host)
        )
    })?;
    Ok(Arc::new(store))
}

fn endpoint_host(endpoint: &str) -> String {
    reqwest::Url::parse(endpoint)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

/// Probes the store every `every` until draining starts and records whether it answered; logs only when the state changes.
pub fn spawn_monitor(
    store: Arc<S3ObjectStore>,
    report: Arc<StorageReport>,
    every: Duration,
    drain: Drain,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.tick().await;
        while drain.tick(&mut tick).await {
            let result = store.probe().await;
            let was = report.degraded.swap(result.is_err(), Ordering::Relaxed);
            match result {
                Err(e) if !was => {
                    tracing::error!("the S3 store is unreachable or refusing requests: {e}")
                }
                Ok(()) if was => tracing::info!("the S3 store answers again"),
                _ => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    const BASE: [(&str, &str); 4] = [
        ("PYN_S3_ENDPOINT", "https://nyc3.digitaloceanspaces.com"),
        ("PYN_S3_BUCKET", "pyn-prod"),
        ("PYN_S3_ACCESS_KEY_ID", "AKID"),
        ("PYN_S3_SECRET_ACCESS_KEY", "s3cret-value"),
    ];

    fn no_files(_: &str) -> std::io::Result<String> {
        Err(std::io::ErrorKind::NotFound.into())
    }

    fn parse(extra: &[(&str, &str)]) -> Result<ObjectStoreSetting, String> {
        let mut pairs = BASE.to_vec();
        pairs.extend_from_slice(extra);
        ObjectStoreSetting::from_env(env(&pairs), no_files)
    }

    fn s3(extra: &[(&str, &str)]) -> S3Settings {
        match parse(extra).unwrap() {
            ObjectStoreSetting::S3(s) => *s,
            other => panic!("S3 expected, got {other:?}"),
        }
    }

    #[test]
    fn the_bucket_selects_s3_with_documented_defaults() {
        let s = s3(&[]);
        assert_eq!((s.region.as_str(), s.prefix.as_str()), ("us-east-1", ""));
        assert!(!s.path_style);
        assert_eq!(s.spool_threshold, 8 * 1024 * 1024);
        assert_eq!(s.max_attempts, 5);
        assert!(s.spool_dir.ends_with("pyn-s3-spool"));
        assert!(parse(&[]).unwrap().is_shared());
    }

    #[test]
    fn the_bucket_settings_are_read_alone_and_ignore_the_data_directory() {
        let mut pairs = BASE.to_vec();
        pairs.push(("PYN_DATA_DIR", "/data"));
        let s = s3_from_env(env(&pairs), no_files).unwrap();
        assert_eq!(s.bucket, "pyn-prod");
        let err = s3_from_env(env(&[("PYN_DATA_DIR", "/data")]), no_files).unwrap_err();
        assert!(err.contains("PYN_S3_BUCKET"), "{err}");
    }

    #[test]
    fn without_a_bucket_the_data_directory_or_memory_is_used() {
        let get = env(&[("PYN_DATA_DIR", "/data")]);
        let chosen = ObjectStoreSetting::from_env(get, no_files).unwrap();
        assert!(matches!(chosen, ObjectStoreSetting::Filesystem(_)) && chosen.is_shared());
        let none = ObjectStoreSetting::from_env(env(&[]), no_files).unwrap();
        assert!(matches!(none, ObjectStoreSetting::Memory) && !none.is_shared());
    }

    #[test]
    fn every_setting_is_read() {
        let s = s3(&[
            ("PYN_S3_REGION", "nyc3"),
            ("PYN_S3_PREFIX", "pyn/"),
            ("PYN_S3_PATH_STYLE", "true"),
            ("PYN_S3_SPOOL_DIR", "/var/spool/pyn"),
            ("PYN_S3_SPOOL_THRESHOLD", "1024"),
            ("PYN_S3_REQUEST_TIMEOUT_SECS", "7"),
            ("PYN_S3_MAX_RETRIES", "0"),
        ]);
        assert_eq!((s.region.as_str(), s.prefix.as_str()), ("nyc3", "pyn/"));
        assert!(s.path_style);
        assert_eq!(s.spool_dir, PathBuf::from("/var/spool/pyn"));
        assert_eq!(s.spool_threshold, 1024);
        assert_eq!(s.request_timeout, Duration::from_secs(7));
        assert_eq!(s.max_attempts, 1);
    }

    #[test]
    fn the_bucket_and_the_data_directory_together_are_refused() {
        let err = parse(&[("PYN_DATA_DIR", "/data")]).unwrap_err();
        assert!(err.contains("PYN_S3_BUCKET") && err.contains("PYN_DATA_DIR"));
        assert!(!err.contains("s3cret-value"));
    }

    #[test]
    fn s3_settings_without_the_bucket_are_refused() {
        for name in [
            "PYN_S3_REGION",
            "PYN_S3_ENDPOINT",
            "PYN_S3_ACCESS_KEY_ID_FILE",
        ] {
            let err = ObjectStoreSetting::from_env(env(&[(name, "x")]), no_files).unwrap_err();
            assert!(err.contains(name) && err.contains("PYN_S3_BUCKET"), "{err}");
        }
        let empty = env(&[("PYN_S3_REGION", "")]);
        assert!(ObjectStoreSetting::from_env(empty, no_files).is_ok());
    }

    #[test]
    fn incomplete_settings_name_what_is_missing_without_echoing_the_secret() {
        for missing in [
            "PYN_S3_ENDPOINT",
            "PYN_S3_ACCESS_KEY_ID",
            "PYN_S3_SECRET_ACCESS_KEY",
        ] {
            let pairs: Vec<_> = BASE.iter().filter(|(n, _)| *n != missing).collect();
            let get = |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            };
            let err = ObjectStoreSetting::from_env(get, no_files).unwrap_err();
            assert!(err.contains(missing), "{err}");
            assert!(!err.contains("s3cret-value"), "{err}");
        }
    }

    #[test]
    fn bad_values_are_named_and_never_echoed() {
        for (name, value) in [
            ("PYN_S3_PATH_STYLE", "yes"),
            ("PYN_S3_SPOOL_THRESHOLD", "8MiB"),
            ("PYN_S3_REQUEST_TIMEOUT_SECS", "0"),
            ("PYN_S3_REQUEST_TIMEOUT_SECS", "soon"),
            ("PYN_S3_MAX_RETRIES", "-1"),
            ("PYN_S3_MAX_RETRIES", "21"),
        ] {
            let err = parse(&[(name, value)]).unwrap_err();
            assert!(err.contains(name), "{name}={value}: {err}");
        }
        for endpoint in [
            "nyc3.digitaloceanspaces.com",
            "ftp://host",
            "https://user:hunter2@host",
            "https://hunter2@host",
        ] {
            let pairs = [
                ("PYN_S3_ENDPOINT", endpoint),
                ("PYN_S3_BUCKET", "b"),
                ("PYN_S3_ACCESS_KEY_ID", "a"),
                ("PYN_S3_SECRET_ACCESS_KEY", "s"),
            ];
            let err = ObjectStoreSetting::from_env(env(&pairs), no_files).unwrap_err();
            assert!(err.contains("PYN_S3_ENDPOINT"), "{err}");
            assert!(!err.contains("hunter2"), "{err}");
        }
    }

    #[test]
    fn a_file_variant_supplies_the_value_minus_one_trailing_newline() {
        let files = |path: &str| match path {
            "/run/secrets/key" => Ok("file-secret\n".to_string()),
            "/run/secrets/id" => Ok("file-id\r\n".to_string()),
            "/run/secrets/blank" => Ok("\n".to_string()),
            _ => Err(std::io::ErrorKind::NotFound.into()),
        };
        let pairs = [
            ("PYN_S3_ENDPOINT", "https://s3.example"),
            ("PYN_S3_BUCKET", "b"),
            ("PYN_S3_ACCESS_KEY_ID_FILE", "/run/secrets/id"),
            ("PYN_S3_SECRET_ACCESS_KEY_FILE", "/run/secrets/key"),
        ];
        let ObjectStoreSetting::S3(s) = ObjectStoreSetting::from_env(env(&pairs), files).unwrap()
        else {
            panic!("s3 expected")
        };
        assert_eq!(
            (s.access_key_id.as_str(), s.secret_access_key.as_str()),
            ("file-id", "file-secret")
        );

        let blank = [
            ("PYN_S3_SECRET_ACCESS_KEY_FILE", "/run/secrets/blank"),
            ("PYN_S3_ACCESS_KEY_ID", "a"),
            ("PYN_S3_ENDPOINT", "https://s3.example"),
            ("PYN_S3_BUCKET", "b"),
        ];
        let err = ObjectStoreSetting::from_env(env(&blank), files).unwrap_err();
        assert!(err.contains("PYN_S3_SECRET_ACCESS_KEY_FILE") && err.contains("empty"));
    }

    #[test]
    fn a_value_and_its_file_variant_together_are_refused() {
        let err = parse(&[("PYN_S3_SECRET_ACCESS_KEY_FILE", "/x")]).unwrap_err();
        assert!(err.contains("PYN_S3_SECRET_ACCESS_KEY_FILE") && err.contains("both set"));
        assert!(!err.contains("s3cret-value"));
    }

    #[test]
    fn an_unreadable_file_is_named_with_its_path_and_never_its_contents() {
        let pairs = [
            ("PYN_S3_ENDPOINT", "https://s3.example"),
            ("PYN_S3_BUCKET", "b"),
            ("PYN_S3_ACCESS_KEY_ID", "a"),
            ("PYN_S3_SECRET_ACCESS_KEY_FILE", "/run/secrets/gone"),
        ];
        let err = ObjectStoreSetting::from_env(env(&pairs), no_files).unwrap_err();
        assert!(err.contains("PYN_S3_SECRET_ACCESS_KEY_FILE"), "{err}");
        assert!(err.contains("/run/secrets/gone"), "{err}");
    }

    #[test]
    fn several_copies_need_a_database_and_shared_content() {
        assert!(check_instances(1, false, false).is_ok());
        assert!(check_instances(2, true, true).is_ok());
        for (postgres, shared) in [(false, true), (true, false), (false, false)] {
            let err = check_instances(2, postgres, shared).unwrap_err();
            assert!(err.contains("PYN_DATABASE_URL") && err.contains("PYN_S3_BUCKET"));
        }
    }

    #[test]
    fn the_report_starts_ok_and_turns_degraded() {
        let report = StorageReport::new(api::StorageKind::S3, Some("b".into()));
        assert_eq!(report.settings().status, api::StorageStatus::Ok);
        report.set_degraded(true);
        let shown = report.settings();
        assert_eq!(shown.status, api::StorageStatus::Degraded);
        assert_eq!(shown.bucket.as_deref(), Some("b"));
    }
}

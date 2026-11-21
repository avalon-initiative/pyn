use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// Largest object one PutObject may carry on AWS S3; bigger spooled content goes by multipart.
pub const MAX_SINGLE_PUT: u64 = 5 * 1024 * 1024 * 1024;

/// How to reach one bucket. `Debug` redacts the secret key.
#[derive(Clone)]
pub struct S3Settings {
    /// Base URL of the service, e.g. `https://nyc3.digitaloceanspaces.com` (`http://` is allowed).
    pub endpoint: String,
    /// The region requests are signed for.
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Prepended to every key; a trailing `/` is added when missing.
    pub prefix: String,
    /// `https://host/bucket/key` instead of `https://bucket.host/key`.
    pub path_style: bool,
    /// Where content over `spool_threshold` waits while it is hashed; swept of stale files at open.
    pub spool_dir: PathBuf,
    pub spool_threshold: usize,
    pub connect_timeout: Duration,
    /// Whole-request limit for calls without a large body (HEAD, DELETE, multipart control).
    pub request_timeout: Duration,
    /// Longest silence tolerated while a body is moving.
    pub io_timeout: Duration,
    /// Tries per request, the first included.
    pub max_attempts: u32,
    pub retry_base: Duration,
    pub retry_max: Duration,
    /// Spooled content over this many bytes is uploaded by multipart.
    pub multipart_threshold: u64,
    pub part_size: u64,
}

impl S3Settings {
    pub fn new(
        endpoint: impl Into<String>,
        bucket: impl Into<String>,
        access_key_id: impl Into<String>,
        secret_access_key: impl Into<String>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            region: "us-east-1".into(),
            bucket: bucket.into(),
            access_key_id: access_key_id.into(),
            secret_access_key: secret_access_key.into(),
            prefix: String::new(),
            path_style: false,
            spool_dir: std::env::temp_dir().join("pyn-s3-spool"),
            spool_threshold: 8 * 1024 * 1024,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            io_timeout: Duration::from_secs(60),
            max_attempts: 5,
            retry_base: Duration::from_millis(200),
            retry_max: Duration::from_secs(10),
            multipart_threshold: MAX_SINGLE_PUT,
            part_size: 64 * 1024 * 1024,
        }
    }
}

impl fmt::Debug for S3Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Settings")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("prefix", &self.prefix)
            .field("path_style", &self.path_style)
            .field("spool_dir", &self.spool_dir)
            .field("spool_threshold", &self.spool_threshold)
            .field("connect_timeout", &self.connect_timeout)
            .field("request_timeout", &self.request_timeout)
            .field("io_timeout", &self.io_timeout)
            .field("max_attempts", &self.max_attempts)
            .field("retry_base", &self.retry_base)
            .field("retry_max", &self.retry_max)
            .field("multipart_threshold", &self.multipart_threshold)
            .field("part_size", &self.part_size)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_contains_the_secret_key() {
        let s = S3Settings::new("https://s3.example", "b", "AKID", "very-secret-value");
        let shown = format!("{s:?} {s:#?}");
        assert!(!shown.contains("very-secret-value"));
        assert!(shown.contains("<redacted>"));
        assert!(shown.contains("AKID"));
    }
}

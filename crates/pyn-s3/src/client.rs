//! The HTTP side: signed requests, retries and error mapping. One method per S3 operation the store uses.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use pyn_core::{PynError, Result};
use reqwest::{Method, Response, StatusCode, Url};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::settings::S3Settings;
use crate::sigv4::{self, Request};
use crate::xml;

/// A request body: nothing, bytes in memory, or a byte range of a file read again on every attempt.
#[derive(Clone)]
pub(crate) enum Body {
    Empty,
    Bytes(Bytes),
    File {
        path: PathBuf,
        offset: u64,
        len: u64,
    },
}

pub(crate) struct Call<'a> {
    pub op: &'static str,
    pub method: Method,
    pub key: &'a str,
    pub query: Vec<(&'a str, String)>,
    pub range: Option<String>,
    pub body: Body,
    pub payload_sha256: String,
    pub retry: bool,
    pub bounded: bool,
}

impl<'a> Call<'a> {
    pub fn new(op: &'static str, method: Method, key: &'a str) -> Self {
        Self {
            op,
            method,
            key,
            query: Vec::new(),
            range: None,
            body: Body::Empty,
            payload_sha256: sigv4::EMPTY_SHA256.to_owned(),
            retry: true,
            bounded: true,
        }
    }
}

pub(crate) struct Client {
    http: reqwest::Client,
    settings: S3Settings,
    base: Url,
    host: String,
}

fn transport(op: &str, host: &str, e: reqwest::Error) -> PynError {
    let e = e.without_url();
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    PynError::Storage(format!("S3 {op} failed: cannot reach {host}: {text}"))
}

fn retryable(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
}

impl Client {
    pub fn new(settings: &S3Settings) -> Result<Self> {
        let bad = |m: &str| PynError::Storage(format!("invalid S3 settings: {m}"));
        let base = Url::parse(&settings.endpoint).map_err(|_| bad("endpoint is not a URL"))?;
        if !matches!(base.scheme(), "http" | "https") || base.host_str().is_none() {
            return Err(bad("endpoint must be an http or https URL with a host"));
        }
        if settings.bucket.is_empty() || settings.bucket.contains('/') {
            return Err(bad("bucket must be a name without slashes"));
        }
        if settings.access_key_id.is_empty() || settings.secret_access_key.is_empty() {
            return Err(bad("access key id and secret access key are required"));
        }
        if settings.max_attempts == 0
            || settings.part_size == 0
            || settings.multipart_threshold == 0
        {
            return Err(bad(
                "attempts, part size and multipart threshold must be positive",
            ));
        }
        let http = reqwest::Client::builder()
            .user_agent("pyn-s3")
            .connect_timeout(settings.connect_timeout)
            .read_timeout(settings.io_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| PynError::Storage(format!("http client: {e}")))?;
        let bucket_host = base.host_str().unwrap_or_default();
        let authority = match (settings.path_style, base.port()) {
            (true, Some(p)) => format!("{bucket_host}:{p}"),
            (true, None) => bucket_host.to_owned(),
            (false, Some(p)) => format!("{}.{bucket_host}:{p}", settings.bucket),
            (false, None) => format!("{}.{bucket_host}", settings.bucket),
        };
        Ok(Self {
            http,
            settings: settings.clone(),
            base,
            host: authority,
        })
    }

    pub fn endpoint_host(&self) -> &str {
        &self.host
    }

    fn path(&self, key: &str) -> String {
        if self.settings.path_style {
            format!("/{}/{key}", self.settings.bucket)
        } else {
            format!("/{key}")
        }
    }

    /// Sends the call, retrying connection errors and throttling or server errors when `retry` allows.
    /// Returns the final response whatever its status.
    pub async fn send(&self, call: &Call<'_>) -> Result<Response> {
        let attempts = if call.retry {
            self.settings.max_attempts
        } else {
            1
        };
        let mut attempt = 1;
        loop {
            let outcome = self.once(call).await;
            let again = attempt < attempts
                && match &outcome {
                    Ok(resp) => retryable(resp.status()),
                    Err(Failure::Transport(_)) => true,
                    Err(Failure::Local(_)) => false,
                };
            match outcome {
                Ok(resp) if !again => return Ok(resp),
                Err(Failure::Local(e)) => return Err(e),
                Err(Failure::Transport(e)) if !again => {
                    return Err(transport(call.op, &self.host, e));
                }
                _ => {}
            }
            tokio::time::sleep(self.backoff(attempt)).await;
            attempt += 1;
        }
    }

    /// Exponential backoff with equal jitter.
    fn backoff(&self, attempt: u32) -> Duration {
        let ceiling = self
            .settings
            .retry_base
            .saturating_mul(1u32 << (attempt - 1).min(16))
            .min(self.settings.retry_max);
        let mut r = [0u8; 8];
        let _ = getrandom::fill(&mut r);
        let jitter = u64::from_le_bytes(r) % (ceiling.as_millis() as u64 / 2 + 1);
        ceiling / 2 + Duration::from_millis(jitter)
    }

    async fn once(&self, call: &Call<'_>) -> std::result::Result<Response, Failure> {
        let path = self.path(call.key);
        let query: Vec<(&str, &str)> = call.query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let mut headers = vec![
            ("host", self.host.as_str()),
            ("x-amz-content-sha256", call.payload_sha256.as_str()),
            ("x-amz-date", amz_date.as_str()),
        ];
        if let Some(r) = &call.range {
            headers.push(("range", r.as_str()));
        }
        let sig = sigv4::sign(
            &self.settings.secret_access_key,
            &self.settings.region,
            &amz_date,
            &Request {
                method: call.method.as_str(),
                path: &path,
                query: &query,
                headers: &headers,
                payload_sha256: &call.payload_sha256,
            },
        );
        let auth = sigv4::authorization(
            &self.settings.access_key_id,
            &self.settings.region,
            &amz_date,
            &sig,
        );
        let canonical_query = sigv4::canonical_query(&query);
        let mut text = format!(
            "{}://{}{}",
            self.base.scheme(),
            self.host,
            sigv4::uri_encode(&path, true)
        );
        if !canonical_query.is_empty() {
            text.push('?');
            text.push_str(&canonical_query);
        }
        let url = Url::parse(&text)
            .map_err(|e| Failure::Local(PynError::Storage(format!("S3 {}: {e}", call.op))))?;
        let mut req = self
            .http
            .request(call.method.clone(), url)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", &call.payload_sha256)
            .header("authorization", auth);
        if let Some(r) = &call.range {
            req = req.header("range", r);
        }
        if call.bounded {
            req = req.timeout(self.settings.request_timeout);
        }
        req = match &call.body {
            Body::Empty => req,
            Body::Bytes(b) => req.body(b.clone()),
            Body::File { path, offset, len } => {
                let mut file = tokio::fs::File::open(path).await.map_err(local(call.op))?;
                file.seek(std::io::SeekFrom::Start(*offset))
                    .await
                    .map_err(local(call.op))?;
                let stream = tokio_util::io::ReaderStream::new(file.take(*len));
                req.header("content-length", *len)
                    .body(reqwest::Body::wrap_stream(stream))
            }
        };
        req.send().await.map_err(Failure::Transport)
    }

    /// The error for a response that was not the one expected: the operation, status and S3 error code.
    pub async fn refused(&self, op: &str, resp: Response) -> PynError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let code = xml::tag(&body, "Code").unwrap_or_default();
        let message = xml::tag(&body, "Message").unwrap_or_default();
        let mut text = format!("S3 {op} failed: HTTP {}", status.as_u16());
        if !code.is_empty() {
            text.push(' ');
            text.push_str(&code);
        }
        if !message.is_empty() {
            text.push_str(": ");
            text.push_str(&message);
        }
        PynError::Storage(text)
    }

    pub async fn head(&self, key: &str) -> Result<Option<u64>> {
        let resp = self
            .send(&Call::new("HeadObject", Method::HEAD, key))
            .await?;
        match resp.status() {
            s if s.is_success() => Ok(Some(
                resp.headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| {
                        PynError::Storage("S3 HeadObject failed: no Content-Length".into())
                    })?,
            )),
            StatusCode::NOT_FOUND => Ok(None),
            _ => Err(self.refused("HeadObject", resp).await),
        }
    }

    pub async fn put(&self, key: &str, body: Body, sha256: String) -> Result<()> {
        let mut call = Call::new("PutObject", Method::PUT, key);
        call.bounded =
            matches!(&body, Body::Empty) || matches!(&body, Body::Bytes(b) if b.len() <= 1 << 20);
        call.body = body;
        call.payload_sha256 = sha256;
        let resp = self.send(&call).await?;
        if resp.status().is_success() {
            return Ok(());
        }
        Err(self.refused("PutObject", resp).await)
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        let resp = self
            .send(&Call::new("DeleteObject", Method::DELETE, key))
            .await?;
        match resp.status() {
            s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(()),
            _ => Err(self.refused("DeleteObject", resp).await),
        }
    }

    pub async fn create_multipart(&self, key: &str) -> Result<String> {
        let mut call = Call::new("CreateMultipartUpload", Method::POST, key);
        call.query.push(("uploads", String::new()));
        call.retry = false;
        let resp = self.send(&call).await?;
        if !resp.status().is_success() {
            return Err(self.refused("CreateMultipartUpload", resp).await);
        }
        let body = resp.text().await.unwrap_or_default();
        xml::tag(&body, "UploadId")
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                PynError::Storage(
                    "S3 CreateMultipartUpload failed: no UploadId in the answer".into(),
                )
            })
    }

    pub async fn upload_part(
        &self,
        key: &str,
        upload_id: &str,
        number: u32,
        path: &Path,
        offset: u64,
        len: u64,
    ) -> Result<String> {
        let sha256 = hash_range(path, offset, len).await?;
        let mut call = Call::new("UploadPart", Method::PUT, key);
        call.query.push(("partNumber", number.to_string()));
        call.query.push(("uploadId", upload_id.to_owned()));
        call.body = Body::File {
            path: path.to_owned(),
            offset,
            len,
        };
        call.payload_sha256 = sha256;
        call.bounded = false;
        let resp = self.send(&call).await?;
        if !resp.status().is_success() {
            return Err(self.refused("UploadPart", resp).await);
        }
        resp.headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| PynError::Storage("S3 UploadPart failed: no ETag in the answer".into()))
    }

    pub async fn complete_multipart(
        &self,
        key: &str,
        upload_id: &str,
        etags: &[String],
    ) -> Result<()> {
        let mut body = String::from("<CompleteMultipartUpload>");
        for (i, etag) in etags.iter().enumerate() {
            body.push_str(&format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>",
                i + 1,
                xml::escape(etag)
            ));
        }
        body.push_str("</CompleteMultipartUpload>");
        let mut call = Call::new("CompleteMultipartUpload", Method::POST, key);
        call.query.push(("uploadId", upload_id.to_owned()));
        call.payload_sha256 = sigv4::sha256_hex(body.as_bytes());
        call.body = Body::Bytes(Bytes::from(body));
        call.retry = false;
        let resp = self.send(&call).await?;
        if !resp.status().is_success() {
            return Err(self.refused("CompleteMultipartUpload", resp).await);
        }
        // S3 can answer 200 and still carry an error document.
        let text = resp.text().await.unwrap_or_default();
        if text.contains("<Error>") {
            let code = xml::tag(&text, "Code").unwrap_or_default();
            return Err(PynError::Storage(format!(
                "S3 CompleteMultipartUpload failed: {code}"
            )));
        }
        Ok(())
    }

    pub async fn abort_multipart(&self, key: &str, upload_id: &str) {
        let mut call = Call::new("AbortMultipartUpload", Method::DELETE, key);
        call.query.push(("uploadId", upload_id.to_owned()));
        if let Err(e) = self.send(&call).await {
            tracing::warn!("{e}");
        }
    }
}

enum Failure {
    Transport(reqwest::Error),
    Local(PynError),
}

fn local(op: &'static str) -> impl Fn(std::io::Error) -> Failure {
    move |e| Failure::Local(PynError::Storage(format!("S3 {op}: spool file: {e}")))
}

/// SHA-256 of `len` bytes of a file from `offset`.
pub(crate) async fn hash_range(path: &Path, offset: u64, len: u64) -> Result<String> {
    use sha2::{Digest, Sha256};
    let io = |e: std::io::Error| PynError::Storage(format!("spool file: {e}"));
    let mut file = tokio::fs::File::open(path).await.map_err(io)?;
    file.seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = file.read(&mut buf[..want]).await.map_err(io)?;
        if n == 0 {
            return Err(PynError::Storage("spool file ended early".into()));
        }
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(endpoint: &str, path_style: bool) -> Client {
        let mut s = S3Settings::new(endpoint, "bkt", "AKID", "secret");
        s.path_style = path_style;
        Client::new(&s).unwrap()
    }

    #[test]
    fn virtual_host_style_puts_the_bucket_in_the_host_and_path_style_in_the_path() {
        let v = client("https://nyc3.digitaloceanspaces.com", false);
        assert_eq!(v.endpoint_host(), "bkt.nyc3.digitaloceanspaces.com");
        assert_eq!(v.path("ab/cd"), "/ab/cd");
        let p = client("http://127.0.0.1:9000", true);
        assert_eq!(p.endpoint_host(), "127.0.0.1:9000");
        assert_eq!(p.path("ab/cd"), "/bkt/ab/cd");
        let vp = client("http://minio.local:9000", false);
        assert_eq!(vp.endpoint_host(), "bkt.minio.local:9000");
    }

    #[test]
    fn unusable_settings_are_refused_up_front() {
        for (endpoint, bucket, key) in [
            ("not a url", "b", "k"),
            ("ftp://host", "b", "k"),
            ("https://host", "", "k"),
            ("https://host", "a/b", "k"),
            ("https://host", "b", ""),
        ] {
            let s = S3Settings::new(endpoint, bucket, key, "secret");
            assert!(
                matches!(Client::new(&s), Err(PynError::Storage(_))),
                "{endpoint} {bucket}"
            );
        }
    }
}

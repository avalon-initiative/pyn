//! `ObjectStore` on any S3-compatible service, over a hand-written SigV4 client.
//!
//! A put hashes the stream into memory (or a spool file past a threshold), checks it, then sends one PutObject at the
//! final content-addressed key. The store sits behind `ChunkedStore`, so it sees whole objects up to 1 MiB, chunks up
//! to 4 MiB and manifests.

mod client;
mod settings;
mod sigv4;
mod spool;
mod xml;

use std::ops::Range;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use pyn_core::object::clamp_range;
use pyn_core::{
    KeyHasher, Keying, ObjectId, ObjectRead, ObjectReader, ObjectStore, PynError, REPO_ALGORITHM,
    Result, Stored,
};
use reqwest::Method;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use client::{Body, Call, Client};
use spool::{Content, Spool};

pub use settings::{MAX_SINGLE_PUT, S3Settings};

/// Spool files older than this are left by a process that died mid-put.
pub const STALE_SPOOL_AGE: Duration = Duration::from_secs(3600);

const MAX_PARTS: u64 = 10_000;

/// Objects live at `<prefix><first two hex chars>/<rest>`, manifests at the same key plus `.cm`.
pub struct S3ObjectStore {
    client: Client,
    prefix: String,
    spool_dir: std::path::PathBuf,
    spool_threshold: usize,
    multipart_threshold: u64,
    part_size: u64,
}

fn storage(e: impl std::fmt::Display) -> PynError {
    PynError::Storage(e.to_string())
}

impl S3ObjectStore {
    /// Validates the settings, creates the spool directory and sweeps stale spool files. Makes no request.
    pub async fn open(settings: S3Settings) -> Result<Self> {
        let client = Client::new(&settings)?;
        tokio::fs::create_dir_all(&settings.spool_dir)
            .await
            .map_err(|e| storage(format!("spool directory: {e}")))?;
        let mut prefix = settings.prefix.trim_start_matches('/').to_owned();
        if !prefix.is_empty() && !prefix.ends_with('/') {
            prefix.push('/');
        }
        let store = Self {
            client,
            prefix,
            spool_dir: settings.spool_dir,
            spool_threshold: settings.spool_threshold,
            multipart_threshold: settings.multipart_threshold.min(MAX_SINGLE_PUT),
            part_size: settings.part_size,
        };
        store.sweep_stale_spool(STALE_SPOOL_AGE).await?;
        Ok(store)
    }

    /// Removes spool files last modified more than `age` ago; returns how many.
    pub async fn sweep_stale_spool(&self, age: Duration) -> Result<usize> {
        spool::sweep(&self.spool_dir, age).await
    }

    /// The host requests go to, for messages.
    pub fn endpoint_host(&self) -> &str {
        self.client.endpoint_host()
    }

    /// Only SHA-256 ids are stored.
    fn key(&self, hash: &ObjectId) -> Result<String> {
        let h = hash.to_hex();
        if hash.algorithm() != REPO_ALGORITHM {
            return Err(PynError::ObjectMissing(h));
        }
        Ok(format!("{}{}/{}", self.prefix, &h[..2], &h[2..]))
    }

    fn manifest_key(&self, hash: &ObjectId) -> Result<String> {
        Ok(format!("{}.cm", self.key(hash)?))
    }

    async fn upload(&self, key: &str, content: Content, len: u64, sha256: String) -> Result<()> {
        match content {
            Content::Memory(bytes) => self.client.put(key, Body::Bytes(bytes), sha256).await,
            Content::File(temp) if len <= self.multipart_threshold => {
                let body = Body::File {
                    path: temp.path().to_owned(),
                    offset: 0,
                    len,
                };
                self.client.put(key, body, sha256).await
            }
            Content::File(temp) => self.multipart(key, temp.path(), len).await,
        }
    }

    async fn multipart(&self, key: &str, path: &std::path::Path, len: u64) -> Result<()> {
        let part_size = self.part_size.max(len.div_ceil(MAX_PARTS - 1));
        let id = self.client.create_multipart(key).await?;
        let sent = async {
            let mut etags = Vec::new();
            let mut offset = 0;
            while offset < len {
                let n = part_size.min(len - offset);
                let number = etags.len() as u32 + 1;
                etags.push(
                    self.client
                        .upload_part(key, &id, number, path, offset, n)
                        .await?,
                );
                offset += n;
            }
            self.client.complete_multipart(key, &id, &etags).await
        }
        .await;
        if sent.is_err() {
            self.client.abort_multipart(key, &id).await;
        }
        sent
    }
}

fn reader_of(resp: reqwest::Response) -> ObjectReader {
    let stream = futures_util::stream::unfold(resp, |mut resp| async move {
        match resp.chunk().await {
            Ok(Some(bytes)) => Some((Ok::<Bytes, std::io::Error>(bytes), resp)),
            Ok(None) => None,
            Err(e) => Some((Err(std::io::Error::other(e.without_url())), resp)),
        }
    });
    Box::pin(tokio_util::io::StreamReader::new(Box::pin(stream)))
}

fn content_range_total(resp: &reqwest::Response) -> Option<u64> {
    let v = resp.headers().get("content-range")?.to_str().ok()?;
    v.rsplit('/').next()?.parse().ok()
}

fn empty_read(total: u64, range: Range<u64>) -> ObjectRead {
    ObjectRead {
        reader: Box::pin(std::io::Cursor::new(Vec::new())),
        total,
        range,
    }
}

#[async_trait]
impl ObjectStore for S3ObjectStore {
    async fn put_stream(
        &self,
        reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> Result<ObjectId> {
        Ok(self.put_stream_tracked(reader, keying, max).await?.id)
    }

    async fn put_stream_tracked(
        &self,
        mut reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> Result<Stored> {
        if let (Keying::Blob { len }, Some(limit)) = (keying, max)
            && len > limit
        {
            return Err(PynError::ObjectTooLarge { limit });
        }
        let mut hasher = KeyHasher::new(keying);
        let mut payload = Sha256::new();
        let mut spool = Spool::new(&self.spool_dir, self.spool_threshold);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf).await.map_err(storage)?;
            if n == 0 {
                break;
            }
            if let Some(limit) = max
                && spool.len() + n as u64 > limit
            {
                return Err(PynError::ObjectTooLarge { limit });
            }
            hasher.update(&buf[..n])?;
            payload.update(&buf[..n]);
            spool.write(&buf[..n]).await?;
        }
        let id = hasher.finish()?;
        let key = self.key(&id)?;
        if self.client.head(&key).await?.is_some() {
            return Ok(Stored {
                id,
                created: Vec::new(),
            });
        }
        let len = spool.len();
        let content = spool.finish().await?;
        self.upload(&key, content, len, hex::encode(payload.finalize()))
            .await?;
        Ok(Stored {
            id,
            created: vec![id],
        })
    }

    async fn read(&self, hash: &ObjectId, range: Option<Range<u64>>) -> Result<Option<ObjectRead>> {
        let key = self.key(hash)?;
        let wanted = range.clone().filter(|r| r.start < r.end);
        if range.is_some() && wanted.is_none() {
            let Some(total) = self.client.head(&key).await? else {
                return Ok(None);
            };
            let clamped = clamp_range(range, total);
            return Ok(Some(empty_read(total, clamped)));
        }
        let mut call = Call::new("GetObject", Method::GET, &key);
        call.bounded = false;
        call.range = wanted
            .as_ref()
            .map(|r| format!("bytes={}-{}", r.start, r.end - 1));
        let resp = self.client.send(&call).await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            let Some(total) = self.client.head(&key).await? else {
                return Ok(None);
            };
            let clamped = clamp_range(range, total);
            return Ok(Some(empty_read(total, clamped)));
        }
        if !status.is_success() {
            return Err(self.client.refused("GetObject", resp).await);
        }
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            let Some(total) = content_range_total(&resp) else {
                return Err(storage("S3 GetObject failed: no usable Content-Range"));
            };
            let range = clamp_range(range, total);
            return Ok(Some(ObjectRead {
                reader: reader_of(resp),
                total,
                range,
            }));
        }
        let Some(total) = resp.content_length() else {
            return Err(storage("S3 GetObject failed: no Content-Length"));
        };
        let range = clamp_range(range, total);
        let mut reader = reader_of(resp);
        if range.start > 0 {
            // The service ignored Range and sent the whole object.
            tokio::io::copy(&mut (&mut reader).take(range.start), &mut tokio::io::sink())
                .await
                .map_err(storage)?;
        }
        Ok(Some(ObjectRead {
            reader: Box::pin(reader.take(range.end - range.start)),
            total,
            range,
        }))
    }

    async fn exists(&self, hash: &ObjectId) -> Result<bool> {
        if self.client.head(&self.key(hash)?).await?.is_some() {
            return Ok(true);
        }
        Ok(self.client.head(&self.manifest_key(hash)?).await?.is_some())
    }

    async fn size(&self, hash: &ObjectId) -> Result<Option<u64>> {
        self.client.head(&self.key(hash)?).await
    }

    async fn delete(&self, hash: &ObjectId) -> Result<bool> {
        let mut removed = false;
        for key in [self.key(hash)?, self.manifest_key(hash)?] {
            if self.client.head(&key).await?.is_some() {
                self.client.delete(&key).await?;
                removed = true;
            }
        }
        Ok(removed)
    }

    async fn put_manifest(&self, key: &ObjectId, bytes: Vec<u8>) -> Result<()> {
        let sha256 = sigv4::sha256_hex(&bytes);
        self.client
            .put(
                &self.manifest_key(key)?,
                Body::Bytes(Bytes::from(bytes)),
                sha256,
            )
            .await
    }

    async fn get_manifest(&self, key: &ObjectId) -> Result<Option<Vec<u8>>> {
        let key = self.manifest_key(key)?;
        let mut call = Call::new("GetObject", Method::GET, &key);
        call.bounded = false;
        let resp = self.client.send(&call).await?;
        match resp.status() {
            s if s.is_success() => resp
                .bytes()
                .await
                .map(|b| Some(b.to_vec()))
                .map_err(|e| storage(format!("S3 GetObject failed: {}", e.without_url()))),
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            _ => Err(self.client.refused("GetObject", resp).await),
        }
    }
}

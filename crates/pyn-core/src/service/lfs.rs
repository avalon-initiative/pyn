//! Objects served through the Git LFS API: uploaded with a declared sha256 oid, recorded per repository, kept in the
//! object store under their blob id.

use std::ops::Range;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, ReadBuf};

use super::RepoService;
use crate::error::{PynError, Result};
use crate::object::{Keying, ObjectReader};
use crate::types::{LfsObject, UserId};

/// A valid LFS oid: 64 lowercase hex digits.
pub fn is_lfs_oid(oid: &str) -> bool {
    oid.len() == 64 && oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

type Mismatch = Arc<Mutex<Option<String>>>;

/// Hashes what passes through and fails the read at the end of a body of `size` bytes whose sha256 is not `oid`.
struct Verifying {
    inner: ObjectReader,
    hasher: Option<Sha256>,
    read: u64,
    size: u64,
    oid: String,
    mismatch: Mismatch,
}

impl AsyncRead for Verifying {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        std::task::ready!(self.inner.as_mut().poll_read(cx, buf))?;
        let fresh = &buf.filled()[before..];
        if !fresh.is_empty() {
            self.read += fresh.len() as u64;
            if let Some(h) = self.hasher.as_mut() {
                h.update(fresh);
            }
            return Poll::Ready(Ok(()));
        }
        if self.read == self.size
            && let Some(h) = self.hasher.take()
        {
            let actual = hex::encode(h.finalize());
            if actual != self.oid {
                *self.mismatch.lock().unwrap() = Some(actual);
                return Poll::Ready(Err(std::io::Error::other("content digest mismatch")));
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl RepoService {
    /// The LFS object recorded under `oid`, if any.
    pub async fn lfs_object(&self, oid: &str) -> Result<Option<LfsObject>> {
        self.meta.lfs_object(&self.repo, oid).await
    }

    /// Whether `size` more bytes may be uploaded: the owner is not frozen and neither the owner's nor the
    /// repository's storage cap would pass, counting the repository's LFS objects.
    pub async fn lfs_admit(&self, size: u64) -> Result<()> {
        self.ensure_writable().await?;
        let Some(cap) = self.storage_cap().await? else {
            return Ok(());
        };
        let held = self.meta.lfs_bytes(&self.repo).await?;
        if let Some(limit) = cap.repo_max_bytes {
            let used = self.meta.repo_usage(&self.repo).await?.stored_bytes + held;
            if used.saturating_add(size) > limit {
                return Err(PynError::RepoStorageLimitReached {
                    repository: cap.repository,
                    limit,
                    used,
                });
            }
        }
        if let Some(limit) = cap.max_bytes {
            let used = self.meta.owner_usage(&cap.owner).await?.stored_bytes + held;
            if used.saturating_add(size) > limit {
                return Err(PynError::StorageLimitReached {
                    owner: cap.owner.to_string(),
                    limit,
                    used,
                });
            }
        }
        Ok(())
    }

    /// Stores `size` bytes from `reader` as the object `oid`, which must be their sha256; a body that is shorter,
    /// longer or hashes otherwise is refused and not kept. An oid the repository has already is left alone (`false`).
    pub async fn lfs_upload(
        &self,
        by: &UserId,
        oid: &str,
        reader: ObjectReader,
        size: u64,
        max: Option<u64>,
    ) -> Result<(LfsObject, bool)> {
        if !is_lfs_oid(oid) {
            return Err(PynError::InvalidRequest(format!("{oid:?} is not an oid")));
        }
        if let Some(have) = self.lfs_object(oid).await? {
            self.ensure_writable().await?;
            if have.size != size {
                return Err(PynError::LengthMismatch {
                    declared: size,
                    actual: have.size,
                });
            }
            return Ok((have, false));
        }
        self.lfs_admit(size).await?;
        let mismatch = Mismatch::default();
        let reader = Verifying {
            inner: reader,
            hasher: Some(Sha256::new()),
            read: 0,
            size,
            oid: oid.to_string(),
            mismatch: mismatch.clone(),
        };
        let stored = self
            .upload(Box::pin(reader), Keying::Blob { len: size }, max, size)
            .await;
        let object = match stored {
            Ok(id) => id,
            Err(e) => {
                let actual = mismatch.lock().unwrap().take();
                return Err(match actual {
                    Some(actual) => PynError::DigestMismatch {
                        expected: oid.to_string(),
                        actual,
                    },
                    None => e,
                });
            }
        };
        let record = LfsObject {
            oid: oid.to_string(),
            size,
            object,
            created_at: self.clock.now(),
            created_by: by.clone(),
        };
        let fresh = self.meta.record_lfs_object(&self.repo, &record).await?;
        Ok((record, fresh))
    }

    /// The object `oid` or the byte `range` of it; `ObjectNotFound` when the repository has no such LFS object.
    pub async fn lfs_read(
        &self,
        oid: &str,
        range: Option<Range<u64>>,
    ) -> Result<(LfsObject, crate::ObjectRead)> {
        let object = self
            .lfs_object(oid)
            .await?
            .ok_or_else(|| PynError::ObjectNotFound(oid.to_string()))?;
        let read = self
            .objects
            .read(&object.object, range)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(oid.to_string()))?;
        Ok((object, read))
    }
}

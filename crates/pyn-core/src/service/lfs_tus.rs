//! Resumable LFS uploads for the tus transfer adapter: each append is kept as a raw chunk in the object store and the
//! sessions are recorded in the metadata store. A finished upload is joined and verified by `lfs_upload`.

use std::pin::Pin;
use std::task::{Context, Poll};

use chrono::{DateTime, Duration, SubsecRound, Utc};
use tokio::io::{AsyncRead, ReadBuf};

use super::RepoService;
use super::uploads::remove_unrecorded;
use crate::chunking::ChainReader;
use crate::error::{PynError, Result};
use crate::object::{Keying, ObjectReader, ObjectStore};
use crate::store::MetadataStore;
use crate::types::{LfsObject, TusCancel, TusClaim, TusSession, UserId};
use std::sync::Arc;

/// How long an upload may sit untouched before it is dropped.
pub const TUS_SESSION_TTL_HOURS: i64 = 24;
/// How long one append may hold its upload before another request can take over.
pub const TUS_APPEND_LEASE_MINUTES: i64 = 60;
/// Open uploads one user may hold at a time.
pub const TUS_MAX_SESSIONS_PER_USER: usize = 32;

/// What the client needs to resume: where the upload stands and when it lapses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TusStatus {
    pub offset: u64,
    pub size: u64,
    pub expires: DateTime<Utc>,
}

/// The outcome of one append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TusAppended {
    pub status: TusStatus,
    /// The object, once the last byte arrived and it verified.
    pub completed: Option<LfsObject>,
}

#[derive(Debug, thiserror::Error)]
pub enum TusError {
    #[error("no such upload")]
    NotFound,
    #[error("the upload is at offset {expected}")]
    OffsetMismatch { expected: u64 },
    #[error("another request is appending to this upload")]
    Busy,
    #[error("the body runs past the declared upload length")]
    TooLong,
    #[error(transparent)]
    Pyn(#[from] PynError),
}

impl TusStatus {
    fn of(s: &TusSession) -> Self {
        Self {
            offset: s.offset,
            size: s.size,
            expires: s.expires,
        }
    }
}

/// Ends a body that fails as if it were short, so what arrived is kept.
struct Lenient(ObjectReader);

impl AsyncRead for Lenient {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.0.as_mut().poll_read(cx, buf) {
            Poll::Ready(Err(_)) => Poll::Ready(Ok(())),
            other => other,
        }
    }
}

/// Drops every upload that lapsed, in any repository, and removes the pieces it stored. Returns how many.
pub async fn sweep_tus_sessions(
    meta: &Arc<dyn MetadataStore>,
    objects: &Arc<dyn ObjectStore>,
    now: DateTime<Utc>,
) -> Result<usize> {
    let lapsed = meta.tus_take_expired(now).await?;
    let n = lapsed.len();
    for s in lapsed {
        remove_unrecorded(meta, objects, s.created).await;
    }
    Ok(n)
}

impl RepoService {
    /// Where the upload of `oid` stands, if one is open.
    pub async fn lfs_tus_status(&self, oid: &str) -> Result<Option<TusStatus>> {
        let live = self
            .meta
            .tus_session(&self.repo, oid)
            .await?
            .filter(|s| s.expires > self.clock.now());
        Ok(live.as_ref().map(TusStatus::of))
    }

    /// Opens an upload of `size` bytes for `oid`, or returns the one already open for the same size.
    pub async fn lfs_tus_create(
        &self,
        by: &UserId,
        oid: &str,
        size: u64,
        max: u64,
    ) -> Result<TusStatus> {
        if !super::lfs::is_lfs_oid(oid) {
            return Err(PynError::InvalidRequest(format!("{oid:?} is not an oid")));
        }
        if size > max {
            return Err(PynError::ObjectTooLarge { limit: max });
        }
        sweep_tus_sessions(&self.meta, &self.objects, self.clock.now()).await?;
        if let Some(have) = self.meta.tus_session(&self.repo, oid).await? {
            if have.size != size {
                return Err(PynError::LengthMismatch {
                    declared: size,
                    actual: have.size,
                });
            }
            return Ok(TusStatus::of(&have));
        }
        if let Some(have) = self.lfs_object(oid).await? {
            self.ensure_writable().await?;
            if have.size != size {
                return Err(PynError::LengthMismatch {
                    declared: size,
                    actual: have.size,
                });
            }
        } else {
            self.lfs_admit(size).await?;
        }
        let session = TusSession {
            repo: self.repo.clone(),
            oid: oid.to_string(),
            owner: by.clone(),
            size,
            offset: 0,
            parts: Vec::new(),
            created: Vec::new(),
            expires: self.clock.now() + Duration::hours(TUS_SESSION_TTL_HOURS),
            busy_until: None,
        };
        let opened = self
            .meta
            .tus_open(&session, TUS_MAX_SESSIONS_PER_USER)
            .await?;
        if opened.size != size {
            return Err(PynError::LengthMismatch {
                declared: size,
                actual: opened.size,
            });
        }
        Ok(TusStatus::of(&opened))
    }

    /// Abandons the upload and removes what it stored.
    pub async fn lfs_tus_cancel(
        &self,
        by: &UserId,
        oid: &str,
    ) -> std::result::Result<(), TusError> {
        match self
            .meta
            .tus_cancel(&self.repo, oid, by, self.clock.now())
            .await?
        {
            TusCancel::Removed(s) => {
                remove_unrecorded(&self.meta, &self.objects, s.created).await;
                Ok(())
            }
            TusCancel::Busy => Err(TusError::Busy),
            TusCancel::Missing => Err(TusError::NotFound),
        }
    }

    /// Appends up to `len` bytes of `reader` at `offset`. A body that ends early (a dropped connection) keeps what
    /// arrived; the last byte completes the upload, which `lfs_upload` verifies against `oid`.
    pub async fn lfs_tus_append(
        &self,
        by: &UserId,
        oid: &str,
        offset: u64,
        reader: ObjectReader,
        len: u64,
        max: u64,
    ) -> std::result::Result<TusAppended, TusError> {
        let now = self.clock.now();
        let lease = (now + Duration::minutes(TUS_APPEND_LEASE_MINUTES)).trunc_subsecs(6);
        match self
            .meta
            .tus_claim(&self.repo, oid, by, offset..offset + len, now, lease)
            .await?
        {
            TusClaim::Claimed(_) => {}
            TusClaim::Missing => return Err(TusError::NotFound),
            TusClaim::Busy => return Err(TusError::Busy),
            TusClaim::Offset { expected } => return Err(TusError::OffsetMismatch { expected }),
            TusClaim::TooLong => return Err(TusError::TooLong),
        }
        let stored = self
            .objects
            .put_stream_tracked(Box::pin(Lenient(reader)), Keying::Raw, Some(len))
            .await;
        let got = match &stored {
            Ok(st) => self.stored_size(&st.id).await,
            Err(_) => Ok(0),
        };
        let expires = self.clock.now() + Duration::hours(TUS_SESSION_TTL_HOURS);
        let (st, got) = match (stored, got) {
            (Ok(st), Ok(got)) => (st, got),
            (Err(e), _) | (_, Err(e)) => {
                let _ = self
                    .meta
                    .tus_commit(&self.repo, oid, lease, None, &[], expires)
                    .await;
                return Err(e.into());
            }
        };
        let part = (got > 0).then_some((st.id, got));
        let committed = self
            .meta
            .tus_commit(&self.repo, oid, lease, part, &st.created, expires)
            .await?;
        let Some(s) = committed else {
            remove_unrecorded(&self.meta, &self.objects, st.created).await;
            return Err(TusError::NotFound);
        };
        if s.offset != s.size {
            return Ok(TusAppended {
                status: TusStatus::of(&s),
                completed: None,
            });
        }
        let status = TusStatus::of(&s);
        let pieces = s.parts.iter().map(|(id, n)| (*id, 0..*n)).collect();
        let joined = ChainReader::new(self.objects.clone(), pieces);
        let result = self
            .lfs_upload(by, oid, Box::pin(joined), s.size, Some(max))
            .await;
        let mut leftovers = s.created;
        if let Ok((object, _)) = &result {
            // Content that chunks to one piece stores that piece under the same key as a part.
            let kept = self
                .objects
                .chunks_of(&object.object)
                .await
                .unwrap_or_default();
            leftovers.retain(|id| !kept.contains(id));
        }
        remove_unrecorded(&self.meta, &self.objects, leftovers).await;
        let (object, _) = result?;
        Ok(TusAppended {
            status,
            completed: Some(object),
        })
    }
}

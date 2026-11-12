//! Resumable LFS uploads for the tus transfer adapter: each append is kept as a raw chunk in the object store and the
//! sessions are tracked in memory. A finished upload is joined and verified by `lfs_upload`.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use chrono::{DateTime, Duration, Utc};
use tokio::io::{AsyncRead, ReadBuf};

use super::RepoService;
use super::uploads::remove_unrecorded;
use crate::ObjectId;
use crate::chunking::ChainReader;
use crate::error::{PynError, Result};
use crate::object::{Keying, ObjectReader};
use crate::types::{LfsObject, RepoId, UserId};

/// How long an upload may sit untouched before it is dropped.
pub const TUS_SESSION_TTL_HOURS: i64 = 24;
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

struct Session {
    owner: UserId,
    size: u64,
    offset: u64,
    parts: Vec<(ObjectId, u64)>,
    created: Vec<ObjectId>,
    expires: DateTime<Utc>,
    busy: bool,
}

impl Session {
    fn status(&self) -> TusStatus {
        TusStatus {
            offset: self.offset,
            size: self.size,
            expires: self.expires,
        }
    }
}

type Key = (RepoId, String);

/// The open resumable uploads of every repository; shared by the services `Repositories` builds.
#[derive(Default)]
pub struct TusSessions {
    open: Mutex<HashMap<Key, Session>>,
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

impl RepoService {
    fn tus_key(&self, oid: &str) -> Key {
        (self.repo.clone(), oid.to_string())
    }

    /// Drops every upload that lapsed, in any repository, and removes the pieces it stored.
    pub async fn lfs_tus_expire(&self) {
        let now = self.clock.now();
        let mut lapsed = Vec::new();
        {
            let mut open = self.tus.open.lock().unwrap();
            open.retain(|_, s| {
                let keep = s.busy || s.expires > now;
                if !keep {
                    lapsed.extend(std::mem::take(&mut s.created));
                }
                keep
            });
        }
        remove_unrecorded(&self.meta, &self.objects, lapsed).await;
    }

    /// Where the upload of `oid` stands, if one is open.
    pub async fn lfs_tus_status(&self, oid: &str) -> Option<TusStatus> {
        self.lfs_tus_expire().await;
        let open = self.tus.open.lock().unwrap();
        open.get(&self.tus_key(oid)).map(Session::status)
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
        self.lfs_tus_expire().await;
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
        let mut open = self.tus.open.lock().unwrap();
        let key = self.tus_key(oid);
        if let Some(have) = open.get(&key) {
            if have.size != size {
                return Err(PynError::LengthMismatch {
                    declared: size,
                    actual: have.size,
                });
            }
            return Ok(have.status());
        }
        if open.values().filter(|s| &s.owner == by).count() >= TUS_MAX_SESSIONS_PER_USER {
            return Err(PynError::UploadsBusy {
                scope: "resumable uploads",
            });
        }
        let session = Session {
            owner: by.clone(),
            size,
            offset: 0,
            parts: Vec::new(),
            created: Vec::new(),
            expires: self.clock.now() + Duration::hours(TUS_SESSION_TTL_HOURS),
            busy: false,
        };
        let status = session.status();
        open.insert(key, session);
        Ok(status)
    }

    /// Abandons the upload and removes what it stored.
    pub async fn lfs_tus_cancel(
        &self,
        by: &UserId,
        oid: &str,
    ) -> std::result::Result<(), TusError> {
        let created = {
            let mut open = self.tus.open.lock().unwrap();
            let key = self.tus_key(oid);
            match open.get(&key) {
                Some(s) if &s.owner == by && !s.busy => {}
                Some(s) if &s.owner == by => return Err(TusError::Busy),
                _ => return Err(TusError::NotFound),
            }
            open.remove(&key).map(|s| s.created).unwrap_or_default()
        };
        remove_unrecorded(&self.meta, &self.objects, created).await;
        Ok(())
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
        {
            let mut open = self.tus.open.lock().unwrap();
            let s = match open.get_mut(&self.tus_key(oid)) {
                Some(s) if &s.owner == by && s.expires > self.clock.now() => s,
                _ => return Err(TusError::NotFound),
            };
            if s.busy {
                return Err(TusError::Busy);
            }
            if offset != s.offset {
                return Err(TusError::OffsetMismatch { expected: s.offset });
            }
            if len > s.size - s.offset {
                return Err(TusError::TooLong);
            }
            s.busy = true;
        }
        let stored = self
            .objects
            .put_stream_tracked(Box::pin(Lenient(reader)), Keying::Raw, Some(len))
            .await;
        let got = match &stored {
            Ok(st) => self.stored_size(&st.id).await,
            Err(_) => Ok(0),
        };
        let finished = {
            let mut open = self.tus.open.lock().unwrap();
            let key = self.tus_key(oid);
            let Some(s) = open.get_mut(&key) else {
                return Err(TusError::NotFound);
            };
            s.busy = false;
            let (st, got) = match (stored, got) {
                (Ok(st), Ok(got)) => (st, got),
                (Err(e), _) | (_, Err(e)) => return Err(e.into()),
            };
            if got > 0 {
                s.parts.push((st.id, got));
                s.offset += got;
            }
            s.created.extend(st.created);
            s.expires = self.clock.now() + Duration::hours(TUS_SESSION_TTL_HOURS);
            if s.offset == s.size {
                open.remove(&key)
            } else {
                return Ok(TusAppended {
                    status: s.status(),
                    completed: None,
                });
            }
        };
        let Some(s) = finished else {
            return Err(TusError::NotFound);
        };
        let status = s.status();
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

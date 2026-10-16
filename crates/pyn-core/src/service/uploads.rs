//! Uploaded objects are recorded per repository: only a repository that uploaded an object, or one of whose
//! revisions names it, may reference it. The object store itself stays shared and deduplicated.

use super::RepoService;
use crate::error::{PynError, Result};
use crate::object::{Keying, ObjectReader};
use crate::store::{MetadataStore, UploadReservation};
use crate::types::RepoId;
use crate::object::ObjectStore;
use pyn_object::ObjectId;
use std::sync::Arc;

/// How long a reservation holds budget if its upload never finishes or releases it.
const UPLOAD_HOLD_SECS: i64 = 6 * 3600;

/// Default ceiling on the bytes a repository holds in uploads that no revision names yet.
pub const DEFAULT_UNREFERENCED_UPLOAD_BYTES: u64 = 32 << 30;

impl RepoService {
    /// Overrides the ceiling on unreferenced uploads (default `DEFAULT_UNREFERENCED_UPLOAD_BYTES`).
    pub fn with_unreferenced_cap(mut self, bytes: u64) -> Self {
        self.unreferenced_cap = bytes;
        self
    }

    /// Reserves `expect` bytes of the unreferenced budget, stores what `reader` yields and records it. The bytes this
    /// call newly wrote are removed again when recording fails.
    pub async fn upload(
        &self,
        reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
        expect: u64,
    ) -> Result<ObjectId> {
        let hold = self
            .meta
            .reserve_upload(
                &self.repo,
                expect,
                self.unreferenced_cap,
                self.clock.now(),
                UPLOAD_HOLD_SECS,
            )
            .await?;
        let guard = ReservationGuard::new(self.meta.clone(), self.repo.clone(), hold);
        let stored = self
            .objects
            .put_stream_tracked(reader, keying, max)
            .await;
        let stored = match stored {
            Ok(stored) => stored,
            Err(e) => {
                guard.release().await;
                return Err(e);
            }
        };
        let mut rollback = Rollback::new(self.objects.clone(), stored.created.clone());
        let recorded = self.record_stored(&stored.id, expect).await;
        match recorded {
            Ok(()) => rollback.keep(),
            Err(_) => rollback.run().await,
        }
        guard.release().await;
        recorded.map(|()| stored.id)
    }

    async fn record_stored(&self, id: &ObjectId, expect: u64) -> Result<()> {
        let size = self.stored_size(id).await?;
        if size > expect {
            return Err(PynError::LengthMismatch {
                declared: expect,
                actual: size,
            });
        }
        self.meta
            .record_upload(&self.repo, id, size, self.unreferenced_cap)
            .await
    }

    /// Joins chunks this repository knows into one object and records it; an unknown chunk is `ObjectMissing`.
    pub async fn assemble(&self, chunks: &[ObjectId], max: Option<u64>) -> Result<ObjectId> {
        if let Some(c) = self.meta.unknown_objects(&self.repo, chunks).await?.first() {
            return Err(PynError::ObjectMissing(c.to_string()));
        }
        let id = self.objects.assemble(chunks, max).await?;
        let size = self.stored_size(&id).await?;
        self.meta
            .record_assembled(&self.repo, &id, size, chunks, self.unreferenced_cap)
            .await?;
        Ok(id)
    }

    /// Stores `bytes` as a blob and records it for this repository.
    pub async fn upload_blob(&self, bytes: Vec<u8>) -> Result<ObjectId> {
        let len = bytes.len() as u64;
        self.upload(
            Box::pin(std::io::Cursor::new(bytes)),
            Keying::Blob { len },
            None,
            len,
        )
        .await
    }

    /// The ids among `ids` this repository cannot reference yet, so the client uploads them.
    pub async fn missing_objects(&self, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
        self.meta.unknown_objects(&self.repo, ids).await
    }

    /// The stored length of `id`, if this repository may reference it and the store holds it.
    pub(super) async fn referenced_size(&self, id: &ObjectId) -> Result<u64> {
        if !self
            .meta
            .unknown_objects(&self.repo, &[*id])
            .await?
            .is_empty()
        {
            return Err(PynError::ObjectMissing(id.to_string()));
        }
        self.stored_size(id).await
    }

    async fn stored_size(&self, id: &ObjectId) -> Result<u64> {
        self.objects
            .size(id)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(id.to_string()))
    }
}

/// Releases an upload's reservation when dropped, including when the upload future is cancelled.
struct ReservationGuard {
    meta: Arc<dyn MetadataStore>,
    repo: RepoId,
    hold: Option<UploadReservation>,
}

impl ReservationGuard {
    fn new(meta: Arc<dyn MetadataStore>, repo: RepoId, hold: UploadReservation) -> Self {
        Self {
            meta,
            repo,
            hold: Some(hold),
        }
    }

    async fn release(mut self) {
        if let Some(hold) = self.hold.take() {
            let _ = self.meta.release_upload(&self.repo, hold).await;
        }
    }
}

impl Drop for ReservationGuard {
    fn drop(&mut self) {
        let Some(hold) = self.hold.take() else {
            return;
        };
        let (meta, repo) = (self.meta.clone(), self.repo.clone());
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                let _ = meta.release_upload(&repo, hold).await;
            });
        }
    }
}

/// Removes objects an upload newly created unless kept; also runs when the upload is cancelled.
struct Rollback {
    objects: Arc<dyn ObjectStore>,
    created: Vec<ObjectId>,
}

impl Rollback {
    fn new(objects: Arc<dyn ObjectStore>, created: Vec<ObjectId>) -> Self {
        Self { objects, created }
    }

    fn keep(&mut self) {
        self.created.clear();
    }

    async fn run(&mut self) {
        for id in std::mem::take(&mut self.created) {
            let _ = self.objects.delete(&id).await;
        }
    }
}

impl Drop for Rollback {
    fn drop(&mut self) {
        let (objects, created) = (self.objects.clone(), std::mem::take(&mut self.created));
        if created.is_empty() {
            return;
        }
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                for id in created {
                    let _ = objects.delete(&id).await;
                }
            });
        }
    }
}

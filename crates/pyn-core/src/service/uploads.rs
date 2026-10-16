//! Uploaded objects are recorded per repository: only a repository that uploaded an object, or one of whose
//! revisions names it, may reference it. The object store itself stays shared and deduplicated.

use super::RepoService;
use crate::error::{PynError, Result};
use crate::object::{Keying, ObjectReader};
use pyn_object::ObjectId;

/// Default ceiling on the bytes a repository holds in uploads that no revision names yet.
pub const DEFAULT_UNREFERENCED_UPLOAD_BYTES: u64 = 32 << 30;

impl RepoService {
    /// Overrides the ceiling on unreferenced uploads (default `DEFAULT_UNREFERENCED_UPLOAD_BYTES`).
    pub fn with_unreferenced_cap(mut self, bytes: u64) -> Self {
        self.unreferenced_cap = bytes;
        self
    }

    /// Stores what `reader` yields and records it for this repository; `expect` is the most it can add.
    pub async fn upload(
        &self,
        reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
        expect: u64,
    ) -> Result<ObjectId> {
        let used = self.meta.unreferenced_bytes(&self.repo).await?;
        if used.saturating_add(expect) > self.unreferenced_cap {
            return Err(self.upload_limit(used));
        }
        let id = self.objects.put_stream(reader, keying, max).await?;
        let size = self.stored_size(&id).await?;
        self.meta
            .record_upload(&self.repo, &id, size, self.unreferenced_cap)
            .await?;
        Ok(id)
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

    fn upload_limit(&self, used: u64) -> PynError {
        PynError::StorageLimitReached {
            owner: self.repo.to_string(),
            limit: self.unreferenced_cap,
            used,
        }
    }
}

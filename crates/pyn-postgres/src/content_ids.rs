//! One-time re-keying of revision content from SHA-256 content hashes to git blob ids (decision #245).

use pyn_core::{Keying, ObjectId, ObjectStore, PynError, REPO_ALGORITHM, Result};
use sqlx::Row;

use crate::{PgMetadataStore, db};

/// What `migrate_content_ids` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ContentIdMigration {
    pub migrated: usize,
    /// Contents whose object is not in the store; their revisions keep the old id.
    pub missing: Vec<String>,
}

impl PgMetadataStore {
    /// Re-stores every object that a not yet migrated revision names under its blob id and points the revisions at it.
    /// The old keys stay until garbage collection; the revisions' v1 hashes keep covering the old content hash.
    pub async fn migrate_content_ids(
        &self,
        objects: &dyn ObjectStore,
    ) -> Result<ContentIdMigration> {
        let olds: Vec<String> = sqlx::query(
            "SELECT DISTINCT content FROM revisions WHERE content_v1 = content ORDER BY content",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?
        .iter()
        .map(|r| r.get("content"))
        .collect();

        let mut report = ContentIdMigration::default();
        for old_hex in olds {
            let old = ObjectId::from_hex(REPO_ALGORITHM, &old_hex)
                .map_err(|e| PynError::Storage(format!("revision content id: {e}")))?;
            let Some(found) = objects.read(&old, None).await? else {
                report.missing.push(old_hex);
                continue;
            };
            let new = objects
                .put_stream(found.reader, Keying::Blob { len: found.total }, None)
                .await?;
            sqlx::query("UPDATE revisions SET content = $2 WHERE content = $1 AND content_v1 = $1")
                .bind(&old_hex)
                .bind(new.to_hex())
                .execute(&self.pool)
                .await
                .map_err(db)?;
            report.migrated += 1;
        }
        Ok(report)
    }
}

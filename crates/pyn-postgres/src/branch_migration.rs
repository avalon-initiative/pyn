//! One-time migration of repositories that predate branches: a default branch at the newest commit and the pointers.

use pyn_core::Result;

use crate::{PgMetadataStore, db};

// Arbitrary constant: concurrent starts take turns instead of migrating the same rows twice.
const LOCK_KEY: i64 = 0x0070_796e_6272_6e63;

/// What `migrate_branches` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BranchMigration {
    pub branches: u64,
    pub pointers: u64,
}

impl PgMetadataStore {
    /// Gives every repository its default branch, at the newest commit of the migrated mainline, and fills the
    /// per-commit pointers from the revisions' commits. Run it after `migrate_history`. Idempotent.
    pub async fn migrate_branches(&self) -> Result<BranchMigration> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let branches = sqlx::query(
            "INSERT INTO branches (repo, name, head, is_default, created_at, created_by)
             SELECT r.repo, $1,
                    (SELECT c.id FROM commits c WHERE c.repo = r.repo ORDER BY c.seq DESC LIMIT 1),
                    true,
                    COALESCE((SELECT p.created_at FROM repositories p WHERE p.id = r.repo), now()),
                    COALESCE((SELECT p.owner FROM repositories p WHERE p.id = r.repo), '')
             FROM (SELECT id AS repo FROM repositories UNION SELECT repo FROM commits) r
             ON CONFLICT DO NOTHING",
        )
        .bind(pyn_core::DEFAULT_BRANCH)
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected();
        sqlx::query(
            "UPDATE branches b SET head = (SELECT c.id FROM commits c WHERE c.repo = b.repo ORDER BY c.seq DESC LIMIT 1)
             WHERE b.is_default AND b.head IS NULL
               AND EXISTS (SELECT 1 FROM commits c WHERE c.repo = b.repo)",
        )
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let pointers = sqlx::query(
            "INSERT INTO commit_pointers (repo, commit_id, path, revision)
             SELECT r.repo, r.commit_id, r.path, r.id FROM revisions r
             WHERE r.commit_id IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM commit_pointers p WHERE p.repo = r.repo AND p.commit_id = r.commit_id)",
        )
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected();
        if pointers > 0 {
            // Counts made before the pointers existed describe empty snapshots.
            sqlx::query(
                "UPDATE commits SET file_count = NULL, exclusive_count = NULL, newest_at = NULL",
            )
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        crate::counts::backfill(&self.pool).await?;
        Ok(BranchMigration { branches, pointers })
    }
}

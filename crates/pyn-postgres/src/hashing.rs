//! One-time hashing of revisions and audit events that predate the hash columns.

use pyn_core::{
    AuditAction, AuditEvent, AuditScope, RepoPath, Result, Revision, RevisionHash, UserId,
};
use sqlx::{PgPool, Row};
use std::str::FromStr;

use crate::{REVISION_COLUMNS, db, revision_from};

// Arbitrary constant: concurrent starts take turns instead of hashing the same rows twice.
const LOCK_KEY: i64 = 0x0070_796e_6861_7368;

/// Hashes every row still lacking a hash, in chain order. Hashed rows are left alone.
pub(crate) async fn backfill(pool: &PgPool) -> Result<()> {
    let mut tx = pool.begin().await.map_err(db)?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *tx)
        .await
        .map_err(db)?;

    let paths = sqlx::query(
        "SELECT DISTINCT repo, path FROM revisions WHERE hash IS NULL ORDER BY repo, path",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(db)?;
    for p in &paths {
        let (repo, path): (String, String) = (p.get("repo"), p.get("path"));
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS} FROM revisions WHERE repo = $1 AND path = $2 ORDER BY id"
        )))
        .bind(&repo)
        .bind(&path)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let mut previous: Option<RevisionHash> = None;
        for row in &rows {
            let mut rev: Revision = revision_from(row)?;
            if row.get::<Option<String>, _>("hash").is_none() {
                rev.previous = previous.clone();
                rev.hash = rev.expected_hash();
                sqlx::query(
                    "UPDATE revisions SET hash = $4, previous_hash = $5 WHERE repo = $1 AND path = $2 AND id = $3",
                )
                .bind(&repo)
                .bind(&path)
                .bind(rev.id.0 as i64)
                .bind(rev.hash.as_str())
                .bind(rev.previous.as_ref().map(RevisionHash::as_str))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
            previous = Some(rev.hash);
        }
    }

    let scopes = sqlx::query("SELECT DISTINCT repo FROM audit_events WHERE hash IS NULL")
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
    for s in &scopes {
        let key: String = s.get("repo");
        let scope = AuditScope::from_key(&key);
        let rows = sqlx::query(
            "SELECT id, at, actor, action, path, detail, hash FROM audit_events WHERE repo = $1 ORDER BY id",
        )
        .bind(&key)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let mut previous: Option<String> = None;
        for r in &rows {
            let stored: Option<String> = r.get("hash");
            let hash = match stored {
                Some(h) => h,
                None => {
                    let event = AuditEvent {
                        id: r.get("id"),
                        at: r.get("at"),
                        actor: UserId::new(r.get::<String, _>("actor")),
                        action: AuditAction::from_str(r.get("action"))?,
                        path: r
                            .get::<Option<String>, _>("path")
                            .map(RepoPath::new)
                            .transpose()?,
                        detail: r.get("detail"),
                        previous: previous.clone(),
                        hash: String::new(),
                    };
                    let hash = event.expected_hash(&scope);
                    sqlx::query(
                        "UPDATE audit_events SET hash = $2, previous_hash = $3 WHERE id = $1",
                    )
                    .bind(event.id)
                    .bind(&hash)
                    .bind(&previous)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                    hash
                }
            };
            previous = Some(hash);
        }
    }
    tx.commit().await.map_err(db)
}

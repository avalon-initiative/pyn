use std::str::FromStr;

use async_trait::async_trait;
use pyn_core::{
    AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent, RepoPath, Result,
    UserId,
};
use sqlx::Row;

use crate::{PgMetadataStore, db};

#[async_trait]
impl AuditStore for PgMetadataStore {
    async fn record(&self, scope: &AuditScope, event: NewAuditEvent) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        // Serializes appends to one log so each event chains to the one before it.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(scope.key())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let previous: Option<String> = sqlx::query_scalar::<_, Option<String>>(
            "SELECT hash FROM audit_events WHERE repo = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(scope.key())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .flatten();
        let hash = event.hash(scope, previous.as_deref());
        sqlx::query(
            "INSERT INTO audit_events (repo, at, actor, action, path, detail, hash, previous_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(scope.key())
        .bind(event.at)
        .bind(event.actor.as_str())
        .bind(event.action.as_str())
        .bind(event.path.as_ref().map(RepoPath::as_str))
        .bind(&event.detail)
        .bind(&hash)
        .bind(&previous)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    async fn list(&self, scope: &AuditScope, query: &AuditQuery) -> Result<Vec<AuditEvent>> {
        let rows = sqlx::query(
            "SELECT id, at, actor, action, path, detail, hash, previous_hash FROM audit_events
             WHERE repo = $1
               AND ($2::text IS NULL OR path = $2)
               AND ($3::text IS NULL OR actor = $3)
               AND ($4::text IS NULL OR action = $4)
               AND ($5::bigint IS NULL OR id < $5)
             ORDER BY id DESC LIMIT $6",
        )
        .bind(scope.key())
        .bind(query.path.as_ref().map(RepoPath::as_str))
        .bind(query.actor.as_ref().map(UserId::as_str))
        .bind(query.action.map(AuditAction::as_str))
        .bind(query.before)
        .bind(query.limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|r| {
                Ok(AuditEvent {
                    id: r.get("id"),
                    at: r.get("at"),
                    actor: UserId::new(r.get::<String, _>("actor")),
                    action: AuditAction::from_str(r.get("action"))?,
                    path: r
                        .get::<Option<String>, _>("path")
                        .map(RepoPath::from_stored)
                        .transpose()?,
                    detail: r.get("detail"),
                    hash: r.get::<Option<String>, _>("hash").unwrap_or_default(),
                    previous: r.get("previous_hash"),
                })
            })
            .collect()
    }
}

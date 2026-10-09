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
        sqlx::query(
            "INSERT INTO audit_events (repo, at, actor, action, path, detail) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(scope.key())
        .bind(event.at)
        .bind(event.actor.as_str())
        .bind(event.action.as_str())
        .bind(event.path.as_ref().map(RepoPath::as_str))
        .bind(&event.detail)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn list(&self, scope: &AuditScope, query: &AuditQuery) -> Result<Vec<AuditEvent>> {
        let rows = sqlx::query(
            "SELECT id, at, actor, action, path, detail FROM audit_events
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
                        .map(RepoPath::new)
                        .transpose()?,
                    detail: r.get("detail"),
                })
            })
            .collect()
    }
}

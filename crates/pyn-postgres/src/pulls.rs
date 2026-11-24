//! Pull requests in PostgreSQL.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pyn_core::pull::{
    NewPull, PullEdit, PullEvent, PullEventKind, PullFilter, PullRequest, PullState, PullStore,
    PullTransition,
};
use pyn_core::{ObjectId, PynError, RepoId, Result, UserId};
use sqlx::postgres::PgRow;
use sqlx::{Postgres, Row, Transaction};

use super::{PgMetadataStore, UNIQUE_VIOLATION, db, object_id};

macro_rules! pull_select {
    ($tail:literal) => {
        concat!(
            "SELECT p.number, p.title, p.body, p.state, p.draft, p.base, p.head, p.author, p.created_at, p.updated_at,
                    p.closed_at, p.merged_at, p.merged_by, p.merge_commit, p.frozen_base, p.frozen_head,
                    (SELECT count(*) FROM pull_events e
                      WHERE e.repo = p.repo AND e.number = p.number AND e.kind = 'comment') AS comments
             FROM pull_requests p ",
            $tail
        )
    };
}

fn optional_id(row: &PgRow, column: &str) -> Result<Option<ObjectId>> {
    row.get::<Option<String>, _>(column)
        .map(|h| object_id(&h, column))
        .transpose()
}

fn pull_from(row: &PgRow) -> Result<PullRequest> {
    let frozen = match (
        optional_id(row, "frozen_base")?,
        optional_id(row, "frozen_head")?,
    ) {
        (Some(b), Some(h)) => Some((b, h)),
        _ => None,
    };
    Ok(PullRequest {
        number: row.get::<i64, _>("number") as u64,
        title: row.get("title"),
        body: row.get("body"),
        state: PullState::parse(&row.get::<String, _>("state"))?,
        draft: row.get("draft"),
        base: row.get("base"),
        head: row.get("head"),
        author: UserId::new(row.get::<String, _>("author")),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        closed_at: row.get("closed_at"),
        merged_at: row.get("merged_at"),
        merged_by: row
            .get::<Option<String>, _>("merged_by")
            .map(|u| UserId::new(&u)),
        merge_commit: optional_id(row, "merge_commit")?,
        frozen,
        comments: row.get::<i64, _>("comments") as u32,
    })
}

fn event_from(row: &PgRow) -> Result<PullEvent> {
    Ok(PullEvent {
        id: row.get::<i64, _>("id") as u64,
        number: row.get::<i64, _>("number") as u64,
        kind: PullEventKind::parse(&row.get::<String, _>("kind"))?,
        actor: UserId::new(row.get::<String, _>("actor")),
        at: row.get("at"),
        body: row.get("body"),
        commit: optional_id(row, "commit_id")?,
    })
}

fn is_unique(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|c| c == UNIQUE_VIOLATION)
}

impl PgMetadataStore {
    async fn open_twin(&self, repo: &RepoId, base: &str, head: &str, except: i64) -> Result<u64> {
        let n: Option<i64> = sqlx::query_scalar(
            "SELECT number FROM pull_requests
             WHERE repo = $1 AND base = $2 AND head = $3 AND state = 'open' AND number <> $4",
        )
        .bind(repo.as_str())
        .bind(base)
        .bind(head)
        .bind(except)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(n.unwrap_or(0) as u64)
    }

    async fn locked_pull(
        tx: &mut Transaction<'_, Postgres>,
        repo: &RepoId,
        number: u64,
    ) -> Result<PullRequest> {
        let row = sqlx::query(pull_select!(
            "WHERE p.repo = $1 AND p.number = $2 FOR UPDATE OF p"
        ))
        .bind(repo.as_str())
        .bind(number as i64)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
        match row {
            Some(r) => pull_from(&r),
            None => Err(PynError::PullRequestNotFound(number)),
        }
    }

    async fn reload(
        tx: &mut Transaction<'_, Postgres>,
        repo: &RepoId,
        number: u64,
    ) -> Result<PullRequest> {
        let row = sqlx::query(pull_select!("WHERE p.repo = $1 AND p.number = $2"))
            .bind(repo.as_str())
            .bind(number as i64)
            .fetch_one(&mut **tx)
            .await
            .map_err(db)?;
        pull_from(&row)
    }
}

#[async_trait]
impl PullStore for PgMetadataStore {
    async fn create_pull(&self, repo: &RepoId, new: NewPull) -> Result<PullRequest> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 696))")
            .bind(repo.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let number: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(number), 0) + 1 FROM pull_requests WHERE repo = $1",
        )
        .bind(repo.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let inserted = sqlx::query(
            "INSERT INTO pull_requests
                 (repo, number, title, body, state, draft, base, head, author, created_at, updated_at)
             VALUES ($1, $2, $3, $4, 'open', $5, $6, $7, $8, $9, $9)",
        )
        .bind(repo.as_str())
        .bind(number)
        .bind(&new.title)
        .bind(&new.body)
        .bind(new.draft)
        .bind(&new.base)
        .bind(&new.head)
        .bind(new.author.as_str())
        .bind(new.now)
        .execute(&mut *tx)
        .await;
        if let Err(e) = inserted {
            drop(tx);
            return Err(if is_unique(&e) {
                PynError::PullRequestExists(self.open_twin(repo, &new.base, &new.head, 0).await?)
            } else {
                db(e)
            });
        }
        let pull = Self::reload(&mut tx, repo, number as u64).await?;
        tx.commit().await.map_err(db)?;
        Ok(pull)
    }

    async fn get_pull(&self, repo: &RepoId, number: u64) -> Result<Option<PullRequest>> {
        let row = sqlx::query(pull_select!("WHERE p.repo = $1 AND p.number = $2"))
            .bind(repo.as_str())
            .bind(number as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        row.as_ref().map(pull_from).transpose()
    }

    async fn list_pulls(
        &self,
        repo: &RepoId,
        filter: &PullFilter,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<PullRequest>> {
        let rows = sqlx::query(pull_select!(
            "WHERE p.repo = $1
               AND ($2::bigint IS NULL OR p.number < $2)
               AND ($3::text IS NULL OR p.state = $3)
               AND ($4::text IS NULL OR p.base = $4)
               AND ($5::text IS NULL OR p.head = $5)
               AND ($6::text IS NULL OR p.author = $6)
             ORDER BY p.number DESC LIMIT $7"
        ))
        .bind(repo.as_str())
        .bind(before.map(|b| b as i64))
        .bind(filter.state.map(PullState::as_str))
        .bind(filter.base.as_deref())
        .bind(filter.head.as_deref())
        .bind(filter.author.as_ref().map(UserId::as_str))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(pull_from).collect()
    }

    async fn edit_pull(
        &self,
        repo: &RepoId,
        number: u64,
        edit: PullEdit,
        now: DateTime<Utc>,
    ) -> Result<PullRequest> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let p = Self::locked_pull(&mut tx, repo, number).await?;
        if p.state != PullState::Open {
            return Err(PynError::PullRequestNotOpen {
                number,
                state: p.state.as_str().into(),
            });
        }
        sqlx::query(
            "UPDATE pull_requests
             SET title = COALESCE($3, title), body = COALESCE($4, body), draft = COALESCE($5, draft), updated_at = $6
             WHERE repo = $1 AND number = $2",
        )
        .bind(repo.as_str())
        .bind(number as i64)
        .bind(edit.title)
        .bind(edit.body)
        .bind(edit.draft)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let pull = Self::reload(&mut tx, repo, number).await?;
        tx.commit().await.map_err(db)?;
        Ok(pull)
    }

    async fn transition_pull(
        &self,
        repo: &RepoId,
        number: u64,
        transition: PullTransition,
        actor: &UserId,
        now: DateTime<Utc>,
    ) -> Result<PullRequest> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let p = Self::locked_pull(&mut tx, repo, number).await?;
        let refuse = || PynError::PullRequestNotOpen {
            number,
            state: p.state.as_str().into(),
        };
        let (kind, commit) = match &transition {
            PullTransition::Close { .. } if p.state == PullState::Open => {
                (PullEventKind::Closed, None)
            }
            PullTransition::Reopen if p.state == PullState::Closed => {
                (PullEventKind::Reopened, None)
            }
            PullTransition::Merge { commit, .. } if p.state != PullState::Merged => {
                (PullEventKind::Merged, *commit)
            }
            _ => return Err(refuse()),
        };
        let updated = match &transition {
            PullTransition::Close { base, head } => {
                sqlx::query(
                    "UPDATE pull_requests SET state = 'closed', closed_at = $3, updated_at = $3,
                         frozen_base = $4, frozen_head = $5 WHERE repo = $1 AND number = $2",
                )
                .bind(repo.as_str())
                .bind(number as i64)
                .bind(now)
                .bind(base.to_hex())
                .bind(head.to_hex())
                .execute(&mut *tx)
                .await
            }
            PullTransition::Reopen => {
                sqlx::query(
                    "UPDATE pull_requests SET state = 'open', closed_at = NULL, updated_at = $3,
                         frozen_base = NULL, frozen_head = NULL WHERE repo = $1 AND number = $2",
                )
                .bind(repo.as_str())
                .bind(number as i64)
                .bind(now)
                .execute(&mut *tx)
                .await
            }
            PullTransition::Merge { base, head, commit } => {
                sqlx::query(
                    "UPDATE pull_requests SET state = 'merged', closed_at = $3, merged_at = $3, updated_at = $3,
                         merged_by = $4, merge_commit = $5, frozen_base = $6, frozen_head = $7
                     WHERE repo = $1 AND number = $2",
                )
                .bind(repo.as_str())
                .bind(number as i64)
                .bind(now)
                .bind(actor.as_str())
                .bind(commit.map(|c| c.to_hex()))
                .bind(base.to_hex())
                .bind(head.to_hex())
                .execute(&mut *tx)
                .await
            }
        };
        if let Err(e) = updated {
            drop(tx);
            return Err(if is_unique(&e) {
                PynError::PullRequestExists(
                    self.open_twin(repo, &p.base, &p.head, number as i64)
                        .await?,
                )
            } else {
                db(e)
            });
        }
        sqlx::query(
            "INSERT INTO pull_events (repo, number, kind, actor, at, commit_id) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(repo.as_str())
        .bind(number as i64)
        .bind(kind.as_str())
        .bind(actor.as_str())
        .bind(now)
        .bind(commit.map(|c| c.to_hex()))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let pull = Self::reload(&mut tx, repo, number).await?;
        tx.commit().await.map_err(db)?;
        Ok(pull)
    }

    async fn comment_pull(
        &self,
        repo: &RepoId,
        number: u64,
        actor: &UserId,
        body: &str,
        now: DateTime<Utc>,
    ) -> Result<PullEvent> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        Self::locked_pull(&mut tx, repo, number).await?;
        sqlx::query("UPDATE pull_requests SET updated_at = $3 WHERE repo = $1 AND number = $2")
            .bind(repo.as_str())
            .bind(number as i64)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let row = sqlx::query(
            "INSERT INTO pull_events (repo, number, kind, actor, at, body) VALUES ($1, $2, 'comment', $3, $4, $5)
             RETURNING id, number, kind, actor, at, body, commit_id",
        )
        .bind(repo.as_str())
        .bind(number as i64)
        .bind(actor.as_str())
        .bind(now)
        .bind(body)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        event_from(&row)
    }

    async fn pull_events(
        &self,
        repo: &RepoId,
        number: u64,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<PullEvent>> {
        let rows = sqlx::query(
            "SELECT id, number, kind, actor, at, body, commit_id FROM pull_events
             WHERE repo = $1 AND number = $2 AND ($3::bigint IS NULL OR id > $3)
             ORDER BY id LIMIT $4",
        )
        .bind(repo.as_str())
        .bind(number as i64)
        .bind(after.map(|a| a as i64))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(event_from).collect()
    }

    async fn rename_pull_branch(&self, repo: &RepoId, from: &str, to: &str) -> Result<()> {
        for column in [
            "UPDATE pull_requests SET base = $3 WHERE repo = $1 AND base = $2",
            "UPDATE pull_requests SET head = $3 WHERE repo = $1 AND head = $2",
        ] {
            sqlx::query(column)
                .bind(repo.as_str())
                .bind(from)
                .bind(to)
                .execute(&self.pool)
                .await
                .map_err(db)?;
        }
        Ok(())
    }
}

//! PostgreSQL `MetadataStore`. Every lock and revision operation is one atomic statement or transaction.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pyn_core::{
    ContentHash, Lock, MetadataStore, NewRevision, PynError, RepoId, RepoPath, Result, Revision,
    RevisionId, UserId,
};
use sqlx::migrate::MigrateDatabase;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row};

// Scratch schema names are built from hex digits only, so interpolating them is safe.
static SCHEMA_COUNTER: AtomicU64 = AtomicU64::new(0);

const UNIQUE_VIOLATION: &str = "23505";

mod access;

pub struct PgMetadataStore {
    pool: PgPool,
    scratch_schema: Option<String>,
}

fn db(e: sqlx::Error) -> PynError {
    PynError::Storage(e.to_string())
}

fn lock_from(row: &PgRow) -> Result<Lock> {
    Ok(Lock {
        path: RepoPath::new(row.get::<String, _>("path"))?,
        owner: UserId::new(row.get::<String, _>("owner")),
        acquired_at: row.get("acquired_at"),
        expires_at: row.get("expires_at"),
    })
}

fn revision_from(row: &PgRow) -> Result<Revision> {
    Ok(Revision {
        id: RevisionId(row.get::<i64, _>("id") as u64),
        path: RepoPath::new(row.get::<String, _>("path"))?,
        content: ContentHash::new(row.get::<String, _>("content")),
        author: UserId::new(row.get::<String, _>("author")),
        message: row.get("message"),
        created_at: row.get("created_at"),
        restored_from: row
            .get::<Option<i64>, _>("restored_from")
            .map(|r| RevisionId(r as u64)),
    })
}

impl PgMetadataStore {
    /// Connects and applies migrations, creating the database first when `create_database` is set.
    pub async fn connect(url: &str, create_database: bool) -> Result<Self> {
        if create_database && !Postgres::database_exists(url).await.map_err(db)? {
            Postgres::create_database(url).await.map_err(db)?;
        }
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await
            .map_err(db)?;
        Self::from_pool(pool).await
    }

    pub fn scratch_schema(&self) -> Option<&str> {
        self.scratch_schema.as_deref()
    }

    pub async fn from_pool(pool: PgPool) -> Result<Self> {
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| PynError::Storage(e.to_string()))?;
        Ok(Self {
            pool,
            scratch_schema: None,
        })
    }

    /// For tests: an isolated schema in the target database, dropped when the store is dropped.
    pub async fn connect_in_scratch_schema(url: &str) -> Result<Self> {
        let n = SCHEMA_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let schema = format!("pyn_test_{:x}_{:x}_{n}", std::process::id(), nanos);

        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await
            .map_err(db)?;
        sqlx::query(AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
            .execute(&admin)
            .await
            .map_err(db)?;
        admin.close().await;

        let search_path = format!("SET search_path TO \"{schema}\"");
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .after_connect(move |conn, _| {
                let stmt = search_path.clone();
                Box::pin(async move {
                    sqlx::query(AssertSqlSafe(stmt)).execute(conn).await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
            .map_err(db)?;
        let mut store = Self::from_pool(pool).await?;
        store.scratch_schema = Some(schema);
        Ok(store)
    }
}

impl Drop for PgMetadataStore {
    fn drop(&mut self) {
        let Some(schema) = self.scratch_schema.take() else {
            return;
        };
        let pool = self.pool.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            tokio::task::block_in_place(|| {
                handle.block_on(async move {
                    let _ = sqlx::query(AssertSqlSafe(format!("DROP SCHEMA \"{schema}\" CASCADE")))
                        .execute(&pool)
                        .await;
                });
            });
        }
    }
}

#[async_trait]
impl MetadataStore for PgMetadataStore {
    async fn acquire_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Lock> {
        // The WHERE clause makes the upsert a no-op when a live lock belongs to someone else.
        const UPSERT: &str = "
            INSERT INTO locks AS l (repo, path, owner, acquired_at, expires_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (repo, path) DO UPDATE SET
                acquired_at = CASE WHEN l.owner = EXCLUDED.owner AND l.expires_at > $4
                                   THEN l.acquired_at ELSE EXCLUDED.acquired_at END,
                owner = EXCLUDED.owner,
                expires_at = EXCLUDED.expires_at
            WHERE l.owner = EXCLUDED.owner OR l.expires_at <= $4
            RETURNING path, owner, acquired_at, expires_at";

        for _ in 0..3 {
            let row = sqlx::query(UPSERT)
                .bind(repo.as_str())
                .bind(path.as_str())
                .bind(owner.as_str())
                .bind(now)
                .bind(expires_at)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
            if let Some(row) = row {
                return lock_from(&row);
            }
            // Lost to a live lock; read it for the error. If it expired meanwhile, retry.
            if let Some(current) = self.get_lock(repo, path, now).await? {
                return Err(PynError::LockHeld {
                    path: path.clone(),
                    owner: current.owner,
                    expires_at: current.expires_at,
                });
            }
        }
        Err(PynError::Storage("lock acquisition kept racing".into()))
    }

    async fn release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let done = sqlx::query(
            "DELETE FROM locks WHERE repo = $1 AND path = $2 AND owner = $3 AND expires_at > $4",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .bind(owner.as_str())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if done.rows_affected() == 0 {
            return Err(PynError::NotLockHolder(path.clone()));
        }
        Ok(())
    }

    async fn get_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: DateTime<Utc>,
    ) -> Result<Option<Lock>> {
        let row = sqlx::query(
            "SELECT path, owner, acquired_at, expires_at FROM locks
             WHERE repo = $1 AND path = $2 AND expires_at > $3",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(lock_from).transpose()
    }

    async fn list_locks(&self, repo: &RepoId, now: DateTime<Utc>) -> Result<Vec<Lock>> {
        let rows = sqlx::query(
            "SELECT path, owner, acquired_at, expires_at FROM locks
             WHERE repo = $1 AND expires_at > $2 ORDER BY path",
        )
        .bind(repo.as_str())
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(lock_from).collect()
    }

    async fn head_revision(&self, repo: &RepoId, path: &RepoPath) -> Result<Option<Revision>> {
        let row = sqlx::query(
            "SELECT id, path, content, author, message, created_at, restored_from FROM revisions
             WHERE repo = $1 AND path = $2 ORDER BY id DESC LIMIT 1",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(revision_from).transpose()
    }

    async fn commit_revision(
        &self,
        repo: &RepoId,
        revision: NewRevision,
        expected_head: Option<RevisionId>,
        lock_holder: Option<&UserId>,
        now: DateTime<Utc>,
    ) -> Result<Revision> {
        let mut tx = self.pool.begin().await.map_err(db)?;

        if let Some(user) = lock_holder {
            let row = sqlx::query(
                "SELECT owner, expires_at FROM locks WHERE repo = $1 AND path = $2 FOR UPDATE",
            )
            .bind(repo.as_str())
            .bind(revision.path.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
            match row {
                Some(r) if r.get::<DateTime<Utc>, _>("expires_at") > now => {
                    let owner: String = r.get("owner");
                    if owner != user.as_str() {
                        return Err(PynError::LockHeld {
                            path: revision.path,
                            owner: UserId::new(owner),
                            expires_at: r.get("expires_at"),
                        });
                    }
                }
                _ => return Err(PynError::LockRequired(revision.path)),
            }
        }

        let expected = expected_head.map_or(0, |r| r.0 as i64);
        // Inserts only when the head still equals `expected`; a concurrent insert of the same id hits the primary key.
        let inserted = sqlx::query(
            "INSERT INTO revisions (repo, path, id, content, author, message, created_at, restored_from)
             SELECT $1, $2, $3, $4, $5, $6, $7, $9::bigint
             WHERE COALESCE((SELECT max(id) FROM revisions WHERE repo = $1 AND path = $2), 0) = $8
             RETURNING id",
        )
        .bind(repo.as_str())
        .bind(revision.path.as_str())
        .bind(expected + 1)
        .bind(revision.content.as_str())
        .bind(revision.author.as_str())
        .bind(&revision.message)
        .bind(revision.created_at)
        .bind(expected)
        .bind(revision.restored_from.map(|r| r.0 as i64))
        .fetch_optional(&mut *tx)
        .await;

        let stale = |actual| PynError::StaleBase {
            path: revision.path.clone(),
            expected: expected_head,
            actual,
        };
        match inserted {
            Ok(Some(_)) => {}
            Ok(None) => {
                let head: Option<i64> = sqlx::query_scalar(
                    "SELECT max(id) FROM revisions WHERE repo = $1 AND path = $2",
                )
                .bind(repo.as_str())
                .bind(revision.path.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
                return Err(stale(head.map(|h| RevisionId(h as u64))));
            }
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some(UNIQUE_VIOLATION) => {
                return Err(stale(None));
            }
            Err(e) => return Err(db(e)),
        }

        if lock_holder.is_some() {
            sqlx::query("DELETE FROM locks WHERE repo = $1 AND path = $2")
                .bind(repo.as_str())
                .bind(revision.path.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;

        Ok(Revision {
            id: RevisionId((expected + 1) as u64),
            path: revision.path,
            content: revision.content,
            author: revision.author,
            message: revision.message,
            created_at: revision.created_at,
            restored_from: revision.restored_from,
        })
    }

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> Result<Option<Revision>> {
        let row = sqlx::query(
            "SELECT id, path, content, author, message, created_at, restored_from FROM revisions
             WHERE repo = $1 AND path = $2 AND id = $3",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .bind(id.0 as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(revision_from).transpose()
    }

    async fn list_head_revisions(&self, repo: &RepoId) -> Result<Vec<Revision>> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (path) id, path, content, author, message, created_at, restored_from
             FROM revisions WHERE repo = $1 ORDER BY path, id DESC",
        )
        .bind(repo.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(revision_from).collect()
    }

    async fn history(&self, repo: &RepoId, path: &RepoPath) -> Result<Vec<Revision>> {
        let rows = sqlx::query(
            "SELECT id, path, content, author, message, created_at, restored_from FROM revisions
             WHERE repo = $1 AND path = $2 ORDER BY id",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(revision_from).collect()
    }
}

//! PostgreSQL `MetadataStore`. Every lock and revision operation is one atomic statement or transaction.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::str::FromStr;

use pyn_core::{
    ContentHash, HistoryCursor, Lock, MetadataStore, Mode, NewRevision, PathFilter, PynError,
    RepoId, RepoPath, RepoRecord, RepoSettings, RepoUpdate, Result, Revision, RevisionId, UserId,
    Visibility,
};
use sqlx::migrate::MigrateDatabase;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row};

// Scratch schema names are built from hex digits only, so interpolating them is safe.
static SCHEMA_COUNTER: AtomicU64 = AtomicU64::new(0);

const UNIQUE_VIOLATION: &str = "23505";

mod access;
mod audit;

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
        mode: row
            .get::<Option<String>, _>("mode")
            .map(|m| mode_from(&m))
            .transpose()?,
    })
}

fn mode_str(mode: Mode) -> &'static str {
    match mode {
        Mode::Shared => "shared",
        Mode::Exclusive => "exclusive",
    }
}

fn mode_from(s: &str) -> Result<Mode> {
    match s {
        "shared" => Ok(Mode::Shared),
        "exclusive" => Ok(Mode::Exclusive),
        other => Err(PynError::Storage(format!("unknown mode {other:?}"))),
    }
}

fn repo_from(row: &PgRow) -> Result<RepoRecord> {
    Ok(RepoRecord {
        id: RepoId::new(row.get::<String, _>("id")),
        owner: UserId::new(row.get::<String, _>("owner")),
        name: row.get("name"),
        visibility: Visibility::from_str(row.get("visibility"))?,
        settings: RepoSettings {
            lease_hours: row.get::<i32, _>("lease_hours") as u32,
            max_locks: row.get::<Option<i32>, _>("max_locks").map(|n| n as u32),
        },
        created_at: row.get("created_at"),
    })
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some(UNIQUE_VIOLATION))
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
        max_locks: u32,
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
            let mut tx = self.pool.begin().await.map_err(db)?;
            // Serializes one user's acquisitions in a repository so the count below cannot go stale.
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("{}/{}", repo.as_str(), owner.as_str()))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            let live: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM locks WHERE repo = $1 AND path = $2 AND expires_at > $3)",
            )
            .bind(repo.as_str())
            .bind(path.as_str())
            .bind(now)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            if !live {
                let held: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM locks WHERE repo = $1 AND owner = $2 AND expires_at > $3",
                )
                .bind(repo.as_str())
                .bind(owner.as_str())
                .bind(now)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
                if held >= i64::from(max_locks) {
                    return Err(PynError::LockLimitReached { limit: max_locks });
                }
            }
            let row = sqlx::query(UPSERT)
                .bind(repo.as_str())
                .bind(path.as_str())
                .bind(owner.as_str())
                .bind(now)
                .bind(expires_at)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
            if let Some(row) = row {
                tx.commit().await.map_err(db)?;
                return lock_from(&row);
            }
            drop(tx);
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

    async fn force_release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: DateTime<Utc>,
    ) -> Result<Option<Lock>> {
        let row = sqlx::query(
            "DELETE FROM locks WHERE repo = $1 AND path = $2 AND expires_at > $3
             RETURNING path, owner, acquired_at, expires_at",
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

    async fn list_locks_of(
        &self,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<Vec<(RepoId, Lock)>> {
        let rows = sqlx::query(
            "SELECT repo, path, owner, acquired_at, expires_at FROM locks
             WHERE owner = $1 AND expires_at > $2 ORDER BY repo COLLATE \"C\", path COLLATE \"C\"",
        )
        .bind(owner.as_str())
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|r| Ok((RepoId::new(r.get::<String, _>("repo")), lock_from(r)?)))
            .collect()
    }

    async fn head_revision(&self, repo: &RepoId, path: &RepoPath) -> Result<Option<Revision>> {
        let row = sqlx::query(
            "SELECT id, path, content, author, message, created_at, restored_from, mode FROM revisions
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
            "INSERT INTO revisions (repo, path, id, content, author, message, created_at, restored_from, mode)
             SELECT $1, $2, $3, $4, $5, $6, $7, $9::bigint, $10
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
        .bind(mode_str(revision.mode))
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
            mode: Some(revision.mode),
        })
    }

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> Result<Option<Revision>> {
        let row = sqlx::query(
            "SELECT id, path, content, author, message, created_at, restored_from, mode FROM revisions
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
            "SELECT DISTINCT ON (path) id, path, content, author, message, created_at, restored_from, mode
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
            "SELECT id, path, content, author, message, created_at, restored_from, mode FROM revisions
             WHERE repo = $1 AND path = $2 ORDER BY id",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(revision_from).collect()
    }

    async fn repo_history(
        &self,
        repo: &RepoId,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: usize,
    ) -> Result<Vec<Revision>> {
        // The glob cannot run in SQL, so scan keyset batches and filter until `limit` rows match.
        let batch = (limit.max(1) * 4).clamp(50, 1000) as i64;
        let mut cursor = before.cloned();
        let mut out = Vec::new();
        loop {
            let rows = sqlx::query(
                "SELECT id, path, content, author, message, created_at, restored_from, mode FROM revisions
                 WHERE repo = $1
                   AND ($2::timestamptz IS NULL
                        OR (created_at, path COLLATE \"C\", id) < ($2, $3::text COLLATE \"C\", $4::bigint))
                 ORDER BY created_at DESC, path COLLATE \"C\" DESC, id DESC
                 LIMIT $5",
            )
            .bind(repo.as_str())
            .bind(cursor.as_ref().map(|c| c.created_at))
            .bind(cursor.as_ref().map(|c| c.path.as_str().to_string()))
            .bind(cursor.as_ref().map(|c| c.id.0 as i64))
            .bind(batch)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
            let scanned = rows.len() as i64;
            for row in &rows {
                let rev = revision_from(row)?;
                cursor = Some(HistoryCursor::of(&rev));
                if filter.is_none_or(|f| f.is_match(&rev.path)) {
                    out.push(rev);
                    if out.len() == limit {
                        return Ok(out);
                    }
                }
            }
            if scanned < batch {
                return Ok(out);
            }
        }
    }

    async fn create_repo(&self, repo: RepoRecord) -> Result<RepoRecord> {
        let inserted = sqlx::query(
            "INSERT INTO repositories (id, owner, name, visibility, lease_hours, max_locks, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(repo.id.as_str())
        .bind(repo.owner.as_str())
        .bind(&repo.name)
        .bind(repo.visibility.as_str())
        .bind(repo.settings.lease_hours as i32)
        .bind(repo.settings.max_locks.map(|n| n as i32))
        .bind(repo.created_at)
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(_) => Ok(repo),
            Err(e) if is_unique_violation(&e) => Err(PynError::RepoExists(repo.address())),
            Err(e) => Err(db(e)),
        }
    }

    async fn find_repo(&self, owner: &UserId, name: &str) -> Result<Option<RepoRecord>> {
        let row = sqlx::query(
            "SELECT id, owner, name, visibility, lease_hours, max_locks, created_at FROM repositories
             WHERE owner = $1 AND name = $2",
        )
        .bind(owner.as_str())
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(repo_from).transpose()
    }

    async fn get_repo(&self, id: &RepoId) -> Result<Option<RepoRecord>> {
        let row = sqlx::query(
            "SELECT id, owner, name, visibility, lease_hours, max_locks, created_at FROM repositories
             WHERE id = $1",
        )
        .bind(id.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(repo_from).transpose()
    }

    async fn list_repos(&self, owner: Option<&UserId>) -> Result<Vec<RepoRecord>> {
        let rows = sqlx::query(
            "SELECT id, owner, name, visibility, lease_hours, max_locks, created_at FROM repositories
             WHERE ($1::text IS NULL OR owner = $1) ORDER BY owner, name",
        )
        .bind(owner.map(UserId::as_str))
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(repo_from).collect()
    }

    async fn list_public_repos(
        &self,
        after: Option<(&UserId, &str)>,
        limit: usize,
    ) -> Result<Vec<RepoRecord>> {
        let rows = sqlx::query(
            "SELECT id, owner, name, visibility, lease_hours, max_locks, created_at FROM repositories
             WHERE visibility = 'public' AND ($1::text IS NULL OR (owner COLLATE \"C\", name COLLATE \"C\") > ($1, $2))
             ORDER BY owner COLLATE \"C\", name COLLATE \"C\" LIMIT $3",
        )
        .bind(after.map(|(o, _)| o.as_str()))
        .bind(after.map(|(_, n)| n))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(repo_from).collect()
    }

    async fn update_repo(&self, id: &RepoId, update: RepoUpdate) -> Result<RepoRecord> {
        let current = self
            .get_repo(id)
            .await?
            .ok_or_else(|| PynError::RepoNotFound(id.to_string()))?;
        let updated = sqlx::query(
            "UPDATE repositories SET name = COALESCE($2, name), visibility = COALESCE($3, visibility),
                 lease_hours = COALESCE($4, lease_hours),
                 max_locks = CASE WHEN $5 THEN $6 ELSE max_locks END
             WHERE id = $1
             RETURNING id, owner, name, visibility, lease_hours, max_locks, created_at",
        )
        .bind(id.as_str())
        .bind(update.name.as_deref())
        .bind(update.visibility.map(Visibility::as_str))
        .bind(update.settings.map(|s| s.lease_hours as i32))
        .bind(update.settings.is_some())
        .bind(update.settings.and_then(|s| s.max_locks).map(|n| n as i32))
        .fetch_optional(&self.pool)
        .await;
        match updated {
            Ok(Some(row)) => repo_from(&row),
            Ok(None) => Err(PynError::RepoNotFound(id.to_string())),
            Err(e) if is_unique_violation(&e) => Err(PynError::RepoExists(format!(
                "{}/{}",
                current.owner,
                update.name.unwrap_or_default()
            ))),
            Err(e) => Err(db(e)),
        }
    }

    async fn delete_repo(&self, id: &RepoId) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        for stmt in [
            "DELETE FROM locks WHERE repo = $1",
            "DELETE FROM revisions WHERE repo = $1",
        ] {
            sqlx::query(stmt)
                .bind(id.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        let removed = sqlx::query("DELETE FROM repositories WHERE id = $1")
            .bind(id.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(removed.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn rewound_store() -> PgMetadataStore {
        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        sqlx::query("DROP TABLE repositories")
            .execute(&store.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version IN (8, 11)")
            .execute(&store.pool)
            .await
            .unwrap();
        store
    }

    async fn migrate(store: &PgMetadataStore) {
        sqlx::migrate!("./migrations")
            .run(&store.pool)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn the_single_repository_data_is_registered_under_its_oldest_admin() {
        let store = rewound_store().await;
        for stmt in [
            "INSERT INTO users (id, created_at) VALUES ('zed', now() - interval '3 days'), ('amy', now() - interval '2 days'), ('bob', now() - interval '1 day')",
            "INSERT INTO memberships (repo, user_id, role) VALUES ('default', 'zed', 'reader'), ('default', 'bob', 'admin'), ('default', 'amy', 'admin')",
            "INSERT INTO locks (repo, path, owner, acquired_at, expires_at) VALUES ('default', 'a', 'amy', now(), now() + interval '1 hour')",
        ] {
            sqlx::query(stmt).execute(&store.pool).await.unwrap();
        }
        migrate(&store).await;

        let repo = store
            .find_repo(&UserId::new("amy"), "default")
            .await
            .unwrap();
        let repo = repo.expect("registered under the oldest admin");
        assert_eq!(repo.id, RepoId::new("default"));
        assert_eq!(repo.visibility, Visibility::Private);
        assert_eq!(repo.settings, RepoSettings::default());
        assert_eq!(store.list_repos(None).await.unwrap().len(), 1);
        let locks = store.list_locks(&repo.id, Utc::now()).await.unwrap();
        assert_eq!(locks.len(), 1, "existing data stays under the same id");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn an_empty_database_gets_no_default_repository() {
        let store = rewound_store().await;
        migrate(&store).await;
        assert!(store.list_repos(None).await.unwrap().is_empty());
    }
}

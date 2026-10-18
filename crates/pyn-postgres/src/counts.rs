//! Per-commit counts of what the snapshot holds (migration 0036), kept as each commit is stored.

use pyn_core::{HeadCounts, ObjectId, PynError, RepoId, Result};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::{CHAIN, db};

/// A commit's counts from its first parent's and the pointers it wrote; NULL when the parent is not counted.
/// Keep identical to the backfill in migrations/0036_commit_counts.sql.
const COUNT_COMMIT: &str = "
    WITH RECURSIVE self AS (
        SELECT parent FROM commits WHERE repo = $1 AND id = $2
    ), chain(id, parent, depth) AS (
        SELECT k.id, k.parent, 0 FROM commits k WHERE k.repo = $1 AND k.id = (SELECT parent FROM self)
        UNION ALL
        SELECT k.id, k.parent, chain.depth + 1 FROM chain JOIN commits k ON k.repo = $1 AND k.id = chain.parent
    ), mine AS (
        SELECT r.path, r.deleted, COALESCE(r.mode = 'exclusive', false) AS exclusive, r.created_at
        FROM commit_pointers p JOIN revisions r ON r.repo = p.repo AND r.path = p.path AND r.id = p.revision
        WHERE p.repo = $1 AND p.commit_id = $2
    ), prior AS (
        SELECT DISTINCT ON (p.path) p.path, r.deleted, COALESCE(r.mode = 'exclusive', false) AS exclusive
        FROM chain
        JOIN commit_pointers p ON p.repo = $1 AND p.commit_id = chain.id
        JOIN revisions r ON r.repo = p.repo AND r.path = p.path AND r.id = p.revision
        WHERE p.path IN (SELECT path FROM mine)
        ORDER BY p.path, chain.depth
    ), delta AS (
        SELECT COALESCE(SUM((NOT m.deleted)::int - (b.path IS NOT NULL AND NOT b.deleted)::int), 0) AS f,
               COALESCE(SUM((NOT m.deleted AND m.exclusive)::int
                            - (b.path IS NOT NULL AND NOT b.deleted AND b.exclusive)::int), 0) AS x,
               MAX(m.created_at) AS t
        FROM mine m LEFT JOIN prior b ON b.path = m.path
    ), fresh AS (
        SELECT CASE WHEN s.parent IS NULL THEN 0 ELSE pc.file_count END + d.f AS f,
               CASE WHEN s.parent IS NULL THEN 0 ELSE pc.exclusive_count END + d.x AS x,
               GREATEST(pc.newest_at, d.t) AS t
        FROM self s CROSS JOIN delta d LEFT JOIN commits pc ON pc.repo = $1 AND pc.id = s.parent
    )
    UPDATE commits SET file_count = fresh.f, exclusive_count = fresh.x, newest_at = fresh.t
    FROM fresh WHERE commits.repo = $1 AND commits.id = $2";

/// Counts `commit` once its pointers are stored; its first parent must be counted already.
pub(crate) async fn count_commit(
    tx: &mut Transaction<'_, Postgres>,
    repo: &RepoId,
    commit: &ObjectId,
) -> Result<()> {
    sqlx::query(COUNT_COMMIT)
        .bind(repo.as_str())
        .bind(commit.to_hex())
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    Ok(())
}

/// Counts every commit still without counts, oldest first. Rows whose parent is uncounted stay uncounted.
pub(crate) async fn backfill(pool: &PgPool) -> Result<u64> {
    let mut tx = pool.begin().await.map_err(db)?;
    let rows =
        sqlx::query("SELECT repo, id FROM commits WHERE file_count IS NULL ORDER BY repo, seq")
            .fetch_all(&mut *tx)
            .await
            .map_err(db)?;
    for row in &rows {
        sqlx::query(COUNT_COMMIT)
            .bind(row.get::<String, _>("repo"))
            .bind(row.get::<String, _>("id"))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
    }
    tx.commit().await.map_err(db)?;
    Ok(rows.len() as u64)
}

/// The counts of `commit`: the stored ones, else counted from its snapshot.
pub(crate) async fn head_counts(
    pool: &PgPool,
    repo: &RepoId,
    commit: &ObjectId,
) -> Result<HeadCounts> {
    let row = sqlx::query(
        "SELECT file_count, exclusive_count, newest_at FROM commits WHERE repo = $1 AND id = $2",
    )
    .bind(repo.as_str())
    .bind(commit.to_hex())
    .fetch_optional(pool)
    .await
    .map_err(db)?
    .ok_or_else(|| PynError::CommitNotFound(commit.to_hex()))?;
    if let Some(files) = row.get::<Option<i64>, _>("file_count") {
        return Ok(HeadCounts {
            files: files as u64,
            exclusive_files: row.get::<Option<i64>, _>("exclusive_count").unwrap_or(0) as u64,
            updated_at: row.get("newest_at"),
        });
    }
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{CHAIN}, ptr AS (
             SELECT p.path, p.revision, chain.depth
             FROM chain JOIN commit_pointers p ON p.repo = $1 AND p.commit_id = chain.id
         ), latest AS (
             SELECT DISTINCT ON (path COLLATE \"C\") path, revision FROM ptr ORDER BY path COLLATE \"C\", depth
         )
         SELECT (SELECT count(*) FROM latest l JOIN revisions r ON r.repo = $1 AND r.path = l.path AND r.id = l.revision
                 WHERE NOT r.deleted) AS files,
                (SELECT count(*) FROM latest l JOIN revisions r ON r.repo = $1 AND r.path = l.path AND r.id = l.revision
                 WHERE NOT r.deleted AND r.mode = 'exclusive') AS exclusive,
                (SELECT max(r.created_at) FROM ptr JOIN revisions r
                 ON r.repo = $1 AND r.path = ptr.path AND r.id = ptr.revision) AS newest"
    )))
    .bind(repo.as_str())
    .bind(commit.to_hex())
    .fetch_one(pool)
    .await
    .map_err(db)?;
    Ok(HeadCounts {
        files: row.get::<i64, _>("files") as u64,
        exclusive_files: row.get::<i64, _>("exclusive") as u64,
        updated_at: row.get("newest"),
    })
}

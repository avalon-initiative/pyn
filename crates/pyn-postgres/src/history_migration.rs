//! One-time migration of history that predates commit records into a single mainline of git commits (decision #245).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use pyn_core::commit_record::{TreeChange, TreeEdit, commit_time, draft_commit};
use pyn_core::{FileMode, ObjectId, Person, PynError, RepoPath, Result, UserId};
use sqlx::Row;

use crate::{PgMetadataStore, db, insert_commit, object_id};

// Arbitrary constant: concurrent starts take turns instead of migrating the same rows twice.
const LOCK_KEY: i64 = 0x0070_796e_6869_7374;

/// What `migrate_history` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct HistoryMigration {
    pub repositories: usize,
    pub commits: usize,
}

struct Group {
    seq: i64,
    author: String,
    message: String,
    at: DateTime<Utc>,
    changeset: Option<String>,
    changes: Vec<(String, String)>,
    rows: Vec<(String, i64)>,
}

impl PgMetadataStore {
    /// Gives every revision that has no commit one: the old commit numbers become the mainline in order, each commit
    /// carries the tree of the repository as of its number, and the revisions keep their v1 hashes. Run it after
    /// `migrate_content_ids`, so the trees name blob ids. Idempotent.
    pub async fn migrate_history(&self) -> Result<HistoryMigration> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let repos: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT repo FROM revisions WHERE commit_id IS NULL ORDER BY repo",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let mut report = HistoryMigration::default();
        for repo in &repos {
            let groups = groups_of(&mut tx, repo).await?;
            let repo_id = pyn_core::RepoId::new(repo.clone());
            let mut parent: Option<(ObjectId, ObjectId)> = None;
            let mut trees: HashMap<ObjectId, Vec<u8>> = HashMap::new();
            for g in groups {
                let mut changes = Vec::new();
                for (path, content) in &g.changes {
                    changes.push(TreeChange {
                        path: RepoPath::from_stored(path.clone())?,
                        mode: FileMode::Regular,
                        id: object_id(content, "revision content id")?,
                        removed: false,
                    });
                }
                let mut edit = TreeEdit::new(parent.as_ref().map(|p| p.1), changes);
                while let Some(id) = edit.wanted() {
                    let body = trees.get(&id).ok_or_else(|| {
                        PynError::Storage(format!("tree {id} is missing while migrating"))
                    })?;
                    edit.supply(id, body)?;
                }
                let built = edit.finish().map_err(|e| {
                    PynError::Storage(format!("migrating commit {} of {repo}: {e}", g.seq))
                })?;
                let person = Person::for_user(&UserId::new(g.author.clone()), None);
                let mut draft = draft_commit(
                    parent.as_ref().map(|p| p.0).into_iter().collect(),
                    built,
                    &person,
                    &g.message,
                    commit_time(g.at),
                    g.changeset.clone(),
                )?;
                for (id, body) in std::mem::take(&mut draft.trees) {
                    sqlx::query(
                        "INSERT INTO trees (repo, id, body) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                    )
                    .bind(repo)
                    .bind(id.to_hex())
                    .bind(&body)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                    trees.insert(id, body);
                }
                insert_commit(&mut tx, &repo_id, g.seq as u64, &draft.record).await?;
                for (path, id) in &g.rows {
                    sqlx::query(
                        "UPDATE revisions SET commit_id = $4 WHERE repo = $1 AND path = $2 AND id = $3",
                    )
                    .bind(repo)
                    .bind(path)
                    .bind(id)
                    .bind(draft.record.id.to_hex())
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                    sqlx::query(
                        "INSERT INTO commit_pointers (repo, commit_id, path, revision) VALUES ($1, $2, $3, $4)
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(repo)
                    .bind(draft.record.id.to_hex())
                    .bind(path)
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                }
                parent = Some((draft.record.id, draft.record.tree));
                report.commits += 1;
            }
            report.repositories += 1;
        }
        tx.commit().await.map_err(db)?;
        Ok(report)
    }
}

/// The repository's old commits in order, each with the files it set.
async fn groups_of(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &str,
) -> Result<Vec<Group>> {
    let rows = sqlx::query(
        "SELECT commit_no, path, id, content, author, message, created_at, changeset FROM revisions
         WHERE repo = $1 ORDER BY commit_no, path COLLATE \"C\"",
    )
    .bind(repo)
    .fetch_all(&mut **tx)
    .await
    .map_err(db)?;
    let mut groups: Vec<Group> = Vec::new();
    for r in &rows {
        let seq: i64 = r.get("commit_no");
        let path: String = r.get("path");
        if groups.last().is_none_or(|g| g.seq != seq) {
            groups.push(Group {
                seq,
                author: r.get("author"),
                message: r.get("message"),
                at: r.get("created_at"),
                changeset: r.get("changeset"),
                changes: Vec::new(),
                rows: Vec::new(),
            });
        }
        let g = groups.last_mut().expect("just pushed");
        g.changes.push((path.clone(), r.get("content")));
        g.rows.push((path, r.get("id")));
    }
    Ok(groups)
}

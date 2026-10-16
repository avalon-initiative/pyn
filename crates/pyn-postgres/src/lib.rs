//! PostgreSQL `MetadataStore`. Every lock and revision operation is one atomic statement or transaction.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::str::FromStr;

use pyn_core::branch::{DEFAULT_BRANCH, conflicts};
use pyn_core::commit_record::{CommitDraft, TreeChange, TreeEdit, draft_commit};
use pyn_core::merge::{MergeWrite, MergeWritten};
use pyn_core::{
    Branch, ChangesetItem, Commit, FileMode, HistoryCursor, Lock, MetadataStore, Mode, ObjectId,
    PathFilter, Person, PushedCommit, PynError, REPO_ALGORITHM, RepoId, RepoPath, RepoRecord,
    RepoSettings, RepoUpdate, RepoUsage, Result, Revision, RevisionHash, RevisionId, StorageCap,
    Usage, UserId, Visibility,
};
use sqlx::migrate::MigrateDatabase;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{AssertSqlSafe, PgPool, Postgres, Row};

// Scratch schema names are built from hex digits only, so interpolating them is safe.
static SCHEMA_COUNTER: AtomicU64 = AtomicU64::new(0);

const UNIQUE_VIOLATION: &str = "23505";

mod access;
mod audit;
mod branch_migration;
mod content_ids;
mod hashing;
mod history_migration;

/// Which clock judges lease expiry and rate-limit windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSource {
    /// The database's clock, so skewed server copies agree.
    Database,
    /// The `now` the caller passes; lets tests drive time.
    Caller,
}

pub struct PgMetadataStore {
    pool: PgPool,
    scratch_schema: Option<String>,
    time: TimeSource,
}

fn db(e: sqlx::Error) -> PynError {
    PynError::Storage(e.to_string())
}

fn lock_from(row: &PgRow) -> Result<Lock> {
    Ok(Lock {
        path: RepoPath::from_stored(row.get::<String, _>("path"))?,
        owner: UserId::new(row.get::<String, _>("owner")),
        acquired_at: row.get("acquired_at"),
        expires_at: row.get("expires_at"),
    })
}

const REVISION_COLUMNS: &str = "id, path, content, author, message, created_at, restored_from, mode, changeset, commit_no, hash, previous_hash, content_v1, commit_id, file_mode, author_name, author_email, author_offset, hash_version, base_id, deleted";

/// The first-parent chain from commit `$2` of repository `$1`, nearest first.
const CHAIN: &str = "WITH RECURSIVE chain(id, parent, depth) AS (
        SELECT id, parent, 0 FROM commits WHERE repo = $1 AND id = $2
        UNION ALL
        SELECT c.id, c.parent, chain.depth + 1 FROM chain JOIN commits c ON c.repo = $1 AND c.id = chain.parent
    )";

/// Commit `$1`'s ancestors through every parent, itself included, as `anc(id)`; empty when `$1` is NULL.
const ANCESTORS: &str = "WITH RECURSIVE anc(id) AS (
        SELECT $1::text WHERE $1::text IS NOT NULL
        UNION
        SELECT p FROM anc JOIN commits c ON c.repo = $2 AND c.id = anc.id CROSS JOIN LATERAL unnest(c.parents) AS p
    )";

fn object_id(hex: &str, what: &str) -> Result<ObjectId> {
    ObjectId::from_hex(REPO_ALGORITHM, hex).map_err(|e| PynError::Storage(format!("{what}: {e}")))
}

/// The all-zero id stands in for the commit of a row `migrate_history` has not reached yet.
fn unmigrated_commit() -> ObjectId {
    ObjectId::from_hex(REPO_ALGORITHM, &"0".repeat(64)).expect("64 hex digits")
}

fn file_mode_from(s: &str) -> Result<FileMode> {
    match s {
        "100644" => Ok(FileMode::Regular),
        "100755" => Ok(FileMode::Executable),
        "120000" => Ok(FileMode::Symlink),
        other => Err(PynError::Storage(format!("unknown file mode {other:?}"))),
    }
}

fn revision_from(row: &PgRow) -> Result<Revision> {
    let person = row
        .get::<Option<String>, _>("author_name")
        .map(|name| Person {
            name,
            email: row
                .get::<Option<String>, _>("author_email")
                .unwrap_or_default(),
            offset_minutes: row
                .get::<Option<i32>, _>("author_offset")
                .unwrap_or_default(),
        });
    Ok(Revision {
        id: RevisionId(row.get::<i64, _>("id") as u64),
        path: RepoPath::from_stored(row.get::<String, _>("path"))?,
        content: ObjectId::from_hex(REPO_ALGORITHM, &row.get::<String, _>("content"))
            .map_err(|e| PynError::Storage(format!("revision content id: {e}")))?,
        legacy_content: row.get("content_v1"),
        author: UserId::new(row.get::<String, _>("author")),
        person,
        hash_version: row.get::<i32, _>("hash_version") as u32,
        message: row.get("message"),
        created_at: row.get("created_at"),
        restored_from: row
            .get::<Option<i64>, _>("restored_from")
            .map(|r| RevisionId(r as u64)),
        mode: row
            .get::<Option<String>, _>("mode")
            .map(|m| mode_from(&m))
            .transpose()?,
        file_mode: file_mode_from(&row.get::<String, _>("file_mode"))?,
        deleted: row.get("deleted"),
        changeset: row.get("changeset"),
        commit: match row.get::<Option<String>, _>("commit_id") {
            Some(hex) => object_id(&hex, "revision commit id")?,
            None => unmigrated_commit(),
        },
        hash: RevisionHash::new(row.get::<Option<String>, _>("hash").unwrap_or_default()),
        previous: row
            .get::<Option<String>, _>("previous_hash")
            .map(RevisionHash::new),
        base: row
            .get::<Option<i64>, _>("base_id")
            .map(|b| RevisionId(b as u64)),
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

/// Appends the revision inside `tx` if its head, lock and storage checks pass; the caller commits.
async fn append_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    item: ChangesetItem,
    lease_now: Option<DateTime<Utc>>,
    cap: Option<&StorageCap>,
    commit: (u64, ObjectId),
    at_branch: &HashMap<String, i64>,
) -> Result<Revision> {
    let ChangesetItem {
        revision,
        expected_head,
        lock_holder,
    } = item;
    let lock_holder = lock_holder.as_ref();
    if let Some(cap) = cap {
        // Serializes this owner's capped writes so the usage read below cannot go stale.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("pyn:storage:{}", cap.owner))
            .execute(&mut **tx)
            .await
            .map_err(db)?;
    }

    if let Some(user) = lock_holder {
        let row = sqlx::query(
            "SELECT owner, expires_at, expires_at > COALESCE($3, clock_timestamp()) AS live
             FROM locks WHERE repo = $1 AND path = $2 FOR UPDATE",
        )
        .bind(repo.as_str())
        .bind(revision.path.as_str())
        .bind(lease_now)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
        match row {
            Some(r) if r.get::<bool, _>("live") => {
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
    let line: i64 = sqlx::query_scalar(
        "SELECT COALESCE(max(id), 0) FROM revisions WHERE repo = $1 AND path = $2",
    )
    .bind(repo.as_str())
    .bind(revision.path.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    let base = at_branch.get(revision.path.as_str()).copied().unwrap_or(0);
    if base != expected {
        return Err(PynError::StaleBase {
            path: revision.path,
            expected: expected_head,
            actual: (base > 0).then_some(RevisionId(base as u64)),
        });
    }
    let previous: Option<RevisionHash> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT hash FROM revisions WHERE repo = $1 AND path = $2 AND id = $3",
    )
    .bind(repo.as_str())
    .bind(revision.path.as_str())
    .bind(line)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?
    .flatten()
    .map(RevisionHash::new);
    let (path, size, mode, content) = (
        revision.path.clone(),
        revision.size,
        revision.mode,
        revision.content,
    );
    let created = revision.create(
        RevisionId((line + 1) as u64),
        previous,
        expected_head,
        commit.1,
    );
    let person = created.person.as_ref().expect("a new revision is v2");
    // The repository's writes are serialized by `next_commit`; the guard still refuses a line that moved.
    let inserted = sqlx::query(
        "INSERT INTO revisions (repo, path, id, content, author, message, created_at, restored_from, mode, size, changeset,
                                commit_no, hash, previous_hash, commit_id, file_mode, author_name, author_email, author_offset, hash_version, base_id, deleted)
         SELECT $1, $2, $3, $4, $5, $6, $7, $9::bigint, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22::bigint, $23
         WHERE COALESCE((SELECT max(id) FROM revisions WHERE repo = $1 AND path = $2), 0) = $8
         RETURNING id",
    )
    .bind(repo.as_str())
    .bind(path.as_str())
    .bind(line + 1)
    .bind(content.to_hex())
    .bind(created.author.as_str())
    .bind(&created.message)
    .bind(created.created_at)
    .bind(line)
    .bind(created.restored_from.map(|r| r.0 as i64))
    .bind(mode_str(mode))
    .bind(size as i64)
    .bind(&created.changeset)
    .bind(commit.0 as i64)
    .bind(created.hash.as_str())
    .bind(created.previous.as_ref().map(RevisionHash::as_str))
    .bind(created.commit.to_hex())
    .bind(created.file_mode.as_str())
    .bind(&person.name)
    .bind(&person.email)
    .bind(person.offset_minutes)
    .bind(created.hash_version as i32)
    .bind(created.base.map(|b| b.0 as i64))
    .bind(created.deleted)
    .fetch_optional(&mut **tx)
    .await;

    let stale = |actual| PynError::StaleBase {
        path: path.clone(),
        expected: expected_head,
        actual,
    };
    match inserted {
        Ok(Some(_)) => {}
        Ok(None) => {
            let head: Option<i64> =
                sqlx::query_scalar("SELECT max(id) FROM revisions WHERE repo = $1 AND path = $2")
                    .bind(repo.as_str())
                    .bind(path.as_str())
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(db)?;
            return Err(stale(head.map(|h| RevisionId(h as u64))));
        }
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some(UNIQUE_VIOLATION) => {
            return Err(stale(None));
        }
        Err(e) => return Err(db(e)),
    }

    if let Some(cap) = cap {
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM revisions
                            WHERE repo = $1 AND content = $2 AND size > 0 AND NOT (path = $4 AND id = $3))",
        )
        .bind(repo.as_str())
        .bind(content.to_hex())
        .bind(line + 1)
        .bind(path.as_str())
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?;
        if !known {
            let total = owner_stored(tx, &cap.owner).await?;
            if total as u64 > cap.max_bytes {
                return Err(PynError::StorageLimitReached {
                    owner: cap.owner.to_string(),
                    limit: cap.max_bytes,
                    used: total as u64 - size,
                });
            }
        }
    }
    if lock_holder.is_some() {
        sqlx::query("DELETE FROM locks WHERE repo = $1 AND path = $2")
            .bind(repo.as_str())
            .bind(path.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db)?;
    }
    Ok(created)
}

/// The commit that appending `items` after `parent` (a commit id) makes, with the trees it adds.
async fn draft(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    parent: Option<&ObjectId>,
    items: &[ChangesetItem],
) -> Result<CommitDraft> {
    let changes = items.iter().map(|i| i.revision.tree_change()).collect();
    let first = &items[0].revision;
    draft_with(
        tx,
        repo,
        parent,
        parent.copied().into_iter().collect(),
        changes,
        (&first.person, &first.message, first.created_at),
        first.changeset.clone(),
    )
    .await
}

/// The commit with `parents` over the tree of commit `parent` with `changes` put in, with the trees it adds.
async fn draft_with(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    parent: Option<&ObjectId>,
    parents: Vec<ObjectId>,
    changes: Vec<TreeChange>,
    (author, message, at): (&Person, &str, DateTime<Utc>),
    changeset: Option<String>,
) -> Result<CommitDraft> {
    let parent_tree = match parent {
        Some(id) => {
            let tree: String =
                sqlx::query_scalar("SELECT tree FROM commits WHERE repo = $1 AND id = $2")
                    .bind(repo.as_str())
                    .bind(id.to_hex())
                    .fetch_one(&mut **tx)
                    .await
                    .map_err(db)?;
            Some(object_id(&tree, "commit tree")?)
        }
        None => None,
    };
    let mut edit = TreeEdit::new(parent_tree, changes);
    while let Some(id) = edit.wanted() {
        let body: Vec<u8> =
            sqlx::query_scalar("SELECT body FROM trees WHERE repo = $1 AND id = $2")
                .bind(repo.as_str())
                .bind(id.to_hex())
                .fetch_one(&mut **tx)
                .await
                .map_err(db)?;
        edit.supply(id, &body)?;
    }
    draft_commit(parents, edit.finish()?, author, message, at, changeset)
}

/// Writes the draft's trees, its commit row as commit number `seq` and the pointers of the revisions it made, and moves
/// `branch` to it.
async fn store_commit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    seq: u64,
    branch: &str,
    draft: &CommitDraft,
    revisions: &[Revision],
) -> Result<()> {
    for (id, body) in &draft.trees {
        sqlx::query(
            "INSERT INTO trees (repo, id, body) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(repo.as_str())
        .bind(id.to_hex())
        .bind(body)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    }
    insert_commit(tx, repo, seq, &draft.record).await?;
    for r in revisions {
        sqlx::query(
            "INSERT INTO commit_pointers (repo, commit_id, path, revision) VALUES ($1, $2, $3, $4)",
        )
        .bind(repo.as_str())
        .bind(draft.record.id.to_hex())
        .bind(r.path.as_str())
        .bind(r.id.0 as i64)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    }
    sqlx::query("UPDATE branches SET head = $3 WHERE repo = $1 AND name = $2")
        .bind(repo.as_str())
        .bind(branch)
        .bind(draft.record.id.to_hex())
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    Ok(())
}

async fn insert_commit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    seq: u64,
    c: &Commit,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO commits (repo, seq, id, parent, tree, author_name, author_email, author_offset,
                              committer_name, committer_email, committer_offset, created_at, message, signature, changeset, parents, raw, authored_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
    )
    .bind(repo.as_str())
    .bind(seq as i64)
    .bind(c.id.to_hex())
    .bind(c.parents.first().map(ObjectId::to_hex))
    .bind(c.tree.to_hex())
    .bind(&c.author.name)
    .bind(&c.author.email)
    .bind(c.author.offset_minutes)
    .bind(&c.committer.name)
    .bind(&c.committer.email)
    .bind(c.committer.offset_minutes)
    .bind(c.created_at)
    .bind(&c.message)
    .bind(&c.signature)
    .bind(&c.changeset)
    .bind(c.parents.iter().map(ObjectId::to_hex).collect::<Vec<_>>())
    .bind(&c.raw)
    .bind(c.authored_at)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

const COMMIT_COLUMNS: &str = "id, parents, tree, author_name, author_email, author_offset, committer_name, committer_email, committer_offset, created_at, message, signature, changeset, raw, authored_at";

/// A commit record without its revisions.
fn commit_from(row: &PgRow) -> Result<Commit> {
    Ok(Commit {
        id: object_id(&row.get::<String, _>("id"), "commit id")?,
        parents: row
            .get::<Vec<String>, _>("parents")
            .iter()
            .map(|p| object_id(p, "commit parent"))
            .collect::<Result<_>>()?,
        tree: object_id(&row.get::<String, _>("tree"), "commit tree")?,
        author: Person {
            name: row.get("author_name"),
            email: row.get("author_email"),
            offset_minutes: row.get("author_offset"),
        },
        committer: Person {
            name: row.get("committer_name"),
            email: row.get("committer_email"),
            offset_minutes: row.get("committer_offset"),
        },
        authored_at: row.get("authored_at"),
        created_at: row.get("created_at"),
        message: row.get("message"),
        signature: row.get("signature"),
        changeset: row.get("changeset"),
        raw: row.get("raw"),
        revisions: Vec::new(),
    })
}

/// Takes the repository's next commit number; the row lock serializes the repository's writes until the transaction ends.
async fn next_commit(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, repo: &RepoId) -> Result<u64> {
    let n: i64 = sqlx::query_scalar(
        "INSERT INTO repo_commits (repo, last) VALUES ($1, 1)
         ON CONFLICT (repo) DO UPDATE SET last = repo_commits.last + 1
         RETURNING last",
    )
    .bind(repo.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    Ok(n as u64)
}

/// Bytes of distinct content across the owner's repositories.
async fn unreferenced<'e>(
    exec: impl sqlx::Executor<'e, Database = sqlx::Postgres>,
    repo: &RepoId,
) -> Result<i64> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(o.size), 0)::bigint FROM repo_objects o
         WHERE o.repo = $1 AND NOT o.assembled
           AND NOT EXISTS (SELECT 1 FROM revisions r WHERE r.repo = o.repo AND r.content = o.id)",
    )
    .bind(repo.as_str())
    .fetch_one(exec)
    .await
    .map_err(db)
}

async fn insert_upload(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    id: &ObjectId,
    size: u64,
    cap: u64,
) -> Result<()> {
    let inserted = sqlx::query(
        "INSERT INTO repo_objects (repo, id, size) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(repo.as_str())
    .bind(id.to_hex())
    .bind(size as i64)
    .execute(&mut **tx)
    .await
    .map_err(db)?
    .rows_affected();
    if inserted == 1 {
        let used = unreferenced(&mut **tx, repo).await? as u64;
        if used > cap {
            return Err(PynError::StorageLimitReached {
                owner: repo.to_string(),
                limit: cap,
                used: used - size,
            });
        }
    }
    Ok(())
}

async fn owner_stored(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: &UserId,
) -> Result<i64> {
    sqlx::query_scalar(
        "SELECT COALESCE(SUM(size), 0)::bigint FROM (
             SELECT DISTINCT r.repo, r.content, r.size FROM revisions r
             JOIN repositories p ON p.id = r.repo WHERE p.owner = $1) t",
    )
    .bind(owner.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(db)
}

fn branch_from(row: &PgRow) -> Result<Branch> {
    Ok(Branch {
        name: row.get("name"),
        head: row
            .get::<Option<String>, _>("head")
            .map(|h| object_id(&h, "branch head"))
            .transpose()?,
        default: row.get("is_default"),
        created_at: row.get("created_at"),
        created_by: UserId::new(row.get::<String, _>("created_by")),
    })
}

/// A repository written to without being registered still has its default branch.
async fn ensure_default_branch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    by: &UserId,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO branches (repo, name, head, is_default, created_at, created_by)
         VALUES ($1, $2, NULL, true, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(repo.as_str())
    .bind(DEFAULT_BRANCH)
    .bind(now)
    .bind(by.as_str())
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

/// The revision number at `commit` of each path (of `only` when given) that has one: the nearest pointer along the
/// first-parent chain.
async fn pointers<'e, E: sqlx::Executor<'e, Database = Postgres>>(
    exec: E,
    repo: &RepoId,
    commit: &ObjectId,
    only: Option<&[String]>,
) -> Result<Vec<(String, i64)>> {
    sqlx::query_as(AssertSqlSafe(format!(
        "{CHAIN}
         SELECT DISTINCT ON (p.path) p.path, p.revision
         FROM chain JOIN commit_pointers p ON p.repo = $1 AND p.commit_id = chain.id
         WHERE $3::text[] IS NULL OR p.path = ANY($3)
         ORDER BY p.path, chain.depth"
    )))
    .bind(repo.as_str())
    .bind(commit.to_hex())
    .bind(only)
    .fetch_all(exec)
    .await
    .map_err(db)
}

async fn commit_known(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    id: &ObjectId,
) -> Result<bool> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM commits WHERE repo = $1 AND id = $2)")
        .bind(repo.as_str())
        .bind(id.to_hex())
        .fetch_one(&mut **tx)
        .await
        .map_err(db)
}

async fn is_ancestor_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    repo: &RepoId,
    ancestor: &ObjectId,
    descendant: &ObjectId,
) -> Result<bool> {
    sqlx::query_scalar(AssertSqlSafe(format!(
        "{ANCESTORS} SELECT EXISTS (SELECT 1 FROM anc WHERE id = $3)"
    )))
    .bind(descendant.to_hex())
    .bind(repo.as_str())
    .bind(ancestor.to_hex())
    .fetch_one(&mut **tx)
    .await
    .map_err(db)
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some(UNIQUE_VIOLATION))
}

impl PgMetadataStore {
    /// The revisions of the given commits, ordered by path.
    async fn revisions_of(&self, repo: &RepoId, commits: &[String]) -> Result<Vec<Revision>> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS} FROM revisions WHERE repo = $1 AND commit_id = ANY($2)
             ORDER BY commit_no DESC, path COLLATE \"C\""
        )))
        .bind(repo.as_str())
        .bind(commits)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(revision_from).collect()
    }

    /// Connects and applies migrations, creating the database first when `create_database` is set.
    pub async fn connect(url: &str, create_database: bool) -> Result<Self> {
        if create_database && !Postgres::database_exists(url).await.map_err(db)? {
            // Another copy starting at the same time may create it first.
            if let Err(e) = Postgres::create_database(url).await
                && !Postgres::database_exists(url).await.map_err(db)?
            {
                return Err(db(e));
            }
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
        hashing::backfill(&pool).await?;
        Ok(Self {
            pool,
            scratch_schema: None,
            time: TimeSource::Database,
        })
    }

    /// Selects the clock for lease and rate-limit comparisons.
    pub fn with_time_source(mut self, time: TimeSource) -> Self {
        self.time = time;
        self
    }

    /// The `now` to bind for `COALESCE($n, now())`: `None` defers to the database.
    fn bound_now(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (self.time == TimeSource::Caller).then_some(now)
    }

    /// Lease time inside `tx`: the database's clock, or `now`.
    async fn lease_now(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        now: DateTime<Utc>,
    ) -> Result<DateTime<Utc>> {
        if self.time == TimeSource::Caller {
            return Ok(now);
        }
        sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut **tx)
            .await
            .map_err(db)
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
        store.time = TimeSource::Caller;
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
            let lease = self.lease_now(&mut tx, now).await?;
            let expires_at = lease + (expires_at - now);
            let live: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM locks WHERE repo = $1 AND path = $2 AND expires_at > $3)",
            )
            .bind(repo.as_str())
            .bind(path.as_str())
            .bind(lease)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            if !live {
                let held: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM locks WHERE repo = $1 AND owner = $2 AND expires_at > $3",
                )
                .bind(repo.as_str())
                .bind(owner.as_str())
                .bind(lease)
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
                .bind(lease)
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
            "DELETE FROM locks WHERE repo = $1 AND path = $2 AND owner = $3 AND expires_at > COALESCE($4, now())",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .bind(owner.as_str())
        .bind(self.bound_now(now))
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
             WHERE repo = $1 AND path = $2 AND expires_at > COALESCE($3, now())",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .bind(self.bound_now(now))
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
            "DELETE FROM locks WHERE repo = $1 AND path = $2 AND expires_at > COALESCE($3, now())
             RETURNING path, owner, acquired_at, expires_at",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .bind(self.bound_now(now))
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(lock_from).transpose()
    }

    async fn list_locks(&self, repo: &RepoId, now: DateTime<Utc>) -> Result<Vec<Lock>> {
        let rows = sqlx::query(
            "SELECT path, owner, acquired_at, expires_at FROM locks
             WHERE repo = $1 AND expires_at > COALESCE($2, now()) ORDER BY path",
        )
        .bind(repo.as_str())
        .bind(self.bound_now(now))
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
             WHERE owner = $1 AND expires_at > COALESCE($2, now()) ORDER BY repo COLLATE \"C\", path COLLATE \"C\"",
        )
        .bind(owner.as_str())
        .bind(self.bound_now(now))
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|r| Ok((RepoId::new(r.get::<String, _>("repo")), lock_from(r)?)))
            .collect()
    }

    async fn head_revision(&self, repo: &RepoId, path: &RepoPath) -> Result<Option<Revision>> {
        let row = sqlx::query(
            "SELECT id, path, content, author, message, created_at, restored_from, mode, changeset, commit_no, hash, previous_hash, content_v1, commit_id, file_mode, author_name, author_email, author_offset, hash_version, base_id, deleted FROM revisions
             WHERE repo = $1 AND path = $2 ORDER BY id DESC LIMIT 1",
        )
        .bind(repo.as_str())
        .bind(path.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(revision_from).transpose()
    }

    async fn commit_changeset_on(
        &self,
        repo: &RepoId,
        branch: &str,
        items: Vec<ChangesetItem>,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Vec<Revision>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let seq = next_commit(&mut tx, repo).await?;
        if branch == DEFAULT_BRANCH {
            ensure_default_branch(&mut tx, repo, &items[0].revision.author, now).await?;
        }
        let head: Option<Option<String>> =
            sqlx::query_scalar("SELECT head FROM branches WHERE repo = $1 AND name = $2")
                .bind(repo.as_str())
                .bind(branch)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let head = head
            .ok_or_else(|| PynError::BranchNotFound(branch.to_string()))?
            .map(|h| object_id(&h, "branch head"))
            .transpose()?;
        let paths: Vec<String> = items
            .iter()
            .map(|i| i.revision.path.as_str().to_string())
            .collect();
        let at_branch: HashMap<String, i64> = match &head {
            Some(h) => pointers(&mut *tx, repo, h, Some(&paths))
                .await?
                .into_iter()
                .collect(),
            None => HashMap::new(),
        };
        let draft = draft(&mut tx, repo, head.as_ref(), &items).await?;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let commit = (seq, draft.record.id);
            out.push(
                append_revision(
                    &mut tx,
                    repo,
                    item,
                    self.bound_now(now),
                    cap.as_ref(),
                    commit,
                    &at_branch,
                )
                .await?,
            );
        }
        store_commit(&mut tx, repo, seq, branch, &draft, &out).await?;
        tx.commit().await.map_err(db)?;
        Ok(out)
    }

    async fn push_commits_on(
        &self,
        repo: &RepoId,
        branch: &str,
        commits: Vec<PushedCommit>,
        pusher: &UserId,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Vec<Revision>> {
        if commits.is_empty() {
            return Ok(Vec::new());
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let mut seq = next_commit(&mut tx, repo).await?;
        if branch == DEFAULT_BRANCH {
            ensure_default_branch(&mut tx, repo, pusher, now).await?;
        }
        let head: Option<Option<String>> =
            sqlx::query_scalar("SELECT head FROM branches WHERE repo = $1 AND name = $2")
                .bind(repo.as_str())
                .bind(branch)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let head = head
            .ok_or_else(|| PynError::BranchNotFound(branch.to_string()))?
            .map(|h| object_id(&h, "branch head"))
            .transpose()?;
        let mut tip = head;
        for (i, c) in commits.iter().enumerate() {
            let first = c.record.parents.first().copied();
            let linked = match (i, head) {
                (0, Some(h)) => match first {
                    Some(p) => p == h || is_ancestor_in(&mut tx, repo, &p, &h).await?,
                    None => false,
                },
                (0, None) => first.is_none(),
                _ => first == tip,
            };
            if !linked {
                return Err(PynError::NotFastForward(branch.to_string()));
            }
            for other in c.record.parents.iter().skip(1) {
                if !commit_known(&mut tx, repo, other).await? {
                    return Err(PynError::PushInvalid(format!(
                        "commit {} merges {other}, which is not in the repository",
                        c.record.id
                    )));
                }
            }
            if commit_known(&mut tx, repo, &c.record.id).await? {
                if i == 0 {
                    return Err(PynError::NotFastForward(branch.to_string()));
                }
                return Err(PynError::PushInvalid(format!(
                    "commit {} is already in the repository",
                    c.record.id
                )));
            }
            tip = Some(c.record.id);
        }

        let mut last_use = BTreeMap::new();
        for (i, c) in commits.iter().enumerate() {
            for item in c
                .items
                .iter()
                .filter(|i| i.mode == Mode::Exclusive && !c.unlocked.contains(&i.path))
            {
                last_use.insert(item.path.as_str().to_string(), i);
            }
        }
        for path in last_use.keys() {
            let row = sqlx::query(
                "SELECT owner, expires_at, expires_at > COALESCE($3, clock_timestamp()) AS live
                 FROM locks WHERE repo = $1 AND path = $2 FOR UPDATE",
            )
            .bind(repo.as_str())
            .bind(path)
            .bind(self.bound_now(now))
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
            match row {
                Some(r) if r.get::<bool, _>("live") => {
                    let owner: String = r.get("owner");
                    if owner != pusher.as_str() {
                        return Err(PynError::LockHeld {
                            path: RepoPath::from_stored(path.clone())?,
                            owner: UserId::new(owner),
                            expires_at: r.get("expires_at"),
                        });
                    }
                }
                _ => return Err(PynError::LockRequired(RepoPath::from_stored(path.clone())?)),
            }
        }

        let paths: Vec<String> = commits
            .iter()
            .flat_map(|c| c.items.iter().map(|i| i.path.as_str().to_string()))
            .collect();
        let start = commits[0].record.parents.first().copied();
        let mut at_branch: HashMap<String, i64> = match &start {
            Some(p) => pointers(&mut *tx, repo, p, Some(&paths))
                .await?
                .into_iter()
                .collect(),
            None => HashMap::new(),
        };
        let mut out = Vec::new();
        for (i, c) in commits.into_iter().enumerate() {
            if i > 0 {
                seq = next_commit(&mut tx, repo).await?;
            }
            let mut revs = Vec::with_capacity(c.items.len());
            for revision in c.items {
                let item = ChangesetItem {
                    expected_head: at_branch
                        .get(revision.path.as_str())
                        .map(|r| RevisionId(*r as u64)),
                    revision,
                    lock_holder: None,
                };
                let commit = (seq, c.record.id);
                revs.push(
                    append_revision(
                        &mut tx,
                        repo,
                        item,
                        self.bound_now(now),
                        cap.as_ref(),
                        commit,
                        &at_branch,
                    )
                    .await?,
                );
            }
            for r in &revs {
                at_branch.insert(r.path.as_str().to_string(), r.id.0 as i64);
                if last_use.get(r.path.as_str()) == Some(&i) {
                    sqlx::query("DELETE FROM locks WHERE repo = $1 AND path = $2")
                        .bind(repo.as_str())
                        .bind(r.path.as_str())
                        .execute(&mut *tx)
                        .await
                        .map_err(db)?;
                }
            }
            for r in &c.adopted {
                at_branch.insert(r.path.as_str().to_string(), r.id.0 as i64);
            }
            let pointed: Vec<Revision> = c.adopted.iter().chain(&revs).cloned().collect();
            let draft = CommitDraft {
                record: c.record,
                trees: c.trees,
            };
            store_commit(&mut tx, repo, seq, branch, &draft, &pointed).await?;
            out.extend(revs);
        }
        if let (Some(h), Some(t)) = (head, tip)
            && !is_ancestor_in(&mut tx, repo, &h, &t).await?
        {
            return Err(PynError::NotFastForward(branch.to_string()));
        }
        tx.commit().await.map_err(db)?;
        Ok(out)
    }

    async fn commit_merge_on(
        &self,
        repo: &RepoId,
        branch: &str,
        merge: MergeWrite,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<MergeWritten> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let seq = next_commit(&mut tx, repo).await?;
        let head: Option<Option<String>> = sqlx::query_scalar(
            "SELECT head FROM branches WHERE repo = $1 AND name = $2 FOR UPDATE",
        )
        .bind(repo.as_str())
        .bind(branch)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let head = head
            .ok_or_else(|| PynError::BranchNotFound(branch.to_string()))?
            .map(|h| object_id(&h, "branch head"))
            .transpose()?;
        if head != Some(merge.expected_head) {
            return Err(PynError::MergeStale(branch.to_string()));
        }
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM commits WHERE repo = $1 AND id = $2)")
                .bind(repo.as_str())
                .bind(merge.other.to_hex())
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        if !known {
            return Err(PynError::CommitNotFound(merge.other.to_string()));
        }
        let paths: Vec<String> = merge
            .items
            .iter()
            .map(|i| i.revision.path.as_str().to_string())
            .collect();
        let at_branch: HashMap<String, i64> =
            pointers(&mut *tx, repo, &merge.expected_head, Some(&paths))
                .await?
                .into_iter()
                .collect();
        let changes = merge
            .adopted
            .iter()
            .map(|r| TreeChange {
                path: r.path.clone(),
                mode: r.file_mode,
                id: r.content,
                removed: false,
            })
            .chain(merge.items.iter().map(|i| i.revision.tree_change()))
            .collect();
        let draft = draft_with(
            &mut tx,
            repo,
            Some(&merge.expected_head),
            vec![merge.expected_head, merge.other],
            changes,
            (&merge.author, &merge.message, now),
            None,
        )
        .await?;
        let mut made = Vec::with_capacity(merge.items.len());
        for item in merge.items {
            let commit = (seq, draft.record.id);
            made.push(
                append_revision(
                    &mut tx,
                    repo,
                    item,
                    self.bound_now(now),
                    cap.as_ref(),
                    commit,
                    &at_branch,
                )
                .await?,
            );
        }
        let pointed: Vec<Revision> = merge.adopted.iter().chain(&made).cloned().collect();
        store_commit(&mut tx, repo, seq, branch, &draft, &pointed).await?;
        tx.commit().await.map_err(db)?;
        Ok(MergeWritten {
            commit: draft.record.id,
            revisions: made,
        })
    }

    async fn fast_forward_branch(
        &self,
        repo: &RepoId,
        branch: &str,
        from: Option<&ObjectId>,
        to: &ObjectId,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        // The repository's writes are serialized on this row, as `next_commit` does.
        sqlx::query("SELECT last FROM repo_commits WHERE repo = $1 FOR UPDATE")
            .bind(repo.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(db)?;
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM commits WHERE repo = $1 AND id = $2)")
                .bind(repo.as_str())
                .bind(to.to_hex())
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        if !known {
            return Err(PynError::CommitNotFound(to.to_string()));
        }
        let moved = sqlx::query(
            "UPDATE branches SET head = $3 WHERE repo = $1 AND name = $2 AND head IS NOT DISTINCT FROM $4",
        )
        .bind(repo.as_str())
        .bind(branch)
        .bind(to.to_hex())
        .bind(from.map(ObjectId::to_hex))
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected();
        if moved == 0 {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM branches WHERE repo = $1 AND name = $2)",
            )
            .bind(repo.as_str())
            .bind(branch)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            return Err(if exists {
                PynError::MergeStale(branch.to_string())
            } else {
                PynError::BranchNotFound(branch.to_string())
            });
        }
        tx.commit().await.map_err(db)
    }

    async fn merge_bases(
        &self,
        repo: &RepoId,
        a: &ObjectId,
        b: &ObjectId,
    ) -> Result<Vec<ObjectId>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "WITH RECURSIVE
             from_a(id) AS (
                 SELECT $1::text
                 UNION
                 SELECT p FROM from_a JOIN commits c ON c.repo = $3 AND c.id = from_a.id CROSS JOIN LATERAL unnest(c.parents) AS p
             ),
             from_b(id) AS (
                 SELECT $2::text
                 UNION
                 SELECT p FROM from_b JOIN commits c ON c.repo = $3 AND c.id = from_b.id CROSS JOIN LATERAL unnest(c.parents) AS p
             ),
             common(id) AS (SELECT id FROM from_a INTERSECT SELECT id FROM from_b),
             shadowed(id) AS (
                 SELECT p FROM common JOIN commits c ON c.repo = $3 AND c.id = common.id CROSS JOIN LATERAL unnest(c.parents) AS p
                 UNION
                 SELECT p FROM shadowed JOIN commits c ON c.repo = $3 AND c.id = shadowed.id CROSS JOIN LATERAL unnest(c.parents) AS p
             )
             SELECT id FROM common WHERE id NOT IN (SELECT id FROM shadowed) ORDER BY id COLLATE \"C\"",
        )
        .bind(a.to_hex())
        .bind(b.to_hex())
        .bind(repo.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        ids.iter().map(|i| object_id(i, "commit id")).collect()
    }

    async fn list_branches(&self, repo: &RepoId) -> Result<Vec<Branch>> {
        let rows = sqlx::query(
            "SELECT name, head, is_default, created_at, created_by FROM branches
             WHERE repo = $1 ORDER BY is_default DESC, name COLLATE \"C\"",
        )
        .bind(repo.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(branch_from).collect()
    }

    async fn get_branch(&self, repo: &RepoId, name: &str) -> Result<Option<Branch>> {
        let row = sqlx::query(
            "SELECT name, head, is_default, created_at, created_by FROM branches
             WHERE repo = $1 AND name = $2",
        )
        .bind(repo.as_str())
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(branch_from).transpose()
    }

    async fn create_branch(
        &self,
        repo: &RepoId,
        name: &str,
        at: &ObjectId,
        by: &UserId,
        now: DateTime<Utc>,
    ) -> Result<Branch> {
        let known: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM commits WHERE repo = $1 AND id = $2)")
                .bind(repo.as_str())
                .bind(at.to_hex())
                .fetch_one(&self.pool)
                .await
                .map_err(db)?;
        if !known {
            return Err(PynError::CommitNotFound(at.to_string()));
        }
        let inserted = sqlx::query(
            "INSERT INTO branches (repo, name, head, is_default, created_at, created_by)
             VALUES ($1, $2, $3, false, $4, $5)",
        )
        .bind(repo.as_str())
        .bind(name)
        .bind(at.to_hex())
        .bind(now)
        .bind(by.as_str())
        .execute(&self.pool)
        .await;
        match inserted {
            Ok(_) => Ok(Branch {
                name: name.to_string(),
                head: Some(*at),
                default: false,
                created_at: now,
                created_by: by.clone(),
            }),
            Err(e) if is_unique_violation(&e) => Err(PynError::BranchExists(name.to_string())),
            Err(e) => Err(db(e)),
        }
    }

    async fn delete_branch(
        &self,
        repo: &RepoId,
        name: &str,
        expected: Option<&ObjectId>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row: Option<(Option<String>, bool)> = sqlx::query_as(
            "SELECT head, is_default FROM branches WHERE repo = $1 AND name = $2 FOR UPDATE",
        )
        .bind(repo.as_str())
        .bind(name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let (head, default) = row.ok_or_else(|| PynError::BranchNotFound(name.to_string()))?;
        if default {
            return Err(PynError::InvalidRequest(
                "the default branch cannot be deleted".into(),
            ));
        }
        if expected.is_some_and(|e| head.as_deref() != Some(e.to_hex().as_str())) {
            return Err(PynError::InvalidRequest(format!(
                "{name} moved while it was being deleted"
            )));
        }
        sqlx::query("DELETE FROM branches WHERE repo = $1 AND name = $2")
            .bind(repo.as_str())
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)
    }

    async fn rename_branch(&self, repo: &RepoId, from: &str, to: &str) -> Result<Branch> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let names: Vec<(String, bool)> =
            sqlx::query_as("SELECT name, is_default FROM branches WHERE repo = $1 FOR UPDATE")
                .bind(repo.as_str())
                .fetch_all(&mut *tx)
                .await
                .map_err(db)?;
        let (_, default) = names
            .iter()
            .find(|(n, _)| n == from)
            .ok_or_else(|| PynError::BranchNotFound(from.to_string()))?;
        if *default {
            return Err(PynError::InvalidRequest(
                "the default branch cannot be renamed".into(),
            ));
        }
        if names.iter().any(|(n, _)| n != from && conflicts(n, to)) {
            return Err(PynError::BranchExists(to.to_string()));
        }
        let row = sqlx::query(
            "UPDATE branches SET name = $3 WHERE repo = $1 AND name = $2
             RETURNING name, head, is_default, created_at, created_by",
        )
        .bind(repo.as_str())
        .bind(from)
        .bind(to)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        branch_from(&row)
    }

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> Result<Option<Revision>> {
        let row = sqlx::query(
            "SELECT id, path, content, author, message, created_at, restored_from, mode, changeset, commit_no, hash, previous_hash, content_v1, commit_id, file_mode, author_name, author_email, author_offset, hash_version, base_id, deleted FROM revisions
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

    async fn get_commit(&self, repo: &RepoId, id: &ObjectId) -> Result<Option<Commit>> {
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {COMMIT_COLUMNS} FROM commits WHERE repo = $1 AND id = $2"
        )))
        .bind(repo.as_str())
        .bind(id.to_hex())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        let Some(row) = row else { return Ok(None) };
        let mut commit = commit_from(&row)?;
        commit.revisions = self.revisions_of(repo, &[id.to_hex()]).await?;
        Ok(Some(commit))
    }

    async fn commits_by_prefix(
        &self,
        repo: &RepoId,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<ObjectId>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM commits WHERE repo = $1 AND left(id, length($2)) = $2 ORDER BY seq LIMIT $3",
        )
        .bind(repo.as_str())
        .bind(prefix)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        ids.iter().map(|i| object_id(i, "commit id")).collect()
    }

    async fn commits(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        limit: usize,
    ) -> Result<Vec<Commit>> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "{ANCESTORS}
             SELECT {COMMIT_COLUMNS} FROM commits
             WHERE repo = $2 AND ($1::text IS NULL OR id IN (SELECT id FROM anc))
               AND ($3::text IS NULL OR seq < (SELECT seq FROM commits WHERE repo = $2 AND id = $3))
             ORDER BY seq DESC LIMIT $4"
        )))
        .bind(tip.map(ObjectId::to_hex))
        .bind(repo.as_str())
        .bind(before.map(ObjectId::to_hex))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let mut commits = rows.iter().map(commit_from).collect::<Result<Vec<_>>>()?;
        let ids: Vec<String> = commits.iter().map(|c| c.id.to_hex()).collect();
        let mut revisions = self.revisions_of(repo, &ids).await?;
        for c in &mut commits {
            let (mine, rest): (Vec<_>, Vec<_>) =
                revisions.into_iter().partition(|r| r.commit == c.id);
            c.revisions = mine;
            revisions = rest;
        }
        Ok(commits)
    }

    async fn missing_commits(
        &self,
        repo: &RepoId,
        tips: &[ObjectId],
        haves: &[ObjectId],
        after: Option<&ObjectId>,
        limit: usize,
        max_examined: usize,
    ) -> Result<Vec<Commit>> {
        let floor: i64 = match after {
            Some(a) => sqlx::query_scalar("SELECT seq FROM commits WHERE repo = $1 AND id = $2")
                .bind(repo.as_str())
                .bind(a.to_hex())
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?
                .ok_or_else(|| PynError::CommitNotFound(a.to_string()))?,
            None => -1,
        };
        let hex = |ids: &[ObjectId]| ids.iter().map(ObjectId::to_hex).collect::<Vec<_>>();
        let rows = sqlx::query(AssertSqlSafe(format!(
            "WITH RECURSIVE
             want(id) AS (
                 SELECT id FROM commits WHERE repo = $1 AND id = ANY($2) AND seq > $4
                 UNION
                 SELECT p.id FROM want JOIN commits c ON c.repo = $1 AND c.id = want.id
                     CROSS JOIN LATERAL unnest(c.parents) AS pid
                     JOIN commits p ON p.repo = $1 AND p.id = pid AND p.seq > $4
             ),
             have(id) AS (
                 SELECT id FROM commits WHERE repo = $1 AND id = ANY($3) AND seq > $4
                 UNION
                 SELECT p.id FROM have JOIN commits c ON c.repo = $1 AND c.id = have.id
                     CROSS JOIN LATERAL unnest(c.parents) AS pid
                     JOIN commits p ON p.repo = $1 AND p.id = pid AND p.seq > $4
             ),
             wanted AS MATERIALIZED (SELECT id FROM want LIMIT $5 + 1),
             owned AS MATERIALIZED (SELECT id FROM have LIMIT $5 + 1),
             page AS (
                 SELECT seq, {COMMIT_COLUMNS} FROM commits
                 WHERE repo = $1 AND id IN (SELECT id FROM wanted)
                   AND id NOT IN (SELECT id FROM owned)
                 ORDER BY seq LIMIT $6
             )
             SELECT (SELECT count(*) FROM wanted) > $5 OR (SELECT count(*) FROM owned) > $5 AS over, page.*
             FROM (SELECT 1) one LEFT JOIN page ON true ORDER BY page.seq"
        )))
        .bind(repo.as_str())
        .bind(hex(tips))
        .bind(hex(haves))
        .bind(floor)
        .bind(max_examined as i64)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        if rows.iter().any(|r| r.get::<bool, _>("over")) {
            return Err(PynError::InvalidRequest(format!(
                "the request would examine more than {max_examined} commits"
            )));
        }
        rows.iter()
            .filter(|r| r.get::<Option<String>, _>("id").is_some())
            .map(commit_from)
            .collect()
    }

    async fn is_ancestor(
        &self,
        repo: &RepoId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> Result<bool> {
        sqlx::query_scalar(AssertSqlSafe(format!(
            "{ANCESTORS} SELECT EXISTS (SELECT 1 FROM anc WHERE id = $3)"
        )))
        .bind(descendant.to_hex())
        .bind(repo.as_str())
        .bind(ancestor.to_hex())
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn revision_at(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        path: &RepoPath,
    ) -> Result<Option<Revision>> {
        let found = pointers(&self.pool, repo, commit, Some(&[path.as_str().to_string()])).await?;
        let Some((_, id)) = found.into_iter().next() else {
            return Ok(None);
        };
        self.get_revision(repo, path, RevisionId(id as u64)).await
    }

    async fn commit_pointers(&self, repo: &RepoId, commit: &ObjectId) -> Result<Vec<Revision>> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS} FROM revisions
             WHERE repo = $1 AND (path, id) IN
                 (SELECT path, revision FROM commit_pointers WHERE repo = $1 AND commit_id = $2)
             ORDER BY path COLLATE \"C\""
        )))
        .bind(repo.as_str())
        .bind(commit.to_hex())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(revision_from).collect()
    }

    async fn has_content(&self, repo: &RepoId, content: &ObjectId) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM revisions WHERE repo = $1 AND content = $2)",
        )
        .bind(repo.as_str())
        .bind(content.to_hex())
        .fetch_one(&self.pool)
        .await
        .map_err(db)
    }

    async fn record_upload(&self, repo: &RepoId, id: &ObjectId, size: u64, cap: u64) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        insert_upload(&mut tx, repo, id, size, cap).await?;
        tx.commit().await.map_err(db)
    }

    async fn record_assembled(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        chunks: &[ObjectId],
        cap: u64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let hex: Vec<String> = chunks.iter().map(|c| c.to_hex()).collect();
        let known: Vec<String> = sqlx::query_scalar(
            "SELECT c FROM unnest($2::text[]) AS c WHERE EXISTS
                 (SELECT 1 FROM repo_objects WHERE repo = $1 AND id = c)
              OR EXISTS (SELECT 1 FROM revisions WHERE repo = $1 AND content = c)",
        )
        .bind(repo.as_str())
        .bind(&hex)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        if let Some(c) = hex.iter().find(|c| !known.contains(c)) {
            return Err(PynError::ObjectMissing(c.clone()));
        }
        sqlx::query("UPDATE repo_objects SET assembled = true WHERE repo = $1 AND id = ANY($2)")
            .bind(repo.as_str())
            .bind(&hex)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        insert_upload(&mut tx, repo, id, size, cap).await?;
        tx.commit().await.map_err(db)
    }

    async fn unknown_objects(&self, repo: &RepoId, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let hex: Vec<String> = ids.iter().map(|c| c.to_hex()).collect();
        let known: Vec<String> = sqlx::query_scalar(
            "SELECT c FROM unnest($2::text[]) AS c WHERE EXISTS
                 (SELECT 1 FROM repo_objects WHERE repo = $1 AND id = c)
              OR EXISTS (SELECT 1 FROM revisions WHERE repo = $1 AND content = c)",
        )
        .bind(repo.as_str())
        .bind(&hex)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(ids
            .iter()
            .zip(&hex)
            .filter(|(_, h)| !known.contains(h))
            .map(|(id, _)| *id)
            .collect())
    }

    async fn unreferenced_bytes(&self, repo: &RepoId) -> Result<u64> {
        Ok(unreferenced(&self.pool, repo).await? as u64)
    }

    async fn get_tree(&self, repo: &RepoId, id: &ObjectId) -> Result<Option<Vec<u8>>> {
        sqlx::query_scalar("SELECT body FROM trees WHERE repo = $1 AND id = $2")
            .bind(repo.as_str())
            .bind(id.to_hex())
            .fetch_optional(&self.pool)
            .await
            .map_err(db)
    }

    async fn snapshot(&self, repo: &RepoId, commit: &ObjectId) -> Result<Vec<Revision>> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "{CHAIN}, latest AS (
                 SELECT DISTINCT ON (p.path COLLATE \"C\") p.path AS lpath, p.revision AS lrev
                 FROM chain JOIN commit_pointers p ON p.repo = $1 AND p.commit_id = chain.id
                 ORDER BY p.path COLLATE \"C\", chain.depth
             )
             SELECT {REVISION_COLUMNS} FROM revisions JOIN latest ON path = lpath AND id = lrev
             WHERE repo = $1 AND NOT deleted ORDER BY path COLLATE \"C\""
        )))
        .bind(repo.as_str())
        .bind(commit.to_hex())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(revision_from).collect()
    }

    async fn list_head_revisions(&self, repo: &RepoId) -> Result<Vec<Revision>> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (path) id, path, content, author, message, created_at, restored_from, mode, changeset, commit_no, hash, previous_hash, content_v1, commit_id, file_mode, author_name, author_email, author_offset, hash_version, base_id, deleted
             FROM revisions WHERE repo = $1 ORDER BY path, id DESC",
        )
        .bind(repo.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(revision_from).collect()
    }

    async fn history(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
    ) -> Result<Vec<Revision>> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "{ANCESTORS}
             SELECT {REVISION_COLUMNS} FROM revisions
             WHERE repo = $2 AND path = $3 AND ($1::text IS NULL OR commit_id IN (SELECT id FROM anc))
             ORDER BY id"
        )))
        .bind(within.map(ObjectId::to_hex))
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
        within: Option<&ObjectId>,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: usize,
    ) -> Result<Vec<Revision>> {
        // The glob cannot run in SQL, so scan keyset batches and filter until `limit` rows match.
        let batch = (limit.max(1) * 4).clamp(50, 1000) as i64;
        let mut cursor = before.cloned();
        let mut out = Vec::new();
        loop {
            let rows = sqlx::query(AssertSqlSafe(format!(
                "{ANCESTORS}
                 SELECT {REVISION_COLUMNS} FROM revisions
                 WHERE repo = $2
                   AND ($1::text IS NULL OR commit_id IN (SELECT id FROM anc))
                   AND ($3::timestamptz IS NULL
                        OR (created_at, path COLLATE \"C\", id) < ($3, $4::text COLLATE \"C\", $5::bigint))
                 ORDER BY created_at DESC, path COLLATE \"C\" DESC, id DESC
                 LIMIT $6"
            )))
            .bind(within.map(ObjectId::to_hex))
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

    async fn create_repo_capped(
        &self,
        repo: RepoRecord,
        max_repos: Option<u64>,
    ) -> Result<RepoRecord> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        if let Some(limit) = max_repos {
            // Serializes this owner's capped creations so the count below cannot go stale.
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("pyn:repos:{}", repo.owner))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            let held: i64 =
                sqlx::query_scalar("SELECT count(*) FROM repositories WHERE owner = $1")
                    .bind(repo.owner.as_str())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(db)?;
            let taken: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM repositories WHERE owner = $1 AND name = $2)",
            )
            .bind(repo.owner.as_str())
            .bind(&repo.name)
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
            if !taken && held as u64 >= limit {
                return Err(PynError::RepoLimitReached {
                    owner: repo.owner.to_string(),
                    limit,
                });
            }
        }
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
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {
                ensure_default_branch(&mut tx, &repo.id, &repo.owner, repo.created_at).await?;
                tx.commit().await.map_err(db)?;
                Ok(repo)
            }
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

    async fn owner_usage(&self, owner: &UserId) -> Result<Usage> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let repositories: i64 =
            sqlx::query_scalar("SELECT count(*) FROM repositories WHERE owner = $1")
                .bind(owner.as_str())
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        let stored_bytes = owner_stored(&mut tx, owner).await?;
        Ok(Usage {
            repositories: repositories as u64,
            members: None,
            stored_bytes: stored_bytes as u64,
        })
    }

    async fn repo_usage(&self, repo: &RepoId) -> Result<RepoUsage> {
        let row = sqlx::query(
            "SELECT (SELECT COALESCE(SUM(size), 0)::bigint
                     FROM (SELECT DISTINCT content, size FROM revisions WHERE repo = $1) d) AS bytes,
                    count(DISTINCT path) AS files, count(*) AS revisions
             FROM revisions WHERE repo = $1",
        )
        .bind(repo.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(RepoUsage {
            stored_bytes: row.get::<i64, _>("bytes") as u64,
            files: row.get::<i64, _>("files") as u64,
            revisions: row.get::<i64, _>("revisions") as u64,
        })
    }

    async fn delete_repo(&self, id: &RepoId) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        for stmt in [
            "DELETE FROM locks WHERE repo = $1",
            "DELETE FROM revisions WHERE repo = $1",
            "DELETE FROM repo_commits WHERE repo = $1",
            "DELETE FROM commits WHERE repo = $1",
            "DELETE FROM commit_pointers WHERE repo = $1",
            "DELETE FROM branches WHERE repo = $1",
            "DELETE FROM trees WHERE repo = $1",
            "DELETE FROM repo_objects WHERE repo = $1",
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
    use pyn_core::NewRevision;

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

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn existing_commits_get_the_bytes_their_ids_were_derived_from() {
        use pyn_object::{Algorithm, Commit as GitCommit, Signature};
        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        for stmt in [
            "ALTER TABLE commits DROP COLUMN raw",
            "DELETE FROM _sqlx_migrations WHERE version = 29",
        ] {
            sqlx::query(stmt).execute(&store.pool).await.unwrap();
        }
        let hex =
            |c: char| ObjectId::from_hex(Algorithm::Sha256, &c.to_string().repeat(64)).unwrap();
        let sig = |name: &str, offset| Signature {
            name: name.as_bytes().to_vec(),
            email: format!("{name}@pyn.invalid").into_bytes(),
            time: 1_700_000_000,
            offset_minutes: offset,
        };
        let cases = [
            (vec![], 0, "root", None),
            (vec![hex('1'), hex('2')], -330, "merge\n", None),
            (
                vec![hex('1')],
                90,
                "signed",
                Some("-----BEGIN-----\n\nabc\n-----END-----"),
            ),
        ];
        let mut ids = Vec::new();
        for (seq, (parents, offset, message, signature)) in cases.iter().enumerate() {
            let git = GitCommit {
                tree: hex('a'),
                parents: parents.clone(),
                author: sig("alice", *offset),
                committer: sig("alice", *offset),
                extra_headers: signature
                    .iter()
                    .map(|s| (b"gpgsig".to_vec(), s.as_bytes().to_vec()))
                    .collect(),
                message: format!("{}\n", message.trim_end_matches('\n')).into_bytes(),
            };
            let id = git.id(Algorithm::Sha256).unwrap();
            ids.push(id);
            sqlx::query(
                "INSERT INTO commits (repo, seq, id, parent, tree, author_name, author_email, author_offset,
                                      committer_name, committer_email, committer_offset, created_at, authored_at, message, signature, parents)
                 VALUES ('r', $1, $2, $3, $4, 'alice', 'alice@pyn.invalid', $5, 'alice', 'alice@pyn.invalid', $5, to_timestamp(1700000000), to_timestamp(1700000000), $6, $7, $8)",
            )
            .bind(seq as i64 + 1)
            .bind(id.to_hex())
            .bind(parents.first().map(ObjectId::to_hex))
            .bind(hex('a').to_hex())
            .bind(offset)
            .bind(message)
            .bind(signature)
            .bind(parents.iter().map(ObjectId::to_hex).collect::<Vec<_>>())
            .execute(&store.pool)
            .await
            .unwrap();
        }
        migrate(&store).await;
        let repo = RepoId::new("r");
        for id in ids {
            let c = store.get_commit(&repo, &id).await.unwrap().unwrap();
            c.verify().unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn existing_revisions_seed_the_per_repository_object_record() {
        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        for stmt in [
            "DROP TABLE repo_objects",
            "DELETE FROM _sqlx_migrations WHERE version = 34",
            "INSERT INTO revisions (repo, path, id, content, author, message, created_at, size, hash_version, commit_no) VALUES
               ('r', 'a', 1, repeat('1', 64), 'u', 'm', now(), 5, 1, 1),
               ('r', 'b', 1, repeat('1', 64), 'u', 'm', now(), 5, 1, 2),
               ('r', 'c', 1, repeat('2', 64), 'u', 'm', now(), 7, 1, 3)",
        ] {
            sqlx::query(stmt).execute(&store.pool).await.unwrap();
        }
        migrate(&store).await;
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT id, size FROM repo_objects WHERE repo = 'r' ORDER BY id")
                .fetch_all(&store.pool)
                .await
                .unwrap();
        assert_eq!(rows, [("1".repeat(64), 5), ("2".repeat(64), 7)]);
        let repo = RepoId::new("r");
        assert_eq!(store.unreferenced_bytes(&repo).await.unwrap(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn existing_revisions_get_commit_numbers_in_time_order_with_changesets_grouped() {
        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        for stmt in [
            "DROP TABLE repo_commits",
            "ALTER TABLE revisions DROP COLUMN commit_no",
            "DELETE FROM _sqlx_migrations WHERE version = 22",
            "INSERT INTO revisions (repo, path, id, content, author, message, created_at, changeset, hash_version) VALUES
               ('r', 'a', 1, repeat('1', 64), 'u', 'm', now() - interval '3 hours', NULL, 1),
               ('r', 'b', 1, repeat('2', 64), 'u', 'm', now() - interval '2 hours', 'cs', 1),
               ('r', 'c', 1, repeat('2', 64), 'u', 'm', now() - interval '2 hours', 'cs', 1),
               ('r', 'a', 2, repeat('3', 64), 'u', 'm', now() - interval '1 hours', NULL, 1),
               ('other', 'a', 1, repeat('1', 64), 'u', 'm', now(), NULL, 1)",
        ] {
            sqlx::query(stmt).execute(&store.pool).await.unwrap();
        }
        migrate(&store).await;
        let done = store.migrate_history().await.unwrap();
        assert_eq!((done.repositories, done.commits), (2, 4));
        assert_eq!(store.migrate_history().await.unwrap(), Default::default());

        let repo = RepoId::new("r");
        let commits = store.commits(&repo, None, None, 10).await.unwrap();
        let shape: Vec<usize> = commits.iter().map(|c| c.revisions.len()).collect();
        assert_eq!(shape, vec![1, 2, 1]);
        assert_eq!(commits[0].parents, vec![commits[1].id]);
        assert_eq!(commits[1].parents, vec![commits[2].id]);
        assert!(commits[2].parents.is_empty());
        for c in &commits {
            c.verify().unwrap();
            assert!(
                c.revisions
                    .iter()
                    .all(|r| r.commit == c.id && r.person.is_none() && r.hash_version == 1)
            );
        }
        let snap = store.snapshot(&repo, &commits[1].id).await.unwrap();
        let ids: Vec<u64> = snap.iter().map(|r| r.id.0).collect();
        assert_eq!(ids, vec![1, 1, 1]);
        let head = store.snapshot(&repo, &commits[0].id).await.unwrap();
        assert_eq!(head.len(), 3);

        assert_eq!(store.migrate_branches().await.unwrap().pointers, 0);
        sqlx::query("DELETE FROM commit_pointers")
            .execute(&store.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM branches")
            .execute(&store.pool)
            .await
            .unwrap();
        let done = store.migrate_branches().await.unwrap();
        assert_eq!((done.branches, done.pointers), (2, 5));
        assert_eq!(store.migrate_branches().await.unwrap(), Default::default());
        let main = store.get_branch(&repo, "main").await.unwrap().unwrap();
        assert_eq!((main.default, main.head), (true, Some(commits[0].id)));
        let other = store
            .get_branch(&RepoId::new("other"), "main")
            .await
            .unwrap()
            .unwrap();
        assert!(other.head.is_some());
        let migrated = store.snapshot(&repo, &commits[0].id).await.unwrap();
        let ids: Vec<u64> = migrated.iter().map(|r| r.id.0).collect();
        assert_eq!(ids, vec![2, 1, 1]);
        let at = store
            .revision_at(&repo, &commits[1].id, &RepoPath::new("a").unwrap())
            .await
            .unwrap();
        assert_eq!(at.map(|r| r.id.0), Some(1));
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn existing_revisions_and_events_are_hashed_once_in_chain_order() {
        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        for stmt in [
            "INSERT INTO revisions (repo, path, id, content, author, message, created_at, commit_no, hash_version) VALUES
               ('r', 'a', 1, repeat('1', 64), 'u', 'm1', '2026-01-01T00:00:00.123456Z', 1, 1),
               ('r', 'a', 2, repeat('2', 64), 'u', 'm2', '2026-01-02T00:00:00Z', 2, 1),
               ('r', 'b', 1, repeat('3', 64), 'u', 'm3', '2026-01-03T00:00:00Z', 3, 1)",
            "INSERT INTO audit_events (repo, at, actor, action, path, detail) VALUES
               ('r', '2026-01-01T00:00:00Z', 'u', 'checkin', 'a', 'd1'),
               ('@server', '2026-01-02T00:00:00Z', 'u', 'admin_granted', NULL, 'd2'),
               ('r', '2026-01-03T00:00:00Z', 'u', 'checkin', 'a', 'd3')",
        ] {
            sqlx::query(stmt).execute(&store.pool).await.unwrap();
        }
        hashing::backfill(&store.pool).await.unwrap();

        let repo = RepoId::new("r");
        let a = store
            .history(&repo, &RepoPath::new("a").unwrap(), None)
            .await
            .unwrap();
        assert!(pyn_core::integrity::verify_revisions(&a).is_empty());
        assert_eq!(a[1].previous.as_ref(), Some(&a[0].hash));
        let b = store
            .history(&repo, &RepoPath::new("b").unwrap(), None)
            .await
            .unwrap();
        assert_eq!(b[0].previous, None);

        let scope = pyn_core::AuditScope::Repo(repo);
        let q = pyn_core::AuditQuery {
            limit: 10,
            ..Default::default()
        };
        let mut events = pyn_core::AuditStore::list(&store, &scope, &q)
            .await
            .unwrap();
        events.reverse();
        assert!(pyn_core::integrity::verify_audit(&scope, &events).is_empty());
        assert_eq!(events.len(), 2);

        let before: Vec<String> =
            sqlx::query_scalar("SELECT hash FROM revisions ORDER BY repo, path, id")
                .fetch_all(&store.pool)
                .await
                .unwrap();
        hashing::backfill(&store.pool).await.unwrap();
        let after: Vec<String> =
            sqlx::query_scalar("SELECT hash FROM revisions ORDER BY repo, path, id")
                .fetch_all(&store.pool)
                .await
                .unwrap();
        assert_eq!(before, after, "a second run changes nothing");
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn a_row_edited_in_sql_fails_verification() {
        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        let repo = RepoId::new("r");
        let path = RepoPath::new("a").unwrap();
        for i in 0..2u64 {
            let rev = NewRevision {
                path: path.clone(),
                content: ObjectId::from_hex(REPO_ALGORITHM, &format!("{:064x}", i + 1)).unwrap(),
                file_mode: FileMode::Regular,
                author: UserId::new("u"),
                person: Person::for_user(&UserId::new("u"), None),
                message: "m".into(),
                created_at: "2026-01-01T00:00:00.123456789Z".parse().unwrap(),
                restored_from: None,
                mode: Mode::Shared,
                deleted: false,
                size: 1,
                changeset: None,
            };
            let base = (i > 0).then_some(RevisionId(i));
            store
                .commit_revision(&repo, rev, base, None, Utc::now())
                .await
                .unwrap();
        }
        let revs = store.history(&repo, &path, None).await.unwrap();
        assert!(pyn_core::integrity::verify_revisions(&revs).is_empty());

        sqlx::query("UPDATE revisions SET message = 'x' WHERE id = 1")
            .execute(&store.pool)
            .await
            .unwrap();
        let revs = store.history(&repo, &path, None).await.unwrap();
        assert_eq!(pyn_core::integrity::verify_revisions(&revs).len(), 1);

        sqlx::query("UPDATE revisions SET message = 'm' WHERE id = 1")
            .execute(&store.pool)
            .await
            .unwrap();
        sqlx::query("UPDATE revisions SET hash_version = 1 WHERE id = 2")
            .execute(&store.pool)
            .await
            .unwrap();
        let revs = store.history(&repo, &path, None).await.unwrap();
        let found = pyn_core::integrity::verify_revisions(&revs);
        assert_eq!(found, vec![(RevisionId(2), pyn_core::BreakKind::Version)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn content_ids_are_rekeyed_and_old_hashes_still_verify() {
        use pyn_core::ObjectStore;
        use pyn_core::memory::MemoryObjectStore;
        use sha2::{Digest, Sha256};

        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap();
        let objects = MemoryObjectStore::new();
        let old = objects.put_chunk(b"hello".to_vec()).await.unwrap();
        assert_eq!(old.to_hex(), hex::encode(Sha256::digest(b"hello")));
        let ghost = "ab".repeat(32);
        for content in [old.to_hex(), ghost.clone()] {
            sqlx::query(
                "INSERT INTO revisions (repo, path, id, content, author, message, created_at, commit_no, content_v1, hash_version)
                 VALUES ('r', $1, 1, $2, 'u', 'm', '2026-01-01T00:00:00Z', 1, $2, 1)",
            )
            .bind(&content[..4])
            .bind(&content)
            .execute(&store.pool)
            .await
            .unwrap();
        }
        hashing::backfill(&store.pool).await.unwrap();
        let before = store
            .history(
                &RepoId::new("r"),
                &RepoPath::new(&old.to_hex()[..4]).unwrap(),
                None,
            )
            .await
            .unwrap();

        let report = store.migrate_content_ids(&objects).await.unwrap();
        assert_eq!((report.migrated, report.missing), (1, vec![ghost]));
        let again = store.migrate_content_ids(&objects).await.unwrap();
        assert_eq!(again.migrated, 0);

        let after = store
            .history(
                &RepoId::new("r"),
                &RepoPath::new(&old.to_hex()[..4]).unwrap(),
                None,
            )
            .await
            .unwrap();
        assert_ne!(after[0].content, old);
        assert_eq!(
            objects.get(&after[0].content).await.unwrap().unwrap(),
            b"hello"
        );
        assert_eq!(
            after[0].legacy_content.as_deref(),
            Some(old.to_hex().as_str())
        );
        assert_eq!(after[0].hash, before[0].hash);
        assert!(pyn_core::integrity::verify_revisions(&after).is_empty());

        store.migrate_history().await.unwrap();
        let head = store
            .commits(&RepoId::new("r"), None, None, 1)
            .await
            .unwrap()[0]
            .id;
        let commit = store
            .get_commit(&RepoId::new("r"), &head)
            .await
            .unwrap()
            .unwrap();
        let files = store
            .snapshot(&RepoId::new("r"), &head)
            .await
            .unwrap()
            .iter()
            .map(|r| pyn_core::commit_record::TreeChange {
                path: r.path.clone(),
                mode: r.file_mode,
                id: r.content,
                removed: false,
            })
            .collect();
        assert_eq!(
            pyn_core::commit_record::tree_of(files).unwrap().root,
            commit.tree,
            "the migrated tree names the re-keyed blob ids"
        );
        assert!(
            store
                .get_tree(&RepoId::new("r"), &commit.tree)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs PYN_DATABASE_URL"]
    async fn verify_finds_a_tampered_base_on_a_branch() {
        use pyn_core::memory::{MemoryAuditStore, MemoryObjectStore};
        use pyn_core::{
            BreakKind, FileMode, ManualClock, Principal, RepoService, Rules, ServiceConfig, UserId,
        };
        use std::sync::Arc;

        let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
        let store = Arc::new(
            PgMetadataStore::connect_in_scratch_schema(&url)
                .await
                .unwrap(),
        );
        let objects = Arc::new(MemoryObjectStore::new());
        let svc = RepoService::new(
            RepoId::new("r"),
            Rules::from_toml("[meta]\ndefault = \"exclusive\"\n").unwrap(),
            store.clone(),
            objects.clone(),
            Arc::new(MemoryAuditStore::new()),
            Arc::new(ManualClock::new(Utc::now())),
            ServiceConfig::default(),
        );
        let who = Principal::unrestricted(UserId::new("alice"));
        let path = RepoPath::new("Content/a.umap").unwrap();
        let put = |branch: &'static str, text: &'static str, base: Option<u64>| {
            let (svc, who, path) = (&svc, &who, &path);
            async move {
                let base = base.map(RevisionId);
                svc.checkout_on(branch, path, &who.user, base)
                    .await
                    .unwrap();
                let c = svc.upload_blob(text.as_bytes().to_vec()).await.unwrap();
                svc.checkin_on(branch, who, path, c, FileMode::Regular, base, text.into())
                    .await
                    .unwrap();
            }
        };
        put("main", "one", None).await;
        svc.create_branch(&who, "feature", None).await.unwrap();
        put("feature", "two", Some(1)).await;
        put("main", "three", Some(1)).await;
        assert!(svc.verify(None, false).await.unwrap().problems.is_empty());

        sqlx::query("UPDATE revisions SET base_id = 2 WHERE id = 3")
            .execute(&store.pool)
            .await
            .unwrap();
        let report = svc.verify(None, false).await.unwrap();
        let found: Vec<_> = report
            .problems
            .iter()
            .map(|p| (p.kind, p.revision))
            .collect();
        assert_eq!(found, vec![(BreakKind::Base, Some(RevisionId(3)))]);
    }
}

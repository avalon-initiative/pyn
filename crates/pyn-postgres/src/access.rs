use std::collections::BTreeSet;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pyn_core::{
    AccessStore, InviteId, InviteRecord, Permission, RepoId, Result, Role, RoleDefinitions,
    SessionRecord, SshKeyRecord, TokenId, TokenRecord, UserId,
};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{PgMetadataStore, db};

fn permissions(names: Vec<String>) -> Result<BTreeSet<Permission>> {
    names.iter().map(|n| Permission::from_str(n)).collect()
}

fn names(permissions: &BTreeSet<Permission>) -> Vec<String> {
    permissions.iter().map(|p| p.as_str().to_string()).collect()
}

fn invite_from(row: &PgRow) -> Result<InviteRecord> {
    Ok(InviteRecord {
        id: InviteId(row.get("id")),
        secret_hash: row.get("secret_hash"),
        repo: RepoId::new(row.get::<String, _>("repo")),
        role: Role::from_str(row.get("role"))?,
        created_by: UserId::new(row.get::<String, _>("created_by")),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
        used_at: row.get("used_at"),
        used_by: row.get::<Option<String>, _>("used_by").map(UserId::new),
        revoked_at: row.get("revoked_at"),
    })
}

fn ssh_key_from(row: &PgRow) -> SshKeyRecord {
    SshKeyRecord {
        id: row.get("id"),
        user: UserId::new(row.get::<String, _>("user_id")),
        title: row.get("title"),
        algorithm: row.get("algorithm"),
        public_key: row.get("public_key"),
        fingerprint: row.get("fingerprint"),
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
    }
}

fn session_from(row: &PgRow) -> SessionRecord {
    SessionRecord {
        id_hash: row.get("id_hash"),
        user: UserId::new(row.get::<String, _>("user_id")),
        csrf_token: row.get("csrf_token"),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
    }
}

fn token_from(row: &PgRow) -> Result<TokenRecord> {
    Ok(TokenRecord {
        id: TokenId(row.get("id")),
        user: UserId::new(row.get::<String, _>("user_id")),
        name: row.get("name"),
        secret_hash: row.get("secret_hash"),
        permissions: permissions(row.get("permissions"))?,
        repos: row
            .get::<Vec<String>, _>("repos")
            .into_iter()
            .map(RepoId::new)
            .collect(),
        created_at: row.get("created_at"),
        expires_at: row.get("expires_at"),
        revoked_at: row.get("revoked_at"),
        last_used_at: row.get("last_used_at"),
    })
}

#[async_trait]
impl AccessStore for PgMetadataStore {
    async fn ensure_user(&self, user: &UserId, now: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO users (id, created_at) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
        )
        .bind(user.as_str())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn set_role(&self, repo: &RepoId, user: &UserId, role: Role) -> Result<()> {
        sqlx::query(
            "INSERT INTO memberships (repo, user_id, role) VALUES ($1, $2, $3)
             ON CONFLICT (repo, user_id) DO UPDATE SET role = EXCLUDED.role",
        )
        .bind(repo.as_str())
        .bind(user.as_str())
        .bind(role.as_str())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn role_of(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>> {
        let role: Option<String> =
            sqlx::query_scalar("SELECT role FROM memberships WHERE repo = $1 AND user_id = $2")
                .bind(repo.as_str())
                .bind(user.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        role.map(|r| Role::from_str(&r)).transpose()
    }

    async fn members(&self, repo: &RepoId) -> Result<Vec<(UserId, Role)>> {
        let rows =
            sqlx::query("SELECT user_id, role FROM memberships WHERE repo = $1 ORDER BY user_id")
                .bind(repo.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        rows.iter()
            .map(|r| {
                Ok((
                    UserId::new(r.get::<String, _>("user_id")),
                    Role::from_str(r.get("role"))?,
                ))
            })
            .collect()
    }

    async fn role_definitions(&self, repo: &RepoId) -> Result<RoleDefinitions> {
        let rows = sqlx::query("SELECT role, permissions FROM role_permissions WHERE repo = $1")
            .bind(repo.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        let mut defs = RoleDefinitions::defaults();
        for row in rows {
            defs.set(
                Role::from_str(row.get("role"))?,
                permissions(row.get("permissions"))?,
            );
        }
        Ok(defs)
    }

    async fn set_role_permissions(
        &self,
        repo: &RepoId,
        role: Role,
        granted: BTreeSet<Permission>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO role_permissions (repo, role, permissions) VALUES ($1, $2, $3)
             ON CONFLICT (repo, role) DO UPDATE SET permissions = EXCLUDED.permissions",
        )
        .bind(repo.as_str())
        .bind(role.as_str())
        .bind(names(&granted))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn create_user(&self, user: &UserId, now: DateTime<Utc>) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO users (id, created_at) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
        )
        .bind(user.as_str())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn user_exists(&self, user: &UserId) -> Result<bool> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1)")
            .bind(user.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(db)
    }

    async fn set_password_hash(&self, user: &UserId, hash: &str) -> Result<()> {
        sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1")
            .bind(user.as_str())
            .bind(hash)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn password_hash(&self, user: &UserId) -> Result<Option<String>> {
        let hash: Option<Option<String>> =
            sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
                .bind(user.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        Ok(hash.flatten())
    }

    async fn create_invite(&self, invite: InviteRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO invites (id, secret_hash, repo, role, created_by, created_at, expires_at, used_at, used_by, revoked_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&invite.id.0)
        .bind(&invite.secret_hash)
        .bind(invite.repo.as_str())
        .bind(invite.role.as_str())
        .bind(invite.created_by.as_str())
        .bind(invite.created_at)
        .bind(invite.expires_at)
        .bind(invite.used_at)
        .bind(invite.used_by.as_ref().map(UserId::as_str))
        .bind(invite.revoked_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn get_invite(&self, id: &InviteId) -> Result<Option<InviteRecord>> {
        let row = sqlx::query(
            "SELECT id, secret_hash, repo, role, created_by, created_at, expires_at, used_at, used_by, revoked_at
             FROM invites WHERE id = $1",
        )
        .bind(&id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(invite_from).transpose()
    }

    async fn list_invites(&self, repo: &RepoId) -> Result<Vec<InviteRecord>> {
        let rows = sqlx::query(
            "SELECT id, secret_hash, repo, role, created_by, created_at, expires_at, used_at, used_by, revoked_at
             FROM invites WHERE repo = $1 ORDER BY created_at DESC, id",
        )
        .bind(repo.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(invite_from).collect()
    }

    async fn revoke_invite(&self, id: &InviteId, now: DateTime<Utc>) -> Result<bool> {
        let done =
            sqlx::query("UPDATE invites SET revoked_at = COALESCE(revoked_at, $2) WHERE id = $1")
                .bind(&id.0)
                .bind(now)
                .execute(&self.pool)
                .await
                .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn use_invite(&self, id: &InviteId, user: &UserId, now: DateTime<Utc>) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE invites SET used_at = $3, used_by = $2
             WHERE id = $1 AND used_at IS NULL AND revoked_at IS NULL AND expires_at > $3",
        )
        .bind(&id.0)
        .bind(user.as_str())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn add_ssh_key(&self, key: SshKeyRecord) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO ssh_keys (id, user_id, title, algorithm, public_key, fingerprint, created_at, last_used_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (fingerprint) DO NOTHING",
        )
        .bind(&key.id)
        .bind(key.user.as_str())
        .bind(&key.title)
        .bind(&key.algorithm)
        .bind(&key.public_key)
        .bind(&key.fingerprint)
        .bind(key.created_at)
        .bind(key.last_used_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn list_ssh_keys(&self, user: &UserId) -> Result<Vec<SshKeyRecord>> {
        let rows = sqlx::query(
            "SELECT id, user_id, title, algorithm, public_key, fingerprint, created_at, last_used_at
             FROM ssh_keys WHERE user_id = $1 ORDER BY created_at DESC, id",
        )
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.iter().map(ssh_key_from).collect())
    }

    async fn delete_ssh_key(&self, user: &UserId, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM ssh_keys WHERE user_id = $1 AND id = $2")
            .bind(user.as_str())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn find_ssh_key(&self, fingerprint: &str) -> Result<Option<SshKeyRecord>> {
        let row = sqlx::query(
            "SELECT id, user_id, title, algorithm, public_key, fingerprint, created_at, last_used_at
             FROM ssh_keys WHERE fingerprint = $1",
        )
        .bind(fingerprint)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.as_ref().map(ssh_key_from))
    }

    async fn touch_ssh_key(&self, fingerprint: &str, now: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE ssh_keys SET last_used_at = $2 WHERE fingerprint = $1")
            .bind(fingerprint)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn create_session(&self, session: SessionRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO sessions (id_hash, user_id, csrf_token, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&session.id_hash)
        .bind(session.user.as_str())
        .bind(&session.csrf_token)
        .bind(session.created_at)
        .bind(session.expires_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn get_session(&self, id_hash: &str) -> Result<Option<SessionRecord>> {
        let row = sqlx::query(
            "SELECT id_hash, user_id, csrf_token, created_at, expires_at FROM sessions WHERE id_hash = $1",
        )
        .bind(id_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.as_ref().map(session_from))
    }

    async fn delete_session(&self, id_hash: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM sessions WHERE id_hash = $1")
            .bind(id_hash)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn delete_expired_sessions(&self, now: DateTime<Utc>) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE expires_at <= $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn create_token(&self, token: TokenRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO tokens (id, user_id, name, secret_hash, permissions, repos, created_at, expires_at, revoked_at, last_used_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&token.id.0)
        .bind(token.user.as_str())
        .bind(&token.name)
        .bind(&token.secret_hash)
        .bind(names(&token.permissions))
        .bind(token.repos.iter().map(|r| r.as_str().to_string()).collect::<Vec<_>>())
        .bind(token.created_at)
        .bind(token.expires_at)
        .bind(token.revoked_at)
        .bind(token.last_used_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn get_token(&self, id: &TokenId) -> Result<Option<TokenRecord>> {
        let row = sqlx::query(
            "SELECT id, user_id, name, secret_hash, permissions, repos, created_at, expires_at, revoked_at, last_used_at
             FROM tokens WHERE id = $1",
        )
            .bind(&id.0)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        row.as_ref().map(token_from).transpose()
    }

    async fn list_tokens(&self, user: &UserId) -> Result<Vec<TokenRecord>> {
        let rows = sqlx::query(
            "SELECT id, user_id, name, secret_hash, permissions, repos, created_at, expires_at, revoked_at, last_used_at
             FROM tokens WHERE user_id = $1 ORDER BY created_at DESC, id",
        )
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(token_from).collect()
    }

    async fn revoke_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<bool> {
        let done =
            sqlx::query("UPDATE tokens SET revoked_at = COALESCE(revoked_at, $2) WHERE id = $1")
                .bind(&id.0)
                .bind(now)
                .execute(&self.pool)
                .await
                .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn touch_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE tokens SET last_used_at = $2 WHERE id = $1")
            .bind(&id.0)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

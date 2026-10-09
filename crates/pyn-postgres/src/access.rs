use std::collections::BTreeSet;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pyn_core::{
    AccessStore, AccountKind, AccountRecord, AccountStatus, CreationEffect, CreationRule,
    CreationScope, CreationSubject, InviteId, InviteRecord, MemberCreation, NewAccount,
    OrgDeleteMark, OrgMemberChange, OrgRole, Permission, RateLimitStore, RateState, RepoId,
    RepoPolicy, Result, Role, RoleDefinitions, SessionRecord, SignupStage, SshKeyRecord,
    SubjectKind, TeamRecord, TokenId, TokenRecord, UserId, VerificationRecord,
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

fn team_from(row: &PgRow) -> TeamRecord {
    TeamRecord {
        org: UserId::new(row.get::<String, _>("org")),
        slug: row.get("slug"),
        name: row.get("name"),
        description: row.get("description"),
        created_at: row.get("created_at"),
    }
}

async fn drop_rules(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &UserId,
    kind: &str,
    subject: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM org_repo_creation_rules WHERE org = $1 AND kind = $2 AND subject = $3",
    )
    .bind(org.as_str())
    .bind(kind)
    .bind(subject)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(())
}

fn fold_roles(roles: Vec<String>) -> Result<Option<Role>> {
    let roles: Result<Vec<Role>> = roles.iter().map(|r| Role::from_str(r)).collect();
    Ok(roles?.into_iter().max())
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

fn account_from(row: &PgRow) -> Result<AccountRecord> {
    Ok(AccountRecord {
        user: UserId::new(row.get::<String, _>("id")),
        kind: AccountKind::from_str(row.get("kind"))?,
        email: row.get("email"),
        email_verified_at: row.get("email_verified_at"),
        signup: SignupStage::from_str(row.get("signup"))?,
        disabled_at: row.get("disabled_at"),
        disabled_reason: row.get("disabled_reason"),
        is_admin: row.get("is_admin"),
        created_at: row.get("created_at"),
        deleting_since: row.get("deleting_since"),
    })
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

    async fn repos_of(&self, user: &UserId) -> Result<Vec<RepoId>> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT repo FROM memberships WHERE user_id = $1 ORDER BY repo")
                .bind(user.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        Ok(rows.into_iter().map(RepoId::new).collect())
    }

    async fn delete_repo_access(&self, repo: &RepoId) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        for stmt in [
            "DELETE FROM memberships WHERE repo = $1",
            "DELETE FROM team_repo_roles WHERE repo = $1",
            "DELETE FROM role_permissions WHERE repo = $1",
            "DELETE FROM invites WHERE repo = $1",
        ] {
            sqlx::query(stmt)
                .bind(repo.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)
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

    async fn create_org(&self, org: &UserId, owner: &UserId, now: DateTime<Utc>) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let created = sqlx::query(
            "INSERT INTO users (id, created_at, kind) VALUES ($1, $2, 'org') ON CONFLICT (id) DO NOTHING",
        )
        .bind(org.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected()
            == 1;
        if !created {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO users (id, created_at) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
        )
        .bind(owner.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query("INSERT INTO org_members (org, user_id, role) VALUES ($1, $2, 'owner')")
            .bind(org.as_str())
            .bind(owner.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    async fn org_role(&self, org: &UserId, user: &UserId) -> Result<Option<OrgRole>> {
        let role: Option<String> =
            sqlx::query_scalar("SELECT role FROM org_members WHERE org = $1 AND user_id = $2")
                .bind(org.as_str())
                .bind(user.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        role.map(|r| OrgRole::from_str(&r)).transpose()
    }

    async fn orgs_of(&self, user: &UserId) -> Result<Vec<(UserId, OrgRole)>> {
        let rows = sqlx::query("SELECT org, role FROM org_members WHERE user_id = $1 ORDER BY org")
            .bind(user.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(db)?;
        rows.iter()
            .map(|r| {
                Ok((
                    UserId::new(r.get::<String, _>("org")),
                    OrgRole::from_str(r.get("role"))?,
                ))
            })
            .collect()
    }

    async fn delete_org(&self, org: &UserId) -> Result<bool> {
        let deleted = sqlx::query("DELETE FROM users WHERE id = $1 AND kind = 'org'")
            .bind(org.as_str())
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(deleted.rows_affected() == 1)
    }

    async fn mark_org_deleting(
        &self,
        org: &UserId,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<OrgDeleteMark> {
        let marked = sqlx::query(
            "UPDATE users SET deleting_since = $2 WHERE id = $1 AND kind = 'org' AND (deleting_since IS NULL OR deleting_since <= $3)",
        )
        .bind(org.as_str())
        .bind(now)
        .bind(stale_before)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if marked.rows_affected() == 1 {
            return Ok(OrgDeleteMark::Set);
        }
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND kind = 'org')",
        )
        .bind(org.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(if exists {
            OrgDeleteMark::Held
        } else {
            OrgDeleteMark::NotFound
        })
    }

    async fn clear_org_deleting(&self, org: &UserId) -> Result<()> {
        sqlx::query("UPDATE users SET deleting_since = NULL WHERE id = $1")
            .bind(org.as_str())
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn repo_policy(&self, org: &UserId) -> Result<RepoPolicy> {
        let base: Option<String> =
            sqlx::query_scalar("SELECT member_creation FROM org_repo_policy WHERE org = $1")
                .bind(org.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        let rows = sqlx::query(
            "SELECT kind, subject, effect, scope FROM org_repo_creation_rules
             WHERE org = $1 ORDER BY kind, subject, effect",
        )
        .bind(org.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let rules = rows
            .iter()
            .map(|r| {
                Ok(CreationRule {
                    subject: CreationSubject::parse(
                        SubjectKind::from_str(r.get("kind"))?,
                        r.get("subject"),
                    )?,
                    effect: CreationEffect::from_str(r.get("effect"))?,
                    scope: CreationScope::from_str(r.get("scope"))?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(RepoPolicy {
            base: base.map_or(Ok(MemberCreation::None), |b| MemberCreation::from_str(&b))?,
            rules,
        })
    }

    async fn set_member_creation(
        &self,
        org: &UserId,
        base: MemberCreation,
    ) -> Result<MemberCreation> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let old: Option<String> = sqlx::query_scalar(
            "SELECT member_creation FROM org_repo_policy WHERE org = $1 FOR UPDATE",
        )
        .bind(org.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO org_repo_policy (org, member_creation) VALUES ($1, $2)
             ON CONFLICT (org) DO UPDATE SET member_creation = EXCLUDED.member_creation",
        )
        .bind(org.as_str())
        .bind(base.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        old.map_or(Ok(MemberCreation::None), |o| MemberCreation::from_str(&o))
    }

    async fn set_creation_rule(&self, org: &UserId, rule: &CreationRule) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let subject = rule.subject.name();
        let present: Option<i32> = match &rule.subject {
            CreationSubject::Team(slug) => {
                sqlx::query_scalar("SELECT 1 FROM teams WHERE org = $1 AND slug = $2 FOR SHARE")
                    .bind(org.as_str())
                    .bind(slug)
                    .fetch_optional(&mut *tx)
                    .await
            }
            CreationSubject::User(user) => {
                sqlx::query_scalar(
                    "SELECT 1 FROM org_members WHERE org = $1 AND user_id = $2 FOR SHARE",
                )
                .bind(org.as_str())
                .bind(user.as_str())
                .fetch_optional(&mut *tx)
                .await
            }
            CreationSubject::Role(_) => Ok(Some(1)),
        }
        .map_err(db)?;
        if present.is_none() {
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO org_repo_creation_rules (org, kind, subject, effect, scope)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (org, kind, subject, effect) DO UPDATE SET scope = EXCLUDED.scope",
        )
        .bind(org.as_str())
        .bind(rule.subject.kind().as_str())
        .bind(subject)
        .bind(rule.effect.as_str())
        .bind(rule.scope.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    async fn remove_creation_rule(
        &self,
        org: &UserId,
        subject: &CreationSubject,
        effect: CreationEffect,
    ) -> Result<Option<CreationScope>> {
        let old: Option<String> = sqlx::query_scalar(
            "DELETE FROM org_repo_creation_rules
             WHERE org = $1 AND kind = $2 AND subject = $3 AND effect = $4 RETURNING scope",
        )
        .bind(org.as_str())
        .bind(subject.kind().as_str())
        .bind(subject.name())
        .bind(effect.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        old.map(|s| CreationScope::from_str(&s)).transpose()
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

    async fn org_members(&self, org: &UserId) -> Result<Vec<(UserId, OrgRole)>> {
        let rows =
            sqlx::query("SELECT user_id, role FROM org_members WHERE org = $1 ORDER BY user_id")
                .bind(org.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        rows.iter()
            .map(|r| {
                Ok((
                    UserId::new(r.get::<String, _>("user_id")),
                    OrgRole::from_str(r.get("role"))?,
                ))
            })
            .collect()
    }

    async fn add_org_member(&self, org: &UserId, user: &UserId, role: OrgRole) -> Result<bool> {
        let added = sqlx::query(
            "INSERT INTO org_members (org, user_id, role) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(org.as_str())
        .bind(user.as_str())
        .bind(role.as_str())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(added.rows_affected() == 1)
    }

    async fn set_org_role(
        &self,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
    ) -> Result<OrgMemberChange> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let Some((old, owners)) = locked_member(&mut tx, org, user).await? else {
            return Ok(OrgMemberChange::NotMember);
        };
        if old == OrgRole::Owner && role != OrgRole::Owner && owners == 1 {
            return Ok(OrgMemberChange::LastOwner);
        }
        sqlx::query("UPDATE org_members SET role = $3 WHERE org = $1 AND user_id = $2")
            .bind(org.as_str())
            .bind(user.as_str())
            .bind(role.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(OrgMemberChange::Done(old))
    }

    async fn remove_org_member(
        &self,
        org: &UserId,
        user: &UserId,
        repos: &[RepoId],
    ) -> Result<OrgMemberChange> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let Some((old, owners)) = locked_member(&mut tx, org, user).await? else {
            return Ok(OrgMemberChange::NotMember);
        };
        if old == OrgRole::Owner && owners == 1 {
            return Ok(OrgMemberChange::LastOwner);
        }
        sqlx::query("DELETE FROM org_members WHERE org = $1 AND user_id = $2")
            .bind(org.as_str())
            .bind(user.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        drop_rules(&mut tx, org, "user", user.as_str()).await?;
        let repos: Vec<String> = repos.iter().map(|r| r.as_str().to_string()).collect();
        sqlx::query("DELETE FROM memberships WHERE user_id = $1 AND repo = ANY($2)")
            .bind(user.as_str())
            .bind(&repos)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(OrgMemberChange::Done(old))
    }

    async fn create_team(&self, team: TeamRecord) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO teams (org, slug, name, description, created_at) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING",
        )
        .bind(team.org.as_str())
        .bind(&team.slug)
        .bind(&team.name)
        .bind(&team.description)
        .bind(team.created_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() == 1)
    }

    async fn team(&self, org: &UserId, slug: &str) -> Result<Option<TeamRecord>> {
        let row = sqlx::query(
            "SELECT org, slug, name, description, created_at FROM teams WHERE org = $1 AND slug = $2",
        )
        .bind(org.as_str())
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.as_ref().map(team_from))
    }

    async fn teams(&self, org: &UserId) -> Result<Vec<TeamRecord>> {
        let rows = sqlx::query(
            "SELECT org, slug, name, description, created_at FROM teams WHERE org = $1 ORDER BY slug",
        )
        .bind(org.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.iter().map(team_from).collect())
    }

    async fn update_team(&self, team: &TeamRecord) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE teams SET name = $3, description = $4 WHERE org = $1 AND slug = $2",
        )
        .bind(team.org.as_str())
        .bind(&team.slug)
        .bind(&team.name)
        .bind(&team.description)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() == 1)
    }

    async fn delete_team(&self, org: &UserId, slug: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let done = sqlx::query("DELETE FROM teams WHERE org = $1 AND slug = $2")
            .bind(org.as_str())
            .bind(slug)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        drop_rules(&mut tx, org, "team", slug).await?;
        tx.commit().await.map_err(db)?;
        Ok(done.rows_affected() == 1)
    }

    async fn team_members(&self, org: &UserId, slug: &str) -> Result<Vec<UserId>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT user_id FROM team_members WHERE org = $1 AND team = $2 ORDER BY user_id",
        )
        .bind(org.as_str())
        .bind(slug)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.into_iter().map(UserId::new).collect())
    }

    async fn add_team_member(&self, org: &UserId, slug: &str, user: &UserId) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO team_members (org, team, user_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(org.as_str())
        .bind(slug)
        .bind(user.as_str())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() == 1)
    }

    async fn remove_team_member(&self, org: &UserId, slug: &str, user: &UserId) -> Result<bool> {
        let done =
            sqlx::query("DELETE FROM team_members WHERE org = $1 AND team = $2 AND user_id = $3")
                .bind(org.as_str())
                .bind(slug)
                .bind(user.as_str())
                .execute(&self.pool)
                .await
                .map_err(db)?;
        Ok(done.rows_affected() == 1)
    }

    async fn teams_of(&self, org: &UserId, user: &UserId) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT team FROM team_members WHERE org = $1 AND user_id = $2 ORDER BY team",
        )
        .bind(org.as_str())
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)
    }

    async fn set_team_role(
        &self,
        repo: &RepoId,
        org: &UserId,
        slug: &str,
        role: Role,
    ) -> Result<Option<Role>> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let old: Option<String> = sqlx::query_scalar(
            "SELECT role FROM team_repo_roles WHERE repo = $1 AND org = $2 AND team = $3 FOR UPDATE",
        )
        .bind(repo.as_str())
        .bind(org.as_str())
        .bind(slug)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO team_repo_roles (repo, org, team, role) VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo, org, team) DO UPDATE SET role = EXCLUDED.role",
        )
        .bind(repo.as_str())
        .bind(org.as_str())
        .bind(slug)
        .bind(role.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        old.map(|r| Role::from_str(&r)).transpose()
    }

    async fn remove_team_role(
        &self,
        repo: &RepoId,
        org: &UserId,
        slug: &str,
    ) -> Result<Option<Role>> {
        let old: Option<String> = sqlx::query_scalar(
            "DELETE FROM team_repo_roles WHERE repo = $1 AND org = $2 AND team = $3 RETURNING role",
        )
        .bind(repo.as_str())
        .bind(org.as_str())
        .bind(slug)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        old.map(|r| Role::from_str(&r)).transpose()
    }

    async fn team_grants(&self, repo: &RepoId) -> Result<Vec<(String, Role)>> {
        let rows =
            sqlx::query("SELECT team, role FROM team_repo_roles WHERE repo = $1 ORDER BY team")
                .bind(repo.as_str())
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        rows.iter()
            .map(|r| Ok((r.get::<String, _>("team"), Role::from_str(r.get("role"))?)))
            .collect()
    }

    async fn team_repos(&self, org: &UserId, slug: &str) -> Result<Vec<(RepoId, Role)>> {
        let rows = sqlx::query(
            "SELECT repo, role FROM team_repo_roles WHERE org = $1 AND team = $2 ORDER BY repo",
        )
        .bind(org.as_str())
        .bind(slug)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|r| {
                Ok((
                    RepoId::new(r.get::<String, _>("repo")),
                    Role::from_str(r.get("role"))?,
                ))
            })
            .collect()
    }

    async fn team_role_of(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>> {
        let roles: Vec<String> = sqlx::query_scalar(
            "SELECT g.role FROM team_repo_roles g
             JOIN team_members m ON m.org = g.org AND m.team = g.team
             WHERE g.repo = $1 AND m.user_id = $2",
        )
        .bind(repo.as_str())
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        fold_roles(roles)
    }

    async fn team_repos_of(&self, user: &UserId) -> Result<Vec<RepoId>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT g.repo FROM team_repo_roles g
             JOIN team_members m ON m.org = g.org AND m.team = g.team
             WHERE m.user_id = $1 ORDER BY g.repo",
        )
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.into_iter().map(RepoId::new).collect())
    }

    async fn create_account(&self, new: NewAccount) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO users (id, created_at, email, password_hash, signup) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(new.user.as_str())
        .bind(new.created_at)
        .bind(new.email)
        .bind(new.password_hash)
        .bind(new.signup.as_str())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn account(&self, user: &UserId) -> Result<Option<AccountRecord>> {
        let row = sqlx::query("SELECT id, kind, email, email_verified_at, signup, disabled_at, disabled_reason, is_admin, created_at, deleting_since FROM users WHERE id = $1")
        .bind(user.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(account_from).transpose()
    }

    async fn verified_email_owner(&self, email: &str) -> Result<Option<UserId>> {
        let id: Option<String> = sqlx::query_scalar(
            "SELECT id FROM users WHERE email = $1 AND email_verified_at IS NOT NULL",
        )
        .bind(email)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(id.map(UserId::new))
    }

    async fn pending_by_email(&self, email: &str) -> Result<Vec<AccountRecord>> {
        let rows = sqlx::query("SELECT id, kind, email, email_verified_at, signup, disabled_at, disabled_reason, is_admin, created_at, deleting_since FROM users WHERE email = $1 AND signup = 'pending_verification' ORDER BY created_at, id")
        .bind(email)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(account_from).collect()
    }

    async fn complete_verification(
        &self,
        user: &UserId,
        signup: SignupStage,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE users SET email_verified_at = $3, signup = $2 WHERE id = $1 AND email IS NOT NULL",
        )
        .bind(user.as_str())
        .bind(signup.as_str())
        .bind(now)
        .execute(&self.pool)
        .await;
        match done {
            Ok(done) => Ok(done.rows_affected() > 0),
            Err(e)
                if e.as_database_error()
                    .is_some_and(|d| d.is_unique_violation()) =>
            {
                Ok(false)
            }
            Err(e) => Err(db(e)),
        }
    }

    async fn set_signup(&self, user: &UserId, signup: SignupStage) -> Result<bool> {
        let done = sqlx::query("UPDATE users SET signup = $2 WHERE id = $1")
            .bind(user.as_str())
            .bind(signup.as_str())
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn set_disabled(
        &self,
        user: &UserId,
        disabled: Option<(DateTime<Utc>, Option<String>)>,
    ) -> Result<bool> {
        let (at, reason) = match disabled {
            Some((at, reason)) => (Some(at), reason),
            None => (None, None),
        };
        let done =
            sqlx::query("UPDATE users SET disabled_at = $2, disabled_reason = $3 WHERE id = $1")
                .bind(user.as_str())
                .bind(at)
                .bind(reason)
                .execute(&self.pool)
                .await
                .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn set_admin(&self, user: &UserId, admin: bool) -> Result<()> {
        sqlx::query("UPDATE users SET is_admin = $2 WHERE id = $1")
            .bind(user.as_str())
            .bind(admin)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn list_accounts(
        &self,
        status: Option<AccountStatus>,
        limit: usize,
    ) -> Result<Vec<AccountRecord>> {
        let rows = sqlx::query("SELECT id, kind, email, email_verified_at, signup, disabled_at, disabled_reason, is_admin, created_at, deleting_since FROM users
             WHERE kind = 'user'
               AND ($1::text IS NULL
                OR CASE WHEN $1 = 'disabled' THEN disabled_at IS NOT NULL
                        ELSE disabled_at IS NULL AND signup = $1 END)
             ORDER BY created_at, id LIMIT $2")
        .bind(status.map(AccountStatus::as_str))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(account_from).collect()
    }

    async fn delete_stale_pending(
        &self,
        now: DateTime<Utc>,
        created_before: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query("DELETE FROM email_verifications WHERE expires_at <= $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        sqlx::query(
            "DELETE FROM users
             WHERE signup = 'pending_verification' AND disabled_at IS NULL AND created_at < $1
               AND NOT EXISTS (SELECT 1 FROM email_verifications v WHERE v.user_id = users.id)",
        )
        .bind(created_before)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn put_verification(&self, record: VerificationRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO email_verifications (token_hash, user_id, email, expires_at) VALUES ($1, $2, $3, $4)
             ON CONFLICT (user_id) DO UPDATE SET
                token_hash = EXCLUDED.token_hash, email = EXCLUDED.email, expires_at = EXCLUDED.expires_at",
        )
        .bind(record.token_hash)
        .bind(record.user.as_str())
        .bind(record.email)
        .bind(record.expires_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn take_verification(&self, token_hash: &str) -> Result<Option<VerificationRecord>> {
        let row = sqlx::query(
            "DELETE FROM email_verifications WHERE token_hash = $1
             RETURNING token_hash, user_id, email, expires_at",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.map(|r| VerificationRecord {
            token_hash: r.get("token_hash"),
            user: UserId::new(r.get::<String, _>("user_id")),
            email: r.get("email"),
            expires_at: r.get("expires_at"),
        }))
    }

    async fn delete_sessions_of(&self, user: &UserId) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user.as_str())
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

#[async_trait]
impl RateLimitStore for PgMetadataStore {
    async fn hit(&self, key: &str, window: Duration, now: DateTime<Utc>) -> Result<RateState> {
        let row = sqlx::query(
            "INSERT INTO rate_limits (key, count, resets_at)
             VALUES ($1, 1, $2::timestamptz + make_interval(secs => $3))
             ON CONFLICT (key) DO UPDATE SET
                count = CASE WHEN rate_limits.resets_at <= $2 THEN 1 ELSE rate_limits.count + 1 END,
                resets_at = CASE WHEN rate_limits.resets_at <= $2 THEN EXCLUDED.resets_at ELSE rate_limits.resets_at END
             RETURNING count, resets_at",
        )
        .bind(key)
        .bind(now)
        .bind(window.num_seconds() as f64)
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(RateState {
            count: row.get::<i32, _>("count") as u32,
            resets_at: row.get("resets_at"),
        })
    }

    async fn state(&self, key: &str, now: DateTime<Utc>) -> Result<Option<RateState>> {
        let row = sqlx::query(
            "SELECT count, resets_at FROM rate_limits WHERE key = $1 AND resets_at > $2",
        )
        .bind(key)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.map(|r| RateState {
            count: r.get::<i32, _>("count") as u32,
            resets_at: r.get("resets_at"),
        }))
    }

    async fn reset(&self, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM rate_limits WHERE key = $1")
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn sweep(&self, now: DateTime<Utc>) -> Result<()> {
        sqlx::query("DELETE FROM rate_limits WHERE resets_at <= $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

/// Locks the organization's member rows; returns the user's role and the number of owners.
async fn locked_member(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    org: &UserId,
    user: &UserId,
) -> Result<Option<(OrgRole, usize)>> {
    let rows = sqlx::query("SELECT user_id, role FROM org_members WHERE org = $1 FOR UPDATE")
        .bind(org.as_str())
        .fetch_all(&mut **tx)
        .await
        .map_err(db)?;
    let mut found = None;
    let mut owners = 0;
    for r in &rows {
        let role = OrgRole::from_str(r.get("role"))?;
        owners += usize::from(role == OrgRole::Owner);
        if r.get::<String, _>("user_id") == user.as_str() {
            found = Some(role);
        }
    }
    Ok(found.map(|role| (role, owners)))
}

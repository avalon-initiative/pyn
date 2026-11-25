//! Account profile rows.

use chrono::{DateTime, Utc};
use pyn_core::{
    AccountKind, AccountRecord, Avatar, ImageKind, ObjectId, OrgProfile, OrgProfileFields,
    OwnerProfile, Profile, ProfileFields, ProfileVisibility, PynError, Result, UserId,
};
use sqlx::Row;
use sqlx::postgres::{PgConnection, PgRow};

use crate::access::account_from;
use crate::{PgMetadataStore, db};

impl PgMetadataStore {
    pub(crate) async fn profile_row(&self, user: &UserId) -> Result<Profile> {
        let row = sqlx::query(
            "SELECT display_name, bio, location, company, links, avatar_content, avatar_type, avatar_size,
                    show_display_name, show_bio, show_location, show_company, show_links, show_avatar, show_starred
             FROM account_profiles WHERE user_id = $1",
        )
        .bind(user.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref()
            .map_or_else(|| Ok(Profile::default()), profile_from)
    }

    pub(crate) async fn owner_profile_rows(
        &self,
        owners: &[UserId],
    ) -> Result<Vec<Option<(AccountRecord, OwnerProfile)>>> {
        let names: Vec<&str> = owners.iter().map(UserId::as_str).collect();
        let rows = sqlx::query(
            "SELECT u.id, u.account_id, u.kind, u.email, u.email_verified_at, u.signup, u.disabled_at,
                    u.disabled_reason, u.is_admin, u.created_at, u.deleting_since, u.frozen_at, u.frozen_reason,
                    p.user_id IS NOT NULL AS has_profile, o.org_id IS NOT NULL AS has_org_profile,
                    p.display_name, p.bio, p.location, p.company, p.links,
                    p.avatar_content, p.avatar_type, p.avatar_size,
                    p.show_display_name, p.show_bio, p.show_location, p.show_company, p.show_links,
                    p.show_avatar, p.show_starred,
                    o.description AS org_description, o.display_name AS org_display_name, o.url AS org_url,
                    o.avatar_content AS org_avatar_content, o.avatar_type AS org_avatar_type,
                    o.avatar_size AS org_avatar_size
             FROM users u
             LEFT JOIN account_profiles p ON p.user_id = u.id
             LEFT JOIN org_profiles o ON o.org_id = u.id
             WHERE u.id = ANY($1)",
        )
        .bind(&names)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let mut found = std::collections::HashMap::with_capacity(rows.len());
        for row in &rows {
            let account = account_from(row)?;
            let profile = match account.kind {
                AccountKind::User if row.get("has_profile") => {
                    OwnerProfile::User(profile_from(row)?)
                }
                AccountKind::User => OwnerProfile::User(Profile::default()),
                AccountKind::Org if row.get("has_org_profile") => {
                    OwnerProfile::Org(org_profile_from(row, "org_")?)
                }
                AccountKind::Org => OwnerProfile::Org(OrgProfile::default()),
            };
            found.insert(account.user.clone(), (account, profile));
        }
        Ok(owners.iter().map(|o| found.get(o).cloned()).collect())
    }

    /// Writes the text and, when given, the visibility in one transaction.
    pub(crate) async fn put_profile(
        &self,
        user: &UserId,
        fields: &ProfileFields,
        visibility: Option<&ProfileVisibility>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        self.put_profile_fields(&mut tx, user, fields, now).await?;
        if let Some(v) = visibility {
            self.put_visibility(&mut tx, user, v, now).await?;
        }
        tx.commit().await.map_err(db)
    }

    pub(crate) async fn put_profile_fields(
        &self,
        conn: &mut PgConnection,
        user: &UserId,
        fields: &ProfileFields,
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO account_profiles (user_id, display_name, bio, location, company, links, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (user_id) DO UPDATE SET display_name = $2, bio = $3, location = $4,
                 company = $5, links = $6, updated_at = $7",
        )
        .bind(user.as_str())
        .bind(&fields.display_name)
        .bind(&fields.bio)
        .bind(&fields.location)
        .bind(&fields.company)
        .bind(&fields.links)
        .bind(now)
        .execute(conn)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub(crate) async fn put_visibility(
        &self,
        conn: &mut PgConnection,
        user: &UserId,
        v: &ProfileVisibility,
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO account_profiles (user_id, show_display_name, show_bio, show_location, show_company,
                 show_links, show_avatar, show_starred, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (user_id) DO UPDATE SET show_display_name = $2, show_bio = $3, show_location = $4,
                 show_company = $5, show_links = $6, show_avatar = $7, show_starred = $8, updated_at = $9",
        )
        .bind(user.as_str())
        .bind(v.display_name)
        .bind(v.bio)
        .bind(v.location)
        .bind(v.company)
        .bind(v.links)
        .bind(v.avatar)
        .bind(v.starred)
        .bind(now)
        .execute(conn)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub(crate) async fn put_avatar(
        &self,
        user: &UserId,
        avatar: Option<&Avatar>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO account_profiles (user_id, avatar_content, avatar_type, avatar_size, updated_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id) DO UPDATE SET avatar_content = $2, avatar_type = $3,
                 avatar_size = $4, updated_at = $5",
        )
        .bind(user.as_str())
        .bind(avatar.map(|a| a.content.to_hex()))
        .bind(avatar.map(|a| a.kind.media_type()))
        .bind(avatar.map(|a| a.size as i64))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }
}

fn profile_from(row: &PgRow) -> Result<Profile> {
    Ok(Profile {
        fields: ProfileFields {
            display_name: row.get("display_name"),
            bio: row.get("bio"),
            location: row.get("location"),
            company: row.get("company"),
            links: row.get("links"),
        },
        avatar: avatar_of(row)?,
        visibility: ProfileVisibility {
            display_name: row.get("show_display_name"),
            bio: row.get("show_bio"),
            location: row.get("show_location"),
            company: row.get("show_company"),
            links: row.get("show_links"),
            avatar: row.get("show_avatar"),
            starred: row.get("show_starred"),
        },
    })
}

/// An organization profile from columns named with `prefix` (`{prefix}display_name`, `{prefix}avatar_content`, ...).
fn org_profile_from(row: &PgRow, prefix: &str) -> Result<OrgProfile> {
    let col = |name: &str| format!("{prefix}{name}");
    Ok(OrgProfile {
        description: row.get(col("description").as_str()),
        display_name: row.get(col("display_name").as_str()),
        url: row.get(col("url").as_str()),
        avatar: image_of(row, &col("avatar"))?,
    })
}

fn avatar_of(row: &PgRow) -> Result<Option<Avatar>> {
    image_of(row, "avatar")
}

/// The image record in the `{prefix}_content`, `{prefix}_type` and `{prefix}_size` columns.
pub(crate) fn image_of(row: &PgRow, prefix: &str) -> Result<Option<Avatar>> {
    let corrupt = |what: &str| PynError::Storage(format!("stored {prefix} has a bad {what}"));
    let Some(content) = row.get::<Option<String>, _>(format!("{prefix}_content").as_str()) else {
        return Ok(None);
    };
    let kind: Option<String> = row.get(format!("{prefix}_type").as_str());
    let size: Option<i64> = row.get(format!("{prefix}_size").as_str());
    Ok(Some(Avatar {
        content: ObjectId::parse(&content).map_err(|_| corrupt("hash"))?,
        kind: kind
            .as_deref()
            .and_then(ImageKind::from_media_type)
            .ok_or_else(|| corrupt("type"))?,
        size: size
            .and_then(|s| u64::try_from(s).ok())
            .ok_or_else(|| corrupt("size"))?,
    }))
}

impl PgMetadataStore {
    pub(crate) async fn org_profile_row(&self, org: &UserId) -> Result<OrgProfile> {
        let row = sqlx::query(
            "SELECT description, display_name, url, avatar_content, avatar_type, avatar_size FROM org_profiles WHERE org_id = $1",
        )
        .bind(org.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref()
            .map_or_else(|| Ok(OrgProfile::default()), |r| org_profile_from(r, ""))
    }

    pub(crate) async fn put_org_fields(
        &self,
        org: &UserId,
        fields: &OrgProfileFields,
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO org_profiles (org_id, description, display_name, url, updated_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (org_id) DO UPDATE SET description = $2, display_name = $3, url = $4,
                 updated_at = $5",
        )
        .bind(org.as_str())
        .bind(fields.description.as_deref())
        .bind(fields.display_name.as_deref())
        .bind(fields.url.as_deref())
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub(crate) async fn put_org_avatar(
        &self,
        org: &UserId,
        avatar: Option<&Avatar>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO org_profiles (org_id, avatar_content, avatar_type, avatar_size, updated_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (org_id) DO UPDATE SET avatar_content = $2, avatar_type = $3,
                 avatar_size = $4, updated_at = $5",
        )
        .bind(org.as_str())
        .bind(avatar.map(|a| a.content.to_hex()))
        .bind(avatar.map(|a| a.kind.media_type()))
        .bind(avatar.map(|a| a.size as i64))
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }
}

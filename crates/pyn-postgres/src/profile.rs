//! Account profile rows.

use chrono::{DateTime, Utc};
use pyn_core::{
    Avatar, ImageKind, ObjectId, OrgProfile, OrgProfileFields, Profile, ProfileFields,
    ProfileVisibility, PynError, Result, UserId,
};
use sqlx::Row;
use sqlx::postgres::{PgConnection, PgRow};

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
        let Some(row) = row else {
            return Ok(Profile::default());
        };
        let avatar = avatar_of(&row)?;
        Ok(Profile {
            fields: ProfileFields {
                display_name: row.get("display_name"),
                bio: row.get("bio"),
                location: row.get("location"),
                company: row.get("company"),
                links: row.get("links"),
            },
            avatar,
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

fn avatar_of(row: &PgRow) -> Result<Option<Avatar>> {
    let corrupt = |what: &str| PynError::Storage(format!("stored avatar has a bad {what}"));
    let Some(content) = row.get::<Option<String>, _>("avatar_content") else {
        return Ok(None);
    };
    let kind: Option<String> = row.get("avatar_type");
    let size: Option<i64> = row.get("avatar_size");
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
        let Some(row) = row else {
            return Ok(OrgProfile::default());
        };
        Ok(OrgProfile {
            description: row.get("description"),
            display_name: row.get("display_name"),
            url: row.get("url"),
            avatar: avatar_of(&row)?,
        })
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

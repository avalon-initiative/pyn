//! Account profile rows.

use chrono::{DateTime, Utc};
use pyn_core::{Avatar, ImageKind, ObjectId, Profile, ProfileFields, PynError, Result, UserId};
use sqlx::Row;

use crate::{PgMetadataStore, db};

impl PgMetadataStore {
    pub(crate) async fn profile_row(&self, user: &UserId) -> Result<Profile> {
        let row = sqlx::query(
            "SELECT display_name, bio, location, company, links, avatar_content, avatar_type, avatar_size
             FROM account_profiles WHERE user_id = $1",
        )
        .bind(user.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(Profile::default());
        };
        let corrupt = |what: &str| PynError::Storage(format!("stored avatar has a bad {what}"));
        let avatar = match row.get::<Option<String>, _>("avatar_content") {
            None => None,
            Some(content) => {
                let kind: Option<String> = row.get("avatar_type");
                let size: Option<i64> = row.get("avatar_size");
                Some(Avatar {
                    content: ObjectId::parse(&content).map_err(|_| corrupt("hash"))?,
                    kind: kind
                        .as_deref()
                        .and_then(ImageKind::from_media_type)
                        .ok_or_else(|| corrupt("type"))?,
                    size: size
                        .and_then(|s| u64::try_from(s).ok())
                        .ok_or_else(|| corrupt("size"))?,
                })
            }
        };
        Ok(Profile {
            fields: ProfileFields {
                display_name: row.get("display_name"),
                bio: row.get("bio"),
                location: row.get("location"),
                company: row.get("company"),
                links: row.get("links"),
            },
            avatar,
        })
    }

    pub(crate) async fn put_profile_fields(
        &self,
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
        .execute(&self.pool)
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

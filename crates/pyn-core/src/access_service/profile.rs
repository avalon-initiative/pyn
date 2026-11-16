//! Account profiles: reading them, and the signed-in user's own edits.

use super::*;
use crate::profile::{Avatar, Profile, ProfileFields, ProfileVisibility};

/// Avatar changes one account may make per hour.
const AVATAR_CHANGES_PER_HOUR: u32 = 30;

impl AccessService {
    /// The active user account named `user`. A missing account, an organization, and one that is disabled or
    /// not yet approved are the same `UserNotFound`.
    pub async fn active_user(&self, user: &UserId) -> Result<AccountRecord> {
        self.store
            .account(user)
            .await?
            .filter(|a| a.kind == AccountKind::User && a.status() == AccountStatus::Active)
            .ok_or_else(|| PynError::UserNotFound(user.to_string()))
    }

    /// For each name, the active user account's canonical name and profile; `None` for any other.
    pub async fn public_profiles(
        &self,
        users: &[UserId],
    ) -> Result<Vec<Option<(UserId, Profile)>>> {
        let mut out = Vec::with_capacity(users.len());
        for user in users {
            out.push(match self.active_user(user).await {
                Ok(a) => Some((
                    a.user.clone(),
                    self.store.profile(&a.user).await?.public_view(),
                )),
                Err(PynError::UserNotFound(_)) => None,
                Err(e) => return Err(e),
            });
        }
        Ok(out)
    }

    pub async fn profile_of(&self, user: &UserId) -> Result<Profile> {
        self.store.profile(user).await
    }

    /// Whether the account can sign in with a password.
    pub async fn has_password(&self, user: &UserId) -> Result<bool> {
        Ok(self.store.password_hash(user).await?.is_some())
    }

    /// Replaces the actor's profile text with the validated `fields`; `visibility` is left as is when `None`.
    pub async fn update_profile(
        &self,
        actor: &Identity,
        fields: ProfileFields,
        visibility: Option<ProfileVisibility>,
    ) -> Result<Profile> {
        actor.require_full_scope()?;
        let fields = fields.validated()?;
        self.account_id(&actor.user).await?;
        self.store
            .set_profile(&actor.user, &fields, self.clock.now())
            .await?;
        if let Some(v) = visibility {
            self.store
                .set_profile_visibility(&actor.user, &v, self.clock.now())
                .await?;
        }
        self.store.profile(&actor.user).await
    }

    /// Records or clears the actor's avatar; the bytes are already stored.
    pub async fn update_avatar(&self, actor: &Identity, avatar: Option<Avatar>) -> Result<Profile> {
        actor.require_full_scope()?;
        self.account_id(&actor.user).await?;
        self.count_limited(
            &format!(
                "avatar:{}",
                bounded_key(&actor.user.as_str().to_lowercase())
            ),
            AVATAR_CHANGES_PER_HOUR,
            Duration::hours(1),
        )
        .await?;
        self.store
            .set_avatar(&actor.user, avatar.as_ref(), self.clock.now())
            .await?;
        self.store.profile(&actor.user).await
    }
}

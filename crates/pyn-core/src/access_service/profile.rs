//! Account profiles: reading them, and the signed-in user's own edits.

use super::*;
use crate::profile::{
    Avatar, OrgProfile, Profile, ProfileFields, ProfileVisibility, validate_org_description,
};

/// The longest people search, in characters.
pub const MAX_PEOPLE_QUERY_CHARS: usize = 64;

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

    /// One page of the people directory: active accounts by name with their public profile, and the cursor for the
    /// next page. `query` matches account names and visible display names, case-insensitively.
    pub async fn people(
        &self,
        query: Option<&str>,
        after: Option<&UserId>,
        limit: usize,
    ) -> Result<(Vec<(UserId, Profile)>, Option<String>)> {
        let query = query.map(str::trim).filter(|q| !q.is_empty());
        if query.is_some_and(|q| q.chars().count() > MAX_PEOPLE_QUERY_CHARS) {
            return Err(PynError::InvalidRequest(format!(
                "a search is at most {MAX_PEOPLE_QUERY_CHARS} characters"
            )));
        }
        let query = query.map(str::to_lowercase);
        let limit = limit.max(1);
        let mut found = self
            .store
            .search_people(query.as_deref(), after, limit + 1)
            .await?;
        let next = (found.len() > limit).then(|| {
            found.truncate(limit);
            found[limit - 1].to_string()
        });
        let mut out = Vec::with_capacity(found.len());
        for user in found {
            let profile = self.store.profile(&user).await?.public_view();
            out.push((user, profile));
        }
        Ok((out, next))
    }

    pub async fn profile_of(&self, user: &UserId) -> Result<Profile> {
        self.store.profile(user).await
    }

    /// The profile the credential may read of its own account: everything for a full-scope one, else the public view.
    pub async fn profile_for(&self, who: &Identity) -> Result<Profile> {
        let profile = self.store.profile(&who.user).await?;
        Ok(if who.is_full_scope() {
            profile
        } else {
            profile.public_view()
        })
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

impl AccessService {
    pub async fn org_profile_of(&self, org: &UserId) -> Result<OrgProfile> {
        self.org(org).await?;
        self.store.org_profile(org).await
    }

    /// An organization's profile when `owner` is an organization; `None` for a user.
    pub async fn org_profile_if_org(&self, owner: &UserId) -> Result<Option<OrgProfile>> {
        match self.org(owner).await {
            Ok(_) => Ok(Some(self.store.org_profile(owner).await?)),
            Err(PynError::OrgNotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Replaces the organization's description. Owners only, with a full-scope credential.
    pub async fn update_org_description(
        &self,
        actor: &Identity,
        org: &UserId,
        description: Option<String>,
    ) -> Result<OrgProfile> {
        let description = validate_org_description(description)?;
        self.require_org_edit(actor, org).await?;
        self.store
            .set_org_description(org, description.as_deref(), self.clock.now())
            .await?;
        self.store.org_profile(org).await
    }

    /// Records or clears the organization's avatar; the bytes are already stored.
    pub async fn update_org_avatar(
        &self,
        actor: &Identity,
        org: &UserId,
        avatar: Option<Avatar>,
    ) -> Result<OrgProfile> {
        self.require_org_edit(actor, org).await?;
        self.count_limited(
            &format!("org-avatar:{}", bounded_key(&org.as_str().to_lowercase())),
            AVATAR_CHANGES_PER_HOUR,
            Duration::hours(1),
        )
        .await?;
        self.store
            .set_org_avatar(org, avatar.as_ref(), self.clock.now())
            .await?;
        self.store.org_profile(org).await
    }

    /// The checks for editing an organization's profile: full scope, an owner, and not frozen.
    pub async fn require_org_edit(&self, actor: &Identity, org: &UserId) -> Result<()> {
        actor.require_full_scope()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.ensure_owner_writable(org).await
    }
}

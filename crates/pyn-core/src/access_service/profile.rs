//! Account profiles: reading them, and the signed-in user's own edits.

use super::*;
use crate::profile::{
    Avatar, OrgProfile, OrgProfileFields, Profile, ProfileFields, ProfileVisibility,
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

    /// Counts a signed-out directory request against the client address. No address, no limit, as for sign-up.
    pub async fn limit_signed_out_people(&self, client: Option<&str>) -> Result<()> {
        match client {
            Some(c) => {
                self.count_limited(
                    &format!("people:client:{}", bounded_key(c)),
                    self.config.limits.people_per_client,
                    PEOPLE_WINDOW,
                )
                .await
            }
            None => Ok(()),
        }
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
        actor.require_account_grant(ACCOUNT_PROFILE_WRITE)?;
        let fields = fields.validated()?;
        self.account_id(&actor.user).await?;
        self.store
            .save_profile(&actor.user, &fields, visibility.as_ref(), self.clock.now())
            .await?;
        self.store.profile(&actor.user).await
    }

    /// Records or clears the actor's avatar; the bytes are already stored.
    pub async fn update_avatar(&self, actor: &Identity, avatar: Option<Avatar>) -> Result<Profile> {
        actor.require_account_grant(ACCOUNT_PROFILE_WRITE)?;
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

    /// Replaces the organization's description, display name and url. Owners only, with a full-scope credential.
    /// The audit entry names the fields that changed.
    pub async fn update_org_fields(
        &self,
        actor: &Identity,
        org: &UserId,
        fields: OrgProfileFields,
    ) -> Result<OrgProfile> {
        let fields = fields.validated()?;
        self.require_org_edit(actor, org).await?;
        let old = self.store.org_profile(org).await?;
        self.store
            .set_org_fields(org, &fields, self.clock.now())
            .await?;
        let changed: Vec<&str> = [
            ("description", old.description != fields.description),
            ("display_name", old.display_name != fields.display_name),
            ("url", old.url != fields.url),
        ]
        .into_iter()
        .filter_map(|(name, changed)| changed.then_some(name))
        .collect();
        if !changed.is_empty() {
            let (scope, action) = (AuditScope::Org(org.clone()), AuditAction::OrgProfileUpdated);
            self.record(scope, actor.into(), action, changed.join(", "))
                .await?;
        }
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
        let had = self.store.org_profile(org).await?.avatar.is_some();
        self.store
            .set_org_avatar(org, avatar.as_ref(), self.clock.now())
            .await?;
        let action = match (&avatar, had) {
            (Some(_), _) => Some(AuditAction::OrgAvatarSet),
            (None, true) => Some(AuditAction::OrgAvatarRemoved),
            (None, false) => None,
        };
        if let Some(action) = action {
            let scope = AuditScope::Org(org.clone());
            self.record(scope, actor.into(), action, "avatar".into())
                .await?;
        }
        self.store.org_profile(org).await
    }

    /// Counts one icon change for `repo` against the avatar change limit.
    pub async fn limit_icon_changes(&self, repo: &RepoId) -> Result<()> {
        self.count_limited(
            &format!("repo-icon:{repo}"),
            AVATAR_CHANGES_PER_HOUR,
            Duration::hours(1),
        )
        .await
    }

    /// The checks for editing an organization's profile: a credential for the organization, an owner, and not frozen.
    pub async fn require_org_edit(&self, actor: &Identity, org: &UserId) -> Result<()> {
        actor.require_org_grant(org, ORG_ADMINISTRATION_WRITE)?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.ensure_owner_writable(org).await
    }
}

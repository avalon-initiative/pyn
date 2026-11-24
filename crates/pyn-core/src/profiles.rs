//! Public profiles and avatars: what a viewer may see of an account, and storing avatar bytes.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::io::AsyncReadExt;

use crate::access::{AccountRecord, Identity, OrgRole, Role};
use crate::access_service::AccessService;
use crate::error::{PynError, Result};
use crate::object::ObjectStore;
use crate::profile::{Avatar, OrgProfile, Profile, check_avatar};
use crate::repo::RepoRecord;
use crate::repositories::Repositories;
use crate::types::{AccountId, UserId};

/// An account's profile as one viewer may see it. Never carries an email address.
#[derive(Debug, Clone)]
pub struct PublicProfile {
    pub id: AccountId,
    pub user: UserId,
    pub created_at: DateTime<Utc>,
    pub profile: Profile,
    /// Whether the viewer is the account itself, who sees every field whatever its visibility.
    pub own: bool,
    /// The repositories the user owns that the viewer may see, each with the viewer's role.
    pub repositories: Vec<(RepoRecord, Option<Role>)>,
    /// The organizations the user belongs to that the viewer belongs to as well, with the viewer's role.
    pub organizations: Vec<(AccountRecord, OrgRole)>,
}

/// The viewer is the account itself through a full-scope credential.
fn is_owner(viewer: Option<&Identity>, user: &UserId) -> bool {
    viewer.is_some_and(|v| v.is_full_scope() && v.user == *user)
}

pub struct Profiles<'a> {
    pub access: &'a AccessService,
    pub repos: &'a Repositories,
    pub objects: &'a Arc<dyn ObjectStore>,
}

impl Profiles<'_> {
    /// The profile of the active user `user` for `viewer` (signed out when `None`). A missing user, an
    /// organization and a disabled or unapproved account are all `UserNotFound`.
    pub async fn public(&self, user: &str, viewer: Option<&Identity>) -> Result<PublicProfile> {
        let user = UserId::new(user);
        let account = self.access.active_user(&user).await?;
        let repositories = self.repos.visible_to(&account.user, viewer).await?;
        let mut organizations = Vec::new();
        if let Some(viewer) = viewer.filter(|v| v.is_full_scope()) {
            for (org, _) in self.access.orgs_of(&account.user).await? {
                if let Some(role) = self.access.org_role(&org.user, &viewer.user).await? {
                    organizations.push((org, role));
                }
            }
        }
        let own = is_owner(viewer, &account.user);
        let profile = self.access.profile_of(&account.user).await?;
        Ok(PublicProfile {
            profile: if own { profile } else { profile.public_view() },
            own,
            id: account.id,
            user: account.user,
            created_at: account.created_at,
            repositories,
            organizations,
        })
    }

    /// The repositories `user` starred that `viewer` may see, newest first, with when. A profile that hides its stars
    /// from the viewer lists none, like an account with none.
    pub async fn starred(
        &self,
        user: &str,
        viewer: Option<&Identity>,
    ) -> Result<Vec<(RepoRecord, Option<Role>, DateTime<Utc>)>> {
        let account = self.access.active_user(&UserId::new(user)).await?;
        let shown = is_owner(viewer, &account.user)
            || self
                .access
                .profile_of(&account.user)
                .await?
                .visibility
                .starred;
        if !shown {
            return Ok(Vec::new());
        }
        self.repos.starred_visible_to(&account.user, viewer).await
    }

    /// Stores the avatar bytes under their SHA-256 and makes them the actor's avatar. Replaced bytes stay in the
    /// store: it is shared and content-addressed, so another record may use the same object.
    pub async fn set_avatar(&self, actor: &Identity, bytes: Vec<u8>) -> Result<Profile> {
        actor.require_account_grant(crate::access::needs::ACCOUNT_PROFILE_WRITE)?;
        let avatar = self.store_avatar(bytes).await?;
        self.access.update_avatar(actor, Some(avatar)).await
    }

    pub async fn remove_avatar(&self, actor: &Identity) -> Result<Profile> {
        self.access.update_avatar(actor, None).await
    }

    /// The active user's avatar and its bytes; `AvatarNotFound` when there is none or `viewer` may not see it.
    pub async fn avatar(&self, user: &str, viewer: Option<&Identity>) -> Result<(Avatar, Vec<u8>)> {
        let user = UserId::new(user);
        let account = self.access.active_user(&user).await?;
        let none = || PynError::AvatarNotFound(account.user.to_string());
        let profile = self.access.profile_of(&account.user).await?;
        let profile = if is_owner(viewer, &account.user) {
            profile
        } else {
            profile.public_view()
        };
        let avatar = profile.avatar.ok_or_else(none)?;
        let bytes = self.avatar_bytes(&avatar).await?.ok_or_else(none)?;
        Ok((avatar, bytes))
    }

    async fn avatar_bytes(&self, avatar: &Avatar) -> Result<Option<Vec<u8>>> {
        let Some(read) = self.objects.read(&avatar.content, None).await? else {
            return Ok(None);
        };
        let mut bytes = Vec::with_capacity(avatar.size as usize);
        read.reader
            .take(crate::profile::MAX_AVATAR_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| PynError::Storage(e.to_string()))?;
        Ok(Some(bytes))
    }

    async fn store_avatar(&self, bytes: Vec<u8>) -> Result<Avatar> {
        let kind = check_avatar(&bytes)?;
        let size = bytes.len() as u64;
        let content = self.objects.put_chunk(bytes).await?;
        Ok(Avatar {
            content,
            kind,
            size,
        })
    }

    /// The organization's description and avatar record.
    pub async fn org_profile(&self, org: &str) -> Result<OrgProfile> {
        self.access.org_profile_of(&UserId::new(org)).await
    }

    /// Stores the avatar bytes and makes them the organization's avatar; owners only.
    pub async fn set_org_avatar(
        &self,
        actor: &Identity,
        org: &str,
        bytes: Vec<u8>,
    ) -> Result<OrgProfile> {
        let org = UserId::new(org);
        self.access.require_org_edit(actor, &org).await?;
        let avatar = self.store_avatar(bytes).await?;
        self.access
            .update_org_avatar(actor, &org, Some(avatar))
            .await
    }

    pub async fn remove_org_avatar(&self, actor: &Identity, org: &str) -> Result<OrgProfile> {
        self.access
            .update_org_avatar(actor, &UserId::new(org), None)
            .await
    }

    /// The organization's avatar and its bytes; `AvatarNotFound` when there is none.
    pub async fn org_avatar(&self, org: &str) -> Result<(Avatar, Vec<u8>)> {
        let org = UserId::new(org);
        let none = || PynError::AvatarNotFound(org.to_string());
        let avatar = self
            .access
            .org_profile_of(&org)
            .await?
            .avatar
            .ok_or_else(none)?;
        let bytes = self.avatar_bytes(&avatar).await?.ok_or_else(none)?;
        Ok((avatar, bytes))
    }
}

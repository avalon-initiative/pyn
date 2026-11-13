//! Public profiles and avatars: what a viewer may see of an account, and storing avatar bytes.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::io::AsyncReadExt;

use crate::access::{AccountRecord, Identity, OrgRole, Role};
use crate::access_service::AccessService;
use crate::error::{PynError, Result};
use crate::object::ObjectStore;
use crate::profile::{Avatar, Profile, check_avatar};
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
    /// The repositories the user owns that the viewer may see, each with the viewer's role.
    pub repositories: Vec<(RepoRecord, Option<Role>)>,
    /// The organizations the user belongs to that the viewer belongs to as well, with the viewer's role.
    pub organizations: Vec<(AccountRecord, OrgRole)>,
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
        Ok(PublicProfile {
            profile: self.access.profile_of(&account.user).await?,
            id: account.id,
            user: account.user,
            created_at: account.created_at,
            repositories,
            organizations,
        })
    }

    /// Stores the avatar bytes under their SHA-256 and makes them the actor's avatar. Replaced bytes stay in the
    /// store: it is shared and content-addressed, so another record may use the same object.
    pub async fn set_avatar(&self, actor: &Identity, bytes: Vec<u8>) -> Result<Profile> {
        actor.require_full_scope()?;
        let kind = check_avatar(&bytes)?;
        let size = bytes.len() as u64;
        let content = self.objects.put_chunk(bytes).await?;
        self.access
            .update_avatar(
                actor,
                Some(Avatar {
                    content,
                    kind,
                    size,
                }),
            )
            .await
    }

    pub async fn remove_avatar(&self, actor: &Identity) -> Result<Profile> {
        self.access.update_avatar(actor, None).await
    }

    /// The active user's avatar and its bytes; `AvatarNotFound` when there is none.
    pub async fn avatar(&self, user: &str) -> Result<(Avatar, Vec<u8>)> {
        let user = UserId::new(user);
        let account = self.access.active_user(&user).await?;
        let none = || PynError::AvatarNotFound(account.user.to_string());
        let avatar = self
            .access
            .profile_of(&account.user)
            .await?
            .avatar
            .ok_or_else(none)?;
        let read = self
            .objects
            .read(&avatar.content, None)
            .await?
            .ok_or_else(none)?;
        let mut bytes = Vec::with_capacity(avatar.size as usize);
        read.reader
            .take(crate::profile::MAX_AVATAR_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| PynError::Storage(e.to_string()))?;
        Ok((avatar, bytes))
    }
}

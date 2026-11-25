//! Repository icons: an image per repository, checked, stored and served like an account avatar.

use super::*;
use crate::profile::Avatar;
use crate::profiles::{image_bytes, store_image};

impl Repositories {
    /// Stores the image and makes it the repository's icon, or removes the icon when `bytes` is `None`. Who may do it
    /// is who may edit the repository; the change is a `repo_updated` audit event naming `icon`.
    pub async fn set_icon(
        &self,
        actor: &Identity,
        record: &RepoRecord,
        bytes: Option<Vec<u8>>,
    ) -> Result<RepoRecord> {
        self.require_owner_admin(actor, record).await?;
        self.access.ensure_owner_writable(&record.owner).await?;
        let icon = match bytes {
            Some(bytes) => Some(store_image(self.objects.as_ref(), bytes, "icon").await?),
            None if record.about.icon.is_none() => return Ok(record.clone()),
            None => None,
        };
        self.access.limit_icon_changes(&record.id).await?;
        let update = RepoUpdate {
            icon: Some(icon),
            ..RepoUpdate::default()
        };
        self.update(actor, record, update).await
    }

    /// The repository's icon and its bytes; `IconNotFound` when it has none. Reading the record is the access check.
    pub async fn icon(&self, record: &RepoRecord) -> Result<(Avatar, Vec<u8>)> {
        let none = || PynError::IconNotFound(record.address());
        let icon = record.about.icon.clone().ok_or_else(none)?;
        let bytes = image_bytes(self.objects.as_ref(), &icon)
            .await?
            .ok_or_else(none)?;
        Ok((icon, bytes))
    }
}

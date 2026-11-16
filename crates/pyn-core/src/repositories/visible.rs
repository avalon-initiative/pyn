//! One owner's repositories as a given viewer may see them.

use super::*;

impl Repositories {
    /// The repositories `owner` owns that the viewer may know about, by the same rule as `open_visible`
    /// (public, or the viewer holds a role), each with the viewer's role. Works without an identity.
    pub async fn visible_to(
        &self,
        owner: &UserId,
        who: Option<&Identity>,
    ) -> Result<Vec<(RepoRecord, Option<Role>)>> {
        let mut out = Vec::new();
        for record in self.meta.list_repos(Some(owner)).await? {
            if self.principal_for(&record, who).await?.is_none() {
                continue;
            }
            let role = self.access.role_for(&record.id, who).await?;
            out.push((record, role));
        }
        Ok(out)
    }
}

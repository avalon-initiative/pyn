//! Local branches and their upstreams in the store of a workspace with `local_store` on, as git keeps them: `refs/heads`
//! are independent of the remote-tracking refs, and `branch.<name>.remote` and `.merge` say which remote branch a local
//! one follows.

use anyhow::{Result, bail};
use pyn_git::{ConfigFile, Expect};
use pyn_object::ObjectId;

use crate::local_store::LocalStore;

const REMOTE: &str = "origin";
const HEADS: &str = "refs/heads/";
const REMOTES: &str = "refs/remotes/origin/";

impl LocalStore {
    fn config_file(&self) -> std::path::PathBuf {
        self.repo().git().git_dir().join("config")
    }

    /// The server branch a local branch follows.
    pub fn upstream(&self, branch: &str) -> Result<Option<String>> {
        let config = ConfigFile::read(&self.config_file())?;
        let remote = config.values("branch", Some(branch), "remote");
        let merge = config.values("branch", Some(branch), "merge");
        Ok(match (remote.last(), merge.last()) {
            (Some(r), Some(m)) if r == REMOTE => m.strip_prefix(HEADS).map(str::to_owned),
            _ => None,
        })
    }

    pub fn set_upstream(&self, branch: &str, upstream: Option<&str>) -> Result<()> {
        let path = self.config_file();
        let mut config = ConfigFile::read(&path)?;
        config.remove_section("branch", Some(branch));
        if let Some(up) = upstream {
            config.set("branch", Some(branch), "remote", REMOTE)?;
            config.set("branch", Some(branch), "merge", &format!("{HEADS}{up}"))?;
        }
        config.write(&path)?;
        Ok(())
    }

    /// Stores made before local branches were independent follow the server branch of the same name.
    pub(crate) fn adopt_upstreams(&self) -> Result<()> {
        let path = self.config_file();
        let config = ConfigFile::read(&path)?;
        if !config.values("pyn", None, "upstreams").is_empty() {
            return Ok(());
        }
        let refs = self.repo().refs();
        for (name, _) in self.heads()? {
            if refs.resolve(&format!("{REMOTES}{name}"))?.is_some() {
                self.set_upstream(&name, Some(&name))?;
            }
        }
        let mut config = ConfigFile::read(&path)?;
        config.set("pyn", None, "upstreams", "true")?;
        config.write(&path)?;
        Ok(())
    }

    /// The local branches and their tips, by name.
    pub fn heads(&self) -> Result<Vec<(String, ObjectId)>> {
        Ok(self
            .repo()
            .refs()
            .list(HEADS)?
            .into_iter()
            .map(|(n, id)| (n[HEADS.len()..].to_owned(), id))
            .collect())
    }

    /// The remote-tracking branches and their tips, by name without `origin/`.
    pub fn remote_heads(&self) -> Result<Vec<(String, ObjectId)>> {
        Ok(self
            .repo()
            .refs()
            .list(REMOTES)?
            .into_iter()
            .map(|(n, id)| (n[REMOTES.len()..].to_owned(), id))
            .collect())
    }

    pub fn remote_tip(&self, name: &str) -> Result<Option<ObjectId>> {
        Ok(self.repo().refs().resolve(&format!("{REMOTES}{name}"))?)
    }

    pub fn create_branch(&self, name: &str, at: &ObjectId) -> Result<()> {
        let full = format!("{HEADS}{name}");
        pyn_git::check_ref_name(&full)
            .map_err(|_| anyhow::anyhow!("'{name}' is not a valid branch name"))?;
        if self.repo().refs().resolve(&full)?.is_some() {
            bail!("a branch named '{name}' already exists");
        }
        self.repo()
            .refs()
            .update_checked(&full, Expect::Absent, at)?;
        Ok(())
    }

    pub fn delete_branch(&self, name: &str) -> Result<()> {
        self.repo()
            .refs()
            .delete(&format!("{HEADS}{name}"), Expect::Any)?;
        let path = self.config_file();
        let mut config = ConfigFile::read(&path)?;
        if config.remove_section("branch", Some(name)) {
            config.write(&path)?;
        }
        Ok(())
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        let Some(tip) = self.tip(old)? else {
            bail!("no branch named '{old}'");
        };
        self.create_branch(new, &tip)?;
        let upstream = self.upstream(old)?;
        self.delete_branch(old)?;
        if let Some(up) = upstream {
            self.set_upstream(new, Some(&up))?;
        }
        Ok(())
    }

    /// The server's tip the branch follows, or for a branch with no upstream the newest commit of it the server has.
    pub fn published_or_base(&self, branch: &str) -> Result<Option<ObjectId>> {
        if let Some(up) = self.upstream(branch)? {
            return Ok(self.repo().refs().resolve(&format!("{REMOTES}{up}"))?);
        }
        let remotes = self.remote_heads()?.into_iter().map(|(_, id)| id).collect();
        let known = self.reach(remotes)?;
        let mut at = self.tip(branch)?;
        while let Some(id) = at {
            if known.contains(&id) {
                return Ok(Some(id));
            }
            at = self.repo().commit(&id)?.parents.first().copied();
        }
        Ok(None)
    }

    /// The remote-tracking branches `name` could mean, as git's `checkout <name>` looks for them.
    pub fn remote_matches(&self, name: &str) -> Result<Vec<String>> {
        Ok(self
            .remote_tip(name)?
            .map(|_| name.to_owned())
            .into_iter()
            .collect())
    }
}

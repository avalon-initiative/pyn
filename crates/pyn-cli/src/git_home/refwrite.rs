//! Moving refs the way git does: a checked write through the ref's lock file, plus the reflog lines
//! `core.logAllRefUpdates` asks for.

use anyhow::{Context, Result, bail};
use pyn_git::{Expect, Ref, ReflogEntry};
use pyn_object::{ObjectId, Signature};

use super::GitHome;
use super::stage::local_offset_minutes;

pub(super) const BRANCHES: &str = "refs/heads/";
pub(super) const TAGS: &str = "refs/tags/";

enum LogPolicy {
    Never,
    Default,
    Always,
}

impl GitHome {
    pub(super) fn zero_id(&self) -> ObjectId {
        let alg = self.repo.algorithm();
        ObjectId::from_bytes(alg, &vec![0; alg.digest_len()])
            .expect("a zero id has the digest length")
    }

    /// Who a reflog line names: the committer identity, or the login name when git has none.
    fn log_identity(&self) -> Signature {
        if let Ok(who) = super::stage::identity(self, "committer") {
            return who;
        }
        let user = super::stage::env("USER").unwrap_or_else(|| "unknown".into());
        Signature {
            email: format!("{user}@localhost").into_bytes(),
            name: user.into_bytes(),
            time: chrono::Utc::now().timestamp(),
            offset_minutes: local_offset_minutes(),
        }
    }

    fn log_policy(&self) -> LogPolicy {
        match self.repo.config().get("core.logallrefupdates") {
            Some(v) if v.eq_ignore_ascii_case("always") => LogPolicy::Always,
            Some(_) if !self.repo.bool_setting("core.logallrefupdates", true) => LogPolicy::Never,
            _ => LogPolicy::Default,
        }
    }

    /// Appends a reflog line for `name`; the file is created where git would create it, or always with `force`.
    pub(super) fn log_ref(
        &self,
        name: &str,
        old: Option<&ObjectId>,
        new: &ObjectId,
        message: &str,
        force: bool,
    ) -> Result<()> {
        let create = force
            || match self.log_policy() {
                LogPolicy::Never => false,
                LogPolicy::Always => true,
                LogPolicy::Default => {
                    name == "HEAD"
                        || ["refs/heads/", "refs/remotes/", "refs/notes/"]
                            .iter()
                            .any(|p| name.starts_with(p))
                }
            };
        let entry = ReflogEntry {
            old: old.copied().unwrap_or_else(|| self.zero_id()),
            new: *new,
            who: self.log_identity(),
            message: message.to_string(),
        };
        self.repo.refs().append_reflog(name, &entry, create)?;
        Ok(())
    }

    /// The ref `HEAD` names when it is on a branch, even an unborn one.
    pub(super) fn head_target(&self) -> Result<Option<String>> {
        Ok(match self.repo.git().refs().head()? {
            Ref::Symbolic(target) => Some(target),
            Ref::Direct(_) => None,
        })
    }

    /// Creates `name`, which must not exist yet.
    pub(super) fn create_ref(&self, name: &str, id: &ObjectId, message: &str) -> Result<()> {
        self.repo
            .refs()
            .update_checked(name, Expect::Absent, id)
            .with_context(|| format!("creating {name}"))?;
        self.log_ref(name, None, id, message, false)
    }

    /// Moves `name` from `old` to `new`, failing if something else moved it first.
    pub(super) fn move_ref(
        &self,
        name: &str,
        old: Option<&ObjectId>,
        new: &ObjectId,
        message: &str,
    ) -> Result<()> {
        let expect = old.map_or(Expect::Absent, |o| Expect::Id(*o));
        self.repo
            .refs()
            .update_checked(name, expect, new)
            .with_context(|| format!("updating {name}"))?;
        self.log_ref(name, old, new, message, false)?;
        if self.head_target()?.as_deref() == Some(name) {
            self.log_ref("HEAD", old, new, message, false)?;
        }
        Ok(())
    }

    /// Points `HEAD` at a branch (or at a commit, detached), logging the move from the commit it was at.
    pub(super) fn set_head(
        &self,
        branch_ref: Option<&str>,
        commit: &ObjectId,
        message: &str,
    ) -> Result<()> {
        let old = self.repo.head_commit()?;
        match branch_ref {
            Some(target) => self.repo.refs().set_symbolic("HEAD", target)?,
            None => self
                .repo
                .refs()
                .update_checked("HEAD", Expect::Any, commit)?,
        }
        self.log_ref("HEAD", old.as_ref(), commit, message, false)
    }

    pub(super) fn branch_ref(name: &str) -> String {
        format!("{BRANCHES}{name}")
    }

    /// The commit a branch is at, if the branch exists.
    pub(super) fn branch_commit(&self, name: &str) -> Result<Option<ObjectId>> {
        let full = Self::branch_ref(name);
        if pyn_git::check_ref_name(&full).is_err() {
            return Ok(None);
        }
        Ok(self.repo.refs().resolve(&full)?)
    }

    pub(super) fn check_branch_name(name: &str) -> Result<()> {
        if name.starts_with('-')
            || name == "HEAD"
            || pyn_git::check_ref_name(&Self::branch_ref(name)).is_err()
        {
            bail!("'{name}' is not a valid branch name");
        }
        Ok(())
    }

    /// Refuses a ref that would need a folder where another ref is a file, or the reverse.
    pub(super) fn check_ref_room(&self, name: &str) -> Result<()> {
        for (existing, _) in self.repo.refs().list("refs/")? {
            if name.starts_with(&format!("{existing}/"))
                || existing.starts_with(&format!("{name}/"))
            {
                bail!("'{name}' exists or is in the way of '{existing}'; cannot use it as a ref");
            }
        }
        Ok(())
    }

    /// Branches checked out in this repository's work trees, with the path of each work tree.
    pub(super) fn checked_out(&self) -> Result<Vec<(String, std::path::PathBuf)>> {
        let common = self.repo.git().common_dir();
        let mut found = Vec::new();
        let mut add = |git_dir: &std::path::Path, tree: std::path::PathBuf| {
            if let Ok(text) = std::fs::read_to_string(git_dir.join("HEAD"))
                && let Some(target) = text.trim().strip_prefix("ref: ")
                && let Some(name) = target.strip_prefix(BRANCHES)
            {
                found.push((name.to_string(), tree));
            }
        };
        let main_tree = common
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        add(common, main_tree);
        if let Ok(rd) = std::fs::read_dir(common.join("worktrees")) {
            for entry in rd.flatten() {
                let dir = entry.path();
                let tree = std::fs::read_to_string(dir.join("gitdir"))
                    .map(|t| {
                        std::path::PathBuf::from(t.trim())
                            .parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_default()
                    })
                    .unwrap_or_else(|_| dir.clone());
                add(&dir, tree);
            }
        }
        Ok(found)
    }

    /// The work tree, other than this one, that has `branch` checked out.
    pub(super) fn used_elsewhere(&self, branch: &str) -> Result<Option<std::path::PathBuf>> {
        let here = std::fs::canonicalize(self.repo.root()?)?;
        Ok(self
            .checked_out()?
            .into_iter()
            .find(|(b, tree)| {
                b == branch && std::fs::canonicalize(tree).unwrap_or_else(|_| tree.clone()) != here
            })
            .map(|(_, tree)| tree))
    }

    /// The work tree, this one included, that has `branch` checked out.
    pub(super) fn checked_out_path(&self, branch: &str) -> Result<Option<std::path::PathBuf>> {
        Ok(self
            .checked_out()?
            .into_iter()
            .find(|(b, _)| b == branch)
            .map(|(_, tree)| tree))
    }
}

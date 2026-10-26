//! Partial clones: the `--filter` flags and the promisor settings a clone or fetch records.

use anyhow::{Result, anyhow};
use clap::Args;
use pyn_git::{Config, ConfigFile, Error, ObjectFilter, promisor_remotes};

use super::GitHome;

#[derive(Debug, Args, Default, PartialEq, Eq)]
pub struct FilterFlags {
    /// Leave out objects the remote can supply later: `blob:none`, `blob:limit=<n>`, `tree:<depth>`, or `combine:<a>+<b>`.
    #[arg(long, value_name = "SPEC")]
    pub(super) filter: Vec<String>,
}

impl FilterFlags {
    pub(super) fn given(&self) -> bool {
        !self.filter.is_empty()
    }

    /// The filter the options name (several are combined), or git's complaint about a bad spec.
    pub(super) fn parse(&self) -> Result<Option<ObjectFilter>> {
        ObjectFilter::from_specs(&self.filter).map_err(|e| match e {
            Error::Unsupported(what) => anyhow!("{what} is not built"),
            _ => match self.filter.iter().find(|s| ObjectFilter::parse(s).is_err()) {
                Some(bad) => anyhow!("invalid filter-spec '{bad}'"),
                None => anyhow!("invalid filter-spec '{}'", self.filter.join("+")),
            },
        })
    }
}

/// Whether `remote` is a promisor remote according to `config`.
pub(super) fn is_promisor(config: &Config, remote: &str) -> bool {
    promisor_remotes(config).iter().any(|n| n == remote)
}

/// The filter stored for `remote` by an earlier partial clone or fetch.
pub(super) fn stored_filter(config: &Config, remote: &str) -> Option<ObjectFilter> {
    let spec = config.get(&format!("remote.{remote}.partialclonefilter"))?;
    ObjectFilter::parse(spec).ok()
}

/// Records `remote` as a promisor remote, with `filter` as its default unless it has one already.
pub(super) fn register_in(
    file: &mut ConfigFile,
    config: &Config,
    remote: &str,
    filter: &ObjectFilter,
) -> Result<()> {
    if !is_promisor(config, remote) {
        file.set("core", None, "repositoryformatversion", "1")?;
        file.set("remote", Some(remote), "promisor", "true")?;
    }
    if config
        .get(&format!("remote.{remote}.partialclonefilter"))
        .is_none()
    {
        file.set("remote", Some(remote), "partialclonefilter", &filter.spec())?;
    }
    Ok(())
}

pub(super) fn register(git: &GitHome, remote: &str, filter: &ObjectFilter) -> Result<()> {
    let path = git.repo.git().common_dir().join("config");
    let mut file = ConfigFile::read(&path)?;
    register_in(&mut file, git.config(), remote, filter)?;
    file.write(&path)?;
    Ok(())
}

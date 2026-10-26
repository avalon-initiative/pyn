//! Downloading objects a partial clone lacks from its promisor remotes, on demand.

use std::path::Path;

use pyn_git::{Remote, Repository, promisor_remotes};
use pyn_gitwire::{FetchRequest, NoHaves, UploadPack};
use pyn_object::ObjectId;

use crate::connect::{Connector, user_config};
use crate::env::Env;
use crate::error::{Error, Result};
use crate::exchange::fetch_pack_into;

/// Makes [`pyn_git::ObjectDb`] download missing promisor objects through this crate's transports.
pub fn install() {
    pyn_git::set_promisor_fetch(fetch_missing);
}

fn fetch_missing(common_dir: &Path, ids: &[ObjectId]) -> std::result::Result<(), String> {
    fetch_from_promisors(common_dir, ids).map_err(|e| e.to_string())
}

fn fetch_from_promisors(common_dir: &Path, ids: &[ObjectId]) -> Result<()> {
    let repo = Repository::open(common_dir)?;
    let mut config = user_config(&Env::process())?;
    config.extend(repo.config().clone());
    let names = promisor_remotes(&config);
    let mut last = Error::Io(std::io::Error::other("no promisor remote is configured"));
    for name in names {
        let Some(url) = Remote::read(&config, &name).and_then(|r| r.urls.first().cloned()) else {
            continue;
        };
        let attempt = (|| -> Result<()> {
            let mut conn = Connector::new(config.clone(), Env::process()).connect(&url, false)?;
            let mut up = UploadPack::open(&mut *conn.transport, repo.algorithm())?;
            let mut req = FetchRequest::new(ids.to_vec());
            req.no_progress = true;
            if up.supports_filter() {
                req.filter = Some("blob:none".into());
            }
            fetch_pack_into(&mut up, &repo, &req, &mut NoHaves, true, &mut |_| {})?;
            Ok(())
        })();
        match attempt {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

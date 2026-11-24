//! `pyn watch`, `pyn unwatch` and `pyn watching`: which repositories the signed-in account hears about.

use anyhow::Result;
use clap::ValueEnum;
use pyn_proto as api;

use crate::address;
use crate::client::Api;
use crate::table;
use crate::time;
use crate::transport::Method;

#[derive(Clone, Copy, ValueEnum)]
pub enum Level {
    /// Every event of the repository.
    All,
    /// Only lock events: acquired, released, expired and forced.
    Locks,
    /// Nothing, even when you contribute.
    Ignore,
}

impl From<Level> for api::WatchLevel {
    fn from(l: Level) -> Self {
        match l {
            Level::All => Self::All,
            Level::Locks => Self::Locks,
            Level::Ignore => Self::Ignore,
        }
    }
}

fn word(l: api::WatchLevel) -> &'static str {
    match l {
        api::WatchLevel::All => "all",
        api::WatchLevel::Locks => "locks",
        api::WatchLevel::Ignore => "ignore",
    }
}

pub fn watch(api: &Api, name: &str, level: Level) -> Result<()> {
    let (owner, short) = address::split_repo(name)?;
    let body = api::SetWatchRequest {
        level: level.into(),
    };
    let state: api::WatchState = api
        .send(
            api.request(Method::PUT, &format!("/v1/repos/{owner}/{short}/watch"))
                .json(&body),
        )?
        .json()?;
    println!(
        "watching {name}: {} ({} watchers)",
        state.level.map_or("none", word),
        state.watchers
    );
    Ok(())
}

pub fn unwatch(api: &Api, name: &str) -> Result<()> {
    let (owner, short) = address::split_repo(name)?;
    let state: api::WatchState = api
        .send(api.request(Method::DELETE, &format!("/v1/repos/{owner}/{short}/watch")))?
        .json()?;
    println!("no longer watching {name} ({} watchers)", state.watchers);
    Ok(())
}

pub fn watching(api: &Api) -> Result<()> {
    let watched: Vec<api::WatchedRepo> = api.send(api.get("/v1/me/watching"))?.json()?;
    let rows: Vec<Vec<String>> = watched
        .into_iter()
        .map(|w| {
            vec![
                word(w.level).to_string(),
                time::local(w.updated_at),
                w.repo.watcher_count.to_string(),
                format!("{}/{}", w.repo.owner, w.repo.name),
            ]
        })
        .collect();
    table::show(
        &["LEVEL", "SINCE", "WATCHERS", "REPOSITORY"],
        &rows,
        "not watching any repository",
    );
    Ok(())
}

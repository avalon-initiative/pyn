//! `checkout <name>` and `switch <name>` creating a branch that tracks the one remote-tracking branch of that name.

use anyhow::{Result, bail};
use pyn_git::Remote;

use super::GitHome;
use super::fetch::remotes_in_order;

/// A remote-tracking branch that `<name>` would be created from.
pub(super) struct Guess {
    pub remote: String,
    pub tracking: String,
}

/// The remote-tracking ref a fetch refspec maps `src` onto.
fn map_through(spec: &str, src: &str) -> Option<String> {
    let spec = spec.strip_prefix('+').unwrap_or(spec);
    let (from, to) = spec.split_once(':')?;
    match (from.split_once('*'), to.split_once('*')) {
        (Some((fp, fs)), Some((tp, ts))) => src
            .strip_prefix(fp)
            .and_then(|mid| mid.strip_suffix(fs))
            .map(|mid| format!("{tp}{mid}{ts}")),
        (None, None) if from == src => Some(to.to_owned()),
        _ => None,
    }
}

/// Whether guessing is on: the flag, else `checkout.guess`, else true.
pub(super) fn enabled(git: &GitHome, flag: Option<bool>) -> bool {
    flag.unwrap_or_else(|| git.repo.bool_setting("checkout.guess", true))
}

/// The single remote-tracking branch `name` maps to, preferring `checkout.defaultRemote`; refuses a tie.
pub(super) fn guess(git: &GitHome, name: &str) -> Result<Option<Guess>> {
    let source = format!("refs/heads/{name}");
    let mut found = Vec::new();
    for remote in remotes_in_order(git) {
        let Some(r) = Remote::read(git.config(), &remote) else {
            continue;
        };
        let hit = r
            .fetch
            .iter()
            .find_map(|spec| map_through(spec, &source))
            .filter(|t| t.starts_with("refs/remotes/"));
        if let Some(tracking) = hit
            && git.repo.refs().resolve(&tracking)?.is_some()
        {
            found.push(Guess { remote, tracking });
        }
    }
    let preferred = git.config().get("checkout.defaultremote");
    if let Some(i) = found
        .iter()
        .position(|g| Some(g.remote.as_str()) == preferred)
    {
        return Ok(Some(found.swap_remove(i)));
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop()),
        n => bail!("'{name}' matched multiple ({n}) remote tracking branches"),
    }
}

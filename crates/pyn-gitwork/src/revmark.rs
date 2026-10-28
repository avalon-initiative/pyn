//! The `@{...}` revision suffixes: reflog positions (`HEAD@{2}`), the branch checked out before (`@{-1}`) and a
//! branch's `@{upstream}` and `@{push}`.

use pyn_git::{Ref, map_ref};
use pyn_object::ObjectId;

use crate::error::{Error, Result};
use crate::repo::Repo;

/// A revision split at its first `@{...}`: what precedes it, what is inside and what follows.
pub(crate) struct Mark<'a> {
    pub base: &'a str,
    pub inner: &'a str,
    pub rest: &'a str,
}

pub(crate) fn split_mark(spec: &str) -> Option<Mark<'_>> {
    let at = spec.find("@{")?;
    if spec[..at].contains(['~', '^']) {
        return None;
    }
    let close = at + 2 + spec[at + 2..].find('}')?;
    Some(Mark {
        base: &spec[..at],
        inner: &spec[at + 2..close],
        rest: &spec[close + 1..],
    })
}

fn is_zero(id: &ObjectId) -> bool {
    id.as_bytes().iter().all(|b| *b == 0)
}

fn short_name(name: &str) -> &str {
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/remotes/"))
        .unwrap_or(name)
}

impl Repo {
    /// The commit `<base>@{<inner>}` names, before any `~` or `^` steps.
    pub(crate) fn resolve_mark(&self, mark: &Mark<'_>, spec: &str) -> Result<ObjectId> {
        let bad = || Error::Revision(format!("bad revision '{spec}'"));
        let inner = mark.inner;
        if let Some(n) = inner.strip_prefix('-') {
            let n: usize = n.parse().map_err(|_| bad())?;
            if !mark.base.is_empty() || n == 0 {
                return Err(bad());
            }
            let from = self
                .previous_checkout(n)?
                .ok_or_else(|| Error::Revision(format!("bad revision '{spec}'")))?;
            return self.resolve(&from);
        }
        if !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit()) {
            let n: usize = inner.parse().map_err(|_| bad())?;
            let name = self.mark_ref(mark.base)?.ok_or_else(bad)?;
            return self.reflog_position(&name, n);
        }
        let lower = inner.to_ascii_lowercase();
        let tracking = if lower == "u" || lower == "upstream" {
            Some(false)
        } else if lower == "push" {
            Some(true)
        } else {
            None
        };
        let Some(push) = tracking else {
            return Err(bad());
        };
        let branch = self.mark_branch(mark.base)?;
        let target = if push {
            self.push_ref(&branch)?
        } else {
            self.upstream_ref(&branch)?
        };
        self.refs()
            .resolve(&target)?
            .ok_or_else(|| Error::Revision(format!("bad revision '{spec}'")))
    }

    /// The ref whose reflog a `<base>@{n}` reads: `base` itself, or the current branch (`HEAD` when detached).
    fn mark_ref(&self, base: &str) -> Result<Option<String>> {
        if base.is_empty() {
            return Ok(Some(match self.refs().head()? {
                Ref::Symbolic(target) => target,
                Ref::Direct(_) => "HEAD".to_owned(),
            }));
        }
        self.expand_ref_name(base)
    }

    /// The short name of the branch `base` stands for; the current branch when it is empty or `HEAD`.
    fn mark_branch(&self, base: &str) -> Result<String> {
        let name = if base.is_empty() || base == "HEAD" || base == "@" {
            match self.refs().head()? {
                Ref::Symbolic(target) => target,
                Ref::Direct(_) => {
                    return Err(Error::Revision("HEAD does not point to a branch".into()));
                }
            }
        } else {
            self.expand_ref_name(base)?
                .ok_or_else(|| Error::Revision(format!("no such branch: '{base}'")))?
        };
        name.strip_prefix("refs/heads/")
            .map(str::to_owned)
            .ok_or_else(|| Error::Revision(format!("no such branch: '{base}'")))
    }

    /// The value of `name` `n` reflog lines back (`0` is the current one); past the oldest line only the value
    /// before it, if there is one.
    pub fn reflog_position(&self, name: &str, n: usize) -> Result<ObjectId> {
        let log = self.refs().read_reflog(name)?;
        let shown = short_name(name);
        if log.is_empty() {
            return Err(Error::Revision(format!("log for '{shown}' is empty")));
        }
        if n < log.len() {
            return Ok(log[log.len() - 1 - n].new);
        }
        if n == log.len() && !is_zero(&log[0].old) {
            return Ok(log[0].old);
        }
        Err(Error::Revision(format!(
            "log for '{shown}' only has {} entries",
            log.len()
        )))
    }

    /// The `n`th previous `HEAD` position by `checkout`/`switch` (`@{-n}`): the branch name, or a commit id when
    /// that was detached. `None` when `HEAD` has not moved that many times.
    pub fn previous_checkout(&self, n: usize) -> Result<Option<String>> {
        let mut seen = 0;
        for entry in self.refs().read_reflog("HEAD")?.iter().rev() {
            let Some(rest) = entry.message.strip_prefix("checkout: moving from ") else {
                continue;
            };
            let Some((from, _)) = rest.split_once(" to ") else {
                continue;
            };
            seen += 1;
            if seen == n {
                return Ok(Some(from.to_owned()));
            }
        }
        Ok(None)
    }

    /// The full name of the ref a short name stands for, trying git's lookup order.
    pub fn expand_ref_name(&self, name: &str) -> Result<Option<String>> {
        if name.is_empty() {
            return Ok(None);
        }
        if let Some(mark) = split_mark(name) {
            return self.expand_mark_ref(&mark);
        }
        let name = if name == "@" { "HEAD" } else { name };
        let candidates = [
            name.to_string(),
            format!("refs/{name}"),
            format!("refs/tags/{name}"),
            format!("refs/heads/{name}"),
            format!("refs/remotes/{name}"),
            format!("refs/remotes/{name}/HEAD"),
        ];
        for candidate in candidates {
            if candidate != "HEAD" && !candidate.starts_with("refs/") {
                continue;
            }
            if pyn_git::check_ref_name(&candidate).is_err() && candidate != "HEAD" {
                continue;
            }
            if self.refs().resolve(&candidate)?.is_some() {
                return Ok(Some(candidate));
            }
        }
        Ok(None)
    }

    /// The existing ref `@{u}`, `@{push}` or `@{-N}` (after `base`) stands for; none for other suffixes.
    fn expand_mark_ref(&self, mark: &Mark<'_>) -> Result<Option<String>> {
        if !mark.rest.is_empty() {
            return Ok(None);
        }
        let lower = mark.inner.to_ascii_lowercase();
        let target = if let Some(n) = mark.inner.strip_prefix('-') {
            let Ok(n) = n.parse::<usize>() else {
                return Ok(None);
            };
            if !mark.base.is_empty() || n == 0 {
                return Ok(None);
            }
            return match self.previous_checkout(n)? {
                Some(name) => self.expand_ref_name(&name),
                None => Ok(None),
            };
        } else if lower == "u" || lower == "upstream" {
            self.upstream_ref(&self.mark_branch(mark.base)?)?
        } else if lower == "push" {
            self.push_ref(&self.mark_branch(mark.base)?)?
        } else {
            return Ok(None);
        };
        Ok(self.refs().resolve(&target)?.map(|_| target))
    }

    /// The remote-tracking (or local) ref `branch` is set to follow, as `@{upstream}` reads it.
    pub fn upstream_ref(&self, branch: &str) -> Result<String> {
        let tracking = pyn_git::tracking(self.config(), branch);
        let (Some(remote), Some(merge)) = (tracking.remote, tracking.merge.first()) else {
            return Err(Error::Revision(format!(
                "no upstream configured for branch '{branch}'"
            )));
        };
        if remote == "." {
            return Ok(merge.clone());
        }
        map_ref(&self.fetch_specs(&remote), merge).ok_or_else(|| {
            Error::Revision(format!(
                "upstream branch '{merge}' not stored as a remote-tracking branch"
            ))
        })
    }

    /// The remote-tracking ref `git push` from `branch` would update, as `@{push}` reads it.
    pub fn push_ref(&self, branch: &str) -> Result<String> {
        let config = self.config();
        let remote = config
            .get(&format!("branch.{branch}.pushremote"))
            .or_else(|| config.get("remote.pushdefault"))
            .map(str::to_owned)
            .or_else(|| pyn_git::tracking(config, branch).remote)
            .ok_or_else(|| {
                Error::Revision(format!("branch '{branch}' has no remote for pushing"))
            })?;
        let local = format!("refs/heads/{branch}");
        let pushes = config.get_all(&format!("remote.{remote}.push"));
        let dest = if pushes.is_empty() {
            None
        } else {
            let specs: Vec<_> = pushes
                .iter()
                .filter_map(|s| pyn_git::Refspec::parse(s).ok())
                .collect();
            Some(map_ref(&specs, &local).ok_or_else(|| {
                Error::Revision(format!(
                    "push refspecs for '{remote}' do not include '{branch}'"
                ))
            })?)
        };
        let tracking_for = |dest: &str| {
            map_ref(&self.fetch_specs(&remote), dest).ok_or_else(|| {
                Error::Revision(format!(
                    "push destination '{dest}' on remote '{remote}' has no local tracking branch"
                ))
            })
        };
        if let Some(dest) = dest {
            return tracking_for(&dest);
        }
        match config.get("push.default").unwrap_or("simple") {
            "nothing" => Err(Error::Revision(
                "push has no destination (push.default is 'nothing')".into(),
            )),
            "matching" | "current" => tracking_for(&local),
            "upstream" | "tracking" => self.upstream_ref(branch),
            _ => {
                let up = self.upstream_ref(branch)?;
                let cur = tracking_for(&local)?;
                if up == cur {
                    Ok(cur)
                } else {
                    Err(Error::Revision(
                        "cannot resolve 'simple' push to a single destination".into(),
                    ))
                }
            }
        }
    }

    fn fetch_specs(&self, remote: &str) -> Vec<pyn_git::Refspec> {
        self.config()
            .get_all(&format!("remote.{remote}.fetch"))
            .into_iter()
            .filter_map(|s| pyn_git::Refspec::parse(s).ok())
            .collect()
    }
}

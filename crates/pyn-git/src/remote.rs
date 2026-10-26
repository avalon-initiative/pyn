//! Remotes and branch tracking in git config: `remote.<name>.*` and `branch.<name>.remote|merge`.

use std::collections::BTreeSet;

use crate::config::Config;
use crate::config_file::ConfigFile;
use crate::error::Result;
use crate::refs::check_name;
use crate::refspec::Refspec;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Remote {
    pub name: String,
    pub urls: Vec<String>,
    pub push_urls: Vec<String>,
    /// The `fetch` refspecs as written.
    pub fetch: Vec<String>,
}

impl Remote {
    /// The remote `name` as `config` defines it, or `None` when no `remote.<name>.*` key exists.
    pub fn read(config: &Config, name: &str) -> Option<Self> {
        let key = |k: &str| format!("remote.{name}.{k}");
        let owned = |v: Vec<&str>| v.into_iter().map(str::to_owned).collect::<Vec<_>>();
        if !remote_names(config).iter().any(|n| n == name) {
            return None;
        }
        Some(Self {
            name: name.to_owned(),
            urls: owned(config.get_all(&key("url"))),
            push_urls: owned(config.get_all(&key("pushurl"))),
            fetch: owned(config.get_all(&key("fetch"))),
        })
    }

    /// The URLs a push uses: `pushurl` when set, otherwise the fetch URLs.
    pub fn push_targets(&self) -> &[String] {
        if self.push_urls.is_empty() {
            &self.urls
        } else {
            &self.push_urls
        }
    }

    /// The parsed fetch refspecs, skipping any that do not parse.
    pub fn fetch_specs(&self) -> Vec<Refspec> {
        self.fetch
            .iter()
            .filter_map(|s| Refspec::parse(s).ok())
            .collect()
    }
}

/// Every remote name with any `remote.<name>.*` key, sorted.
pub fn remote_names(config: &Config) -> Vec<String> {
    let names: BTreeSet<&str> = config
        .entries()
        .iter()
        .filter(|e| e.section == "remote")
        .filter_map(|e| e.subsection.as_deref())
        .collect();
    names.into_iter().map(str::to_owned).collect()
}

/// Remotes objects may be missing for: `extensions.partialClone`, then those with `remote.<name>.promisor`.
pub fn promisor_remotes(config: &Config) -> Vec<String> {
    let mut out: Vec<String> = config
        .get("extensions.partialclone")
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
        .into_iter()
        .collect();
    for name in remote_names(config) {
        let on = config.get_bool(&format!("remote.{name}.promisor"));
        if matches!(on, Ok(Some(true))) && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

/// Git's rule: the name must work as a ref component under `refs/remotes/`.
pub fn is_valid_remote_name(name: &str) -> bool {
    !name.is_empty() && check_name(&format!("refs/remotes/{name}/test")).is_ok()
}

/// What `branch.<name>.remote` and `branch.<name>.merge` say.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Tracking {
    pub remote: Option<String>,
    pub merge: Vec<String>,
}

pub fn tracking(config: &Config, branch: &str) -> Tracking {
    Tracking {
        remote: config
            .get(&format!("branch.{branch}.remote"))
            .map(str::to_owned),
        merge: config
            .get_all(&format!("branch.{branch}.merge"))
            .into_iter()
            .map(str::to_owned)
            .collect(),
    }
}

/// Branches whose `branch.<b>.remote` is `remote`, sorted.
pub fn branches_tracking(config: &Config, remote: &str) -> Vec<String> {
    let names: BTreeSet<&str> = config
        .entries()
        .iter()
        .filter(|e| {
            e.section == "branch" && e.key == "remote" && e.value.as_deref() == Some(remote)
        })
        .filter_map(|e| e.subsection.as_deref())
        .collect();
    names.into_iter().map(str::to_owned).collect()
}

/// Points `branch` at `remote` and, when given, the upstream ref `merge`.
pub fn set_tracking(
    file: &mut ConfigFile,
    branch: &str,
    remote: &str,
    merge: Option<&str>,
) -> Result<()> {
    file.set("branch", Some(branch), "remote", remote)?;
    match merge {
        Some(m) => file.set("branch", Some(branch), "merge", m),
        None => Ok(()),
    }
}

pub fn unset_tracking(file: &mut ConfigFile, branch: &str) {
    for key in ["remote", "merge"] {
        file.unset_all("branch", Some(branch), key);
    }
}

/// Defines `name` with one URL and the default fetch refspec, as `git remote add`.
pub fn add_remote(file: &mut ConfigFile, name: &str, url: &str) -> Result<()> {
    file.add("remote", Some(name), "url", url)?;
    file.add(
        "remote",
        Some(name),
        "fetch",
        &Refspec::default_fetch(name).to_string(),
    )
}

/// Drops `remote.<name>` and everything that points at it: tracking, `pushRemote` and `remote.pushDefault`.
pub fn remove_remote(file: &mut ConfigFile, name: &str) {
    for branch in file.subsections("branch") {
        let points_here = file.values("branch", Some(&branch), "remote") == [name];
        if points_here {
            for key in ["remote", "merge", "pushremote"] {
                file.unset_all("branch", Some(&branch), key);
            }
        } else {
            file.unset_where("branch", Some(&branch), "pushremote", |v| v == name);
        }
    }
    file.unset_where("remote", None, "pushdefault", |v| v == name);
    file.remove_section("remote", Some(name));
}

/// Renames `remote.<old>` to `<new>`, moving fetch refspecs under `refs/remotes/<old>/` and repointing
/// branches. Returns the refspecs it could not update.
pub fn rename_remote(file: &mut ConfigFile, old: &str, new: &str) -> Result<Vec<String>> {
    file.rename_section("remote", old, new)?;
    let old_prefix = format!("refs/remotes/{old}/");
    let new_prefix = format!("refs/remotes/{new}/");
    let specs = file.values("remote", Some(new), "fetch");
    file.unset_all("remote", Some(new), "fetch");
    let mut kept = Vec::new();
    for spec in specs {
        let moved = Refspec::parse(&spec).ok().and_then(|mut r| {
            let dst = r.dst.as_deref()?.strip_prefix(&old_prefix)?.to_owned();
            r.dst = Some(format!("{new_prefix}{dst}"));
            Some(r.to_string())
        });
        if moved.is_none() {
            kept.push(spec.clone());
        }
        file.add(
            "remote",
            Some(new),
            "fetch",
            moved.as_deref().unwrap_or(&spec),
        )?;
    }
    for branch in file.subsections("branch") {
        for key in ["remote", "pushRemote"] {
            file.replace_where("branch", Some(&branch), key, new, |v| v == old)?;
        }
    }
    file.replace_where("remote", None, "pushDefault", new, |v| v == old)?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> ConfigFile {
        ConfigFile::parse(text).unwrap()
    }

    #[test]
    fn reads_remotes_with_several_urls() {
        let c = Config::parse(
            b"[remote \"b\"]\n\turl = u1\n\turl = u2\n\tpushurl = p\n\tfetch = +refs/heads/*:refs/remotes/b/*\n[remote \"a\"]\n\ttagopt = --no-tags\n",
        )
        .unwrap();
        assert_eq!(remote_names(&c), ["a", "b"]);
        let b = Remote::read(&c, "b").unwrap();
        assert_eq!(b.urls, ["u1", "u2"]);
        assert_eq!(b.push_targets(), ["p"]);
        assert_eq!(b.fetch_specs().len(), 1);
        assert!(Remote::read(&c, "nope").is_none());
        assert!(Remote::read(&c, "a").unwrap().push_targets().is_empty());
    }

    #[test]
    fn remote_names_follow_the_ref_rules() {
        for ok in ["origin", "-x", "a/b", "up.stream"] {
            assert!(is_valid_remote_name(ok), "{ok}");
        }
        for bad in ["", "a b", "a..b", ".a", "a.lock", "a~"] {
            assert!(!is_valid_remote_name(bad), "{bad}");
        }
    }

    #[test]
    fn add_and_remove_leave_other_content_alone() {
        let mut f = file("# top\n[core]\n\tbare = false\n");
        add_remote(&mut f, "o", "u").unwrap();
        set_tracking(&mut f, "main", "o", Some("refs/heads/main")).unwrap();
        assert_eq!(
            f.render(),
            "# top\n[core]\n\tbare = false\n[remote \"o\"]\n\turl = u\n\tfetch = +refs/heads/*:refs/remotes/o/*\n[branch \"main\"]\n\tremote = o\n\tmerge = refs/heads/main\n"
        );
        remove_remote(&mut f, "o");
        assert_eq!(f.render(), "# top\n[core]\n\tbare = false\n");
    }

    #[test]
    fn remove_keeps_the_branch_keys_that_are_not_tracking() {
        let mut f = file(
            "[remote \"o\"]\n\turl = u\n[remote]\n\tpushDefault = o\n[branch \"m\"]\n\tremote = o\n\tmerge = x\n\tpushRemote = o\n\trebase = true\n[branch \"k\"]\n\tremote = .\n\tpushRemote = o\n",
        );
        remove_remote(&mut f, "o");
        assert_eq!(
            f.render(),
            "[branch \"m\"]\n\trebase = true\n[branch \"k\"]\n\tremote = .\n"
        );
    }

    #[test]
    fn rename_moves_refspecs_and_reports_the_ones_it_cannot() {
        let mut f = file(
            "[remote \"a\"]\n\turl = u\n\tfetch = +refs/heads/*:refs/remotes/a/*\n\tfetch = +refs/heads/q:refs/other/q\n\ttagopt = --no-tags\n[branch \"b\"]\n\tremote = a\n\tpushRemote = a\n[remote]\n\tpushDefault = a\n",
        );
        let kept = rename_remote(&mut f, "a", "z").unwrap();
        assert_eq!(kept, ["+refs/heads/q:refs/other/q"]);
        assert_eq!(
            f.render(),
            "[remote \"z\"]\n\turl = u\n\ttagopt = --no-tags\n\tfetch = +refs/heads/*:refs/remotes/z/*\n\tfetch = +refs/heads/q:refs/other/q\n[branch \"b\"]\n\tremote = z\n\tpushRemote = z\n[remote]\n\tpushDefault = z\n"
        );
    }

    #[test]
    fn tracking_reads_and_clears() {
        let c = Config::parse(b"[branch \"m\"]\n\tremote = o\n\tmerge = refs/heads/m\n").unwrap();
        assert_eq!(
            tracking(&c, "m"),
            Tracking {
                remote: Some("o".into()),
                merge: vec!["refs/heads/m".into()]
            }
        );
        assert_eq!(branches_tracking(&c, "o"), ["m"]);
        let mut f = file("[branch \"m\"]\n\tremote = o\n\tmerge = refs/heads/m\n");
        unset_tracking(&mut f, "m");
        assert_eq!(f.render(), "");
    }
}

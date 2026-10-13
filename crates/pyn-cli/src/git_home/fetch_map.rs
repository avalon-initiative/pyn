//! Which remote refs a fetch takes and where each goes: git's ref map, built without any I/O.

use pyn_git::Refspec;
use pyn_gitwire::RemoteRef;
use pyn_object::ObjectId;

/// How a fetched ref is recorded in `FETCH_HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Merge,
    NotForMerge,
    /// Updated locally but not recorded in `FETCH_HEAD`.
    Ignore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub id: ObjectId,
    /// The local ref the value is stored in, if any.
    pub local: Option<String>,
    pub force: bool,
    pub status: Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagMode {
    Default,
    All,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MapError {
    Missing(String),
    Conflict {
        first: String,
        second: String,
        local: String,
    },
}

impl std::fmt::Display for MapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "couldn't find remote ref {name}"),
            Self::Conflict {
                first,
                second,
                local,
            } => write!(f, "Cannot fetch both {first} and {second} to {local}"),
        }
    }
}

impl std::error::Error for MapError {}

const NAME_RULES: [(&str, &str); 6] = [
    ("", ""),
    ("refs/", ""),
    ("refs/tags/", ""),
    ("refs/heads/", ""),
    ("refs/remotes/", ""),
    ("refs/remotes/", "/HEAD"),
];

/// The remote ref a possibly abbreviated `name` means, by git's order of rules.
pub fn find_ref<'a>(refs: &'a [RemoteRef], name: &str) -> Option<&'a RemoteRef> {
    refs.iter()
        .filter(|r| r.id.is_some())
        .filter_map(|r| {
            NAME_RULES
                .iter()
                .position(|(pre, post)| r.name == format!("{pre}{name}{post}"))
                .map(|rule| (rule, r))
        })
        .min_by_key(|(rule, _)| *rule)
        .map(|(_, r)| r)
}

/// Whether `short` is `full` or an abbreviation of it under git's name rules.
pub fn names_match(short: &str, full: &str) -> bool {
    NAME_RULES
        .iter()
        .any(|(pre, post)| full == format!("{pre}{short}{post}"))
}

/// The full local ref a destination written as `heads/x`, `tags/x` or `x` means.
pub fn local_ref(dst: &str) -> Option<String> {
    if dst.is_empty() {
        None
    } else if dst.starts_with("refs/") {
        Some(dst.to_owned())
    } else if ["heads/", "tags/", "remotes/"]
        .iter()
        .any(|p| dst.starts_with(p))
    {
        Some(format!("refs/{dst}"))
    } else {
        Some(format!("refs/heads/{dst}"))
    }
}

/// The refs one refspec selects. A name that matches nothing is an error unless `missing_ok`.
pub fn select(
    refs: &[RemoteRef],
    spec: &Refspec,
    missing_ok: bool,
) -> Result<Vec<Entry>, MapError> {
    if spec.negative {
        return Ok(Vec::new());
    }
    if spec.is_pattern() {
        return Ok(refs
            .iter()
            .filter_map(|r| {
                let id = r.id.filter(|_| spec.matches_src(&r.name))?;
                Some(Entry {
                    name: r.name.clone(),
                    id,
                    local: spec.map(&r.name),
                    force: spec.force,
                    status: Status::NotForMerge,
                })
            })
            .collect());
    }
    let name = if spec.src.is_empty() {
        "HEAD"
    } else {
        &spec.src
    };
    match find_ref(refs, name) {
        Some(r) => Ok(vec![Entry {
            name: r.name.clone(),
            id: r.id.expect("find_ref skips unborn refs"),
            local: spec.dst.as_deref().and_then(local_ref),
            force: spec.force,
            status: Status::NotForMerge,
        }]),
        None if missing_ok => Ok(Vec::new()),
        None => Err(MapError::Missing(name.to_owned())),
    }
}

pub struct Inputs<'a> {
    pub remote_refs: &'a [RemoteRef],
    /// Refspecs named on the command line; they replace the configured ones.
    pub cmdline: &'a [Refspec],
    pub configured: &'a [Refspec],
    /// `branch.<current>.merge`, when the current branch tracks this remote.
    pub merge: &'a [String],
    /// Whether the current branch has any `merge` setting, whatever its remote.
    pub has_merge: bool,
    pub tags: TagMode,
    /// Tags to follow, given the entries chosen so far; asked only when `tags` is `Default`.
    pub follow: &'a dyn Fn(&[Entry]) -> Vec<Entry>,
}

pub struct Map {
    pub entries: Vec<Entry>,
    /// Some selected ref is stored locally, so tags that point into the fetched history follow.
    pub autotags: bool,
}

fn excluded(negatives: &[&Refspec], name: &str) -> bool {
    negatives.iter().any(|n| n.matches_src(name))
}

/// Git's `get_ref_map`: the entries to fetch, in the order they are reported.
pub fn build(inp: &Inputs<'_>) -> Result<Map, MapError> {
    let negatives: Vec<&Refspec> = inp
        .cmdline
        .iter()
        .chain(inp.configured)
        .filter(|s| s.negative)
        .collect();
    let mut entries = Vec::new();
    let mut opportunistic = Vec::new();
    let mut autotags = false;
    let has_dst = |s: &Refspec| s.dst.as_deref().is_some_and(|d| !d.is_empty());
    if !inp.cmdline.is_empty() {
        for spec in inp.cmdline {
            autotags |= has_dst(spec);
            entries.extend(select(inp.remote_refs, spec, false)?);
        }
        for e in &mut entries {
            e.status = Status::Merge;
        }
        let picked: Vec<RemoteRef> = entries
            .iter()
            .map(|e| RemoteRef {
                name: e.name.clone(),
                id: Some(e.id),
                peeled: None,
                symref_target: None,
            })
            .collect();
        for spec in inp.configured {
            for mut e in select(&picked, spec, true)? {
                e.status = Status::Ignore;
                opportunistic.push(e);
            }
        }
    } else if !inp.configured.is_empty() || !inp.merge.is_empty() {
        for (i, spec) in inp.configured.iter().enumerate() {
            let found = select(inp.remote_refs, spec, false)?;
            autotags |= has_dst(spec);
            let first_exact = i == 0 && !inp.has_merge && !spec.is_pattern();
            entries.extend(found.into_iter().map(|mut e| {
                if first_exact {
                    e.status = Status::Merge;
                }
                e
            }));
        }
        for wanted in inp.merge {
            let name = if wanted.starts_with("refs/") {
                wanted.clone()
            } else {
                format!("refs/heads/{wanted}")
            };
            match entries.iter_mut().find(|e| e.name == name) {
                Some(e) => e.status = Status::Merge,
                None => {
                    let spec = Refspec {
                        force: false,
                        negative: false,
                        src: name,
                        dst: None,
                    };
                    for mut e in select(inp.remote_refs, &spec, true)? {
                        e.status = Status::Merge;
                        entries.push(e);
                    }
                }
            }
        }
    } else {
        let spec = Refspec {
            force: false,
            negative: false,
            src: "HEAD".into(),
            dst: None,
        };
        entries = select(inp.remote_refs, &spec, false)?;
        entries[0].status = Status::Merge;
    }
    if inp.tags == TagMode::All {
        let all = Refspec::parse("refs/tags/*:refs/tags/*").expect("a valid refspec");
        entries.extend(select(inp.remote_refs, &all, false)?);
    } else if inp.tags == TagMode::Default && autotags {
        let followed = (inp.follow)(&entries);
        entries.extend(followed);
    }
    entries.extend(opportunistic);
    entries.retain(|e| !excluded(&negatives, &e.name));
    Ok(Map {
        entries: dedupe(entries)?,
        autotags,
    })
}

/// Drops later entries that store into a ref an earlier one does; two different sources for one ref is an error.
pub fn dedupe(entries: Vec<Entry>) -> Result<Vec<Entry>, MapError> {
    let mut kept: Vec<Entry> = Vec::new();
    for e in entries {
        let Some(local) = &e.local else {
            kept.push(e);
            continue;
        };
        match kept.iter_mut().find(|k| k.local.as_ref() == Some(local)) {
            Some(prev) if prev.name != e.name => {
                return Err(MapError::Conflict {
                    first: prev.name.clone(),
                    second: e.name,
                    local: local.clone(),
                });
            }
            Some(_) => {}
            None => kept.push(e),
        }
    }
    Ok(kept)
}

/// Tags to follow along with a fetch: those the remote has and this repository lacks whose object, or the object
/// they point at, is already here or about to arrive.
pub fn follow_tags(
    remote_refs: &[RemoteRef],
    local_refs: &dyn Fn(&str) -> bool,
    have: &dyn Fn(&ObjectId) -> bool,
    taken: &[Entry],
) -> Vec<Entry> {
    let present = |id: &ObjectId| have(id) || taken.iter().any(|e| e.id == *id);
    remote_refs
        .iter()
        .filter(|r| r.name.starts_with("refs/tags/") && !local_refs(&r.name))
        .filter_map(|r| {
            let id = r.id?;
            (present(&id) || r.peeled.is_some_and(|p| present(&p))).then(|| Entry {
                name: r.name.clone(),
                id,
                local: Some(r.name.clone()),
                force: false,
                status: Status::NotForMerge,
            })
        })
        .collect()
}

/// The part of a ref name git shows in a fetch summary: without `refs/heads/`, `refs/tags/` or `refs/remotes/`.
pub fn pretty(name: &str) -> &str {
    ["refs/heads/", "refs/tags/", "refs/remotes/"]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyn_object::Algorithm;

    fn id(n: u8) -> ObjectId {
        ObjectId::from_bytes(Algorithm::Sha1, &[n; 20]).unwrap()
    }

    fn remote(name: &str, n: u8) -> RemoteRef {
        RemoteRef {
            name: name.into(),
            id: Some(id(n)),
            peeled: None,
            symref_target: None,
        }
    }

    fn refs() -> Vec<RemoteRef> {
        vec![
            remote("HEAD", 1),
            remote("refs/heads/feat", 2),
            remote("refs/heads/main", 1),
            remote("refs/tags/v1", 3),
        ]
    }

    fn spec(s: &str) -> Refspec {
        Refspec::parse(s).unwrap()
    }

    fn inputs<'a>(
        remote_refs: &'a [RemoteRef],
        cmdline: &'a [Refspec],
        configured: &'a [Refspec],
    ) -> Inputs<'a> {
        Inputs {
            remote_refs,
            cmdline,
            configured,
            merge: &[],
            has_merge: false,
            tags: TagMode::Default,
            follow: &|_| Vec::new(),
        }
    }

    #[test]
    fn short_names_follow_gits_rule_order() {
        let mut r = refs();
        r.push(remote("refs/tags/main", 9));
        assert_eq!(find_ref(&r, "main").unwrap().name, "refs/tags/main");
        assert_eq!(find_ref(&r, "feat").unwrap().name, "refs/heads/feat");
        assert_eq!(find_ref(&r, "HEAD").unwrap().name, "HEAD");
        assert!(find_ref(&r, "nope").is_none());
    }

    #[test]
    fn local_names_default_to_branches() {
        assert_eq!(local_ref("x").as_deref(), Some("refs/heads/x"));
        assert_eq!(local_ref("tags/x").as_deref(), Some("refs/tags/x"));
        assert_eq!(local_ref("refs/a/x").as_deref(), Some("refs/a/x"));
        assert_eq!(local_ref(""), None);
    }

    #[test]
    fn configured_patterns_map_every_branch_and_mark_nothing_for_merge() {
        let r = refs();
        let cfg = [spec("+refs/heads/*:refs/remotes/o/*")];
        let map = build(&inputs(&r, &[], &cfg)).unwrap();
        let got: Vec<_> = map
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.local.as_deref(), e.status))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "refs/heads/feat",
                    Some("refs/remotes/o/feat"),
                    Status::NotForMerge
                ),
                (
                    "refs/heads/main",
                    Some("refs/remotes/o/main"),
                    Status::NotForMerge
                ),
            ]
        );
        assert!(map.autotags && map.entries[0].force);
    }

    #[test]
    fn an_exact_first_refspec_or_the_merge_setting_marks_for_merge() {
        let r = refs();
        let cfg = [spec("refs/heads/main:refs/remotes/o/main")];
        let map = build(&inputs(&r, &[], &cfg)).unwrap();
        assert_eq!(map.entries[0].status, Status::Merge);
        let cfg = [spec("+refs/heads/*:refs/remotes/o/*")];
        let merge = ["refs/heads/main".to_owned()];
        let map = build(&Inputs {
            merge: &merge,
            has_merge: true,
            ..inputs(&r, &[], &cfg)
        })
        .unwrap();
        let by_name = |n: &str| map.entries.iter().find(|e| e.name == n).unwrap().status;
        assert_eq!(by_name("refs/heads/main"), Status::Merge);
        assert_eq!(by_name("refs/heads/feat"), Status::NotForMerge);
    }

    #[test]
    fn no_refspecs_at_all_fetches_head() {
        let r = refs();
        let map = build(&inputs(&r, &[], &[])).unwrap();
        assert_eq!(map.entries.len(), 1);
        assert_eq!(map.entries[0].name, "HEAD");
        assert_eq!(map.entries[0].status, Status::Merge);
        let no_head = [remote("refs/heads/main", 1)];
        assert_eq!(
            build(&inputs(&no_head, &[], &[])).err(),
            Some(MapError::Missing("HEAD".into()))
        );
    }

    #[test]
    fn command_line_names_merge_and_update_the_configured_tracking_ref_quietly() {
        let r = refs();
        let cmd = [spec("main")];
        let cfg = [spec("+refs/heads/*:refs/remotes/o/*")];
        let map = build(&inputs(&r, &cmd, &cfg)).unwrap();
        assert_eq!(map.entries.len(), 2);
        assert_eq!(map.entries[0].local, None);
        assert_eq!(map.entries[0].status, Status::Merge);
        assert_eq!(map.entries[1].local.as_deref(), Some("refs/remotes/o/main"));
        assert_eq!(map.entries[1].status, Status::Ignore);
        assert!(!map.autotags);
        assert_eq!(
            build(&inputs(&r, &[spec("gone")], &cfg)).err(),
            Some(MapError::Missing("gone".into()))
        );
    }

    #[test]
    fn two_sources_for_one_local_ref_are_an_error_and_the_same_one_is_merged() {
        let r = refs();
        let cmd = [spec("main:x"), spec("feat:x")];
        assert!(matches!(
            build(&inputs(&r, &cmd, &[])).err(),
            Some(MapError::Conflict { .. })
        ));
        let cmd = [spec("main:x"), spec("+main:x")];
        let map = build(&inputs(&r, &cmd, &[])).unwrap();
        assert_eq!(map.entries.len(), 1);
        assert!(!map.entries[0].force);
    }

    #[test]
    fn negative_refspecs_drop_matching_refs() {
        let r = refs();
        let cfg = [
            spec("+refs/heads/*:refs/remotes/o/*"),
            spec("^refs/heads/feat"),
        ];
        let map = build(&inputs(&r, &[], &cfg)).unwrap();
        assert_eq!(map.entries.len(), 1);
    }

    #[test]
    fn tags_follow_when_their_target_is_here_or_arriving() {
        let mut r = refs();
        r.push(RemoteRef {
            peeled: Some(id(2)),
            ..remote("refs/tags/v2", 7)
        });
        r.push(remote("refs/tags/lost", 8));
        let taken = [Entry {
            name: "refs/heads/feat".into(),
            id: id(2),
            local: None,
            force: false,
            status: Status::NotForMerge,
        }];
        let have = |i: &ObjectId| *i == id(3);
        let local = |n: &str| n == "refs/tags/old";
        let got: Vec<_> = follow_tags(&r, &local, &have, &taken)
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(got, ["refs/tags/v1", "refs/tags/v2"]);
    }

    #[test]
    fn summary_names_drop_the_common_prefixes() {
        assert_eq!(pretty("refs/heads/a"), "a");
        assert_eq!(pretty("refs/remotes/o/a"), "o/a");
        assert_eq!(pretty("refs/notes/a"), "refs/notes/a");
        assert_eq!(pretty("HEAD"), "HEAD");
    }
}

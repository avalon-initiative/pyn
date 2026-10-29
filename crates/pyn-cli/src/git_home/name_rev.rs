//! `pyn name-rev` on a git repository: a commit named relative to the nearest ref that reaches it.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_object::{Kind, ObjectId, Tag};

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes, kind_of, peel, prefix, shorten_ref};

/// Weight of a step through a merge's later parent, so such names lose to first-parent ones.
const MERGE_WEIGHT: usize = 65535;

/// Find symbolic names for commits from the refs that reach them.
#[derive(Debug, Args)]
pub struct NameRevArgs {
    /// Use only tags to name commits.
    #[arg(long)]
    tags: bool,
    /// Print only the name.
    #[arg(long)]
    name_only: bool,
    /// Fail when a commit has no name.
    #[arg(long)]
    no_undefined: bool,
    /// Print an abbreviated commit id for a commit that has no name.
    #[arg(long)]
    always: bool,
    /// Commits to name.
    commits: Vec<String>,
}

struct Tip {
    annotated: bool,
    exact: String,
    target: ObjectId,
    commit: ObjectId,
    name: String,
    at: i64,
    from_tag: bool,
}

struct RevName {
    tip: String,
    at: i64,
    generation: usize,
    distance: usize,
    from_tag: bool,
}

pub(super) fn name_rev(git: &GitHome, cwd: &Path, args: NameRevArgs) -> Result<()> {
    let tips = tips(git, &args)?;
    let names = name_all(git, &tips)?;
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let mut out = String::new();
    for arg in &args.commits {
        let Some(id) = resolver.object(arg)? else {
            eprintln!("Could not get sha1 for {arg}. Skipping.");
            continue;
        };
        let kind = kind_of(git, &id)?;
        let exact = tips.iter().find(|t| t.target == id && t.annotated);
        let by_commit = match kind {
            Some(Kind::Commit | Kind::Tag) => peel(git, &id, Some(Kind::Commit))?,
            _ => None,
        };
        let found = match (exact, by_commit.and_then(|c| names.get(&c))) {
            (Some(t), _) => Some(t.exact.clone()),
            (None, Some(n)) if n.generation == 0 => Some(n.tip.clone()),
            (None, Some(n)) => Some(format!(
                "{}~{}",
                n.tip.strip_suffix("^0").unwrap_or(&n.tip),
                n.generation
            )),
            (None, None) => None,
        };
        let name = match found {
            Some(name) => name,
            None if !args.no_undefined => "undefined".to_string(),
            None if args.always => git.repo.abbreviate(&id)?,
            None => {
                if !args.name_only {
                    out.push_str(&format!("{arg} "));
                }
                emit_bytes(out.as_bytes());
                bail!("cannot describe '{}'", id.to_hex());
            }
        };
        if args.name_only {
            out.push_str(&format!("{name}\n"));
        } else {
            out.push_str(&format!("{arg} {name}\n"));
        }
    }
    emit_bytes(out.as_bytes());
    Ok(())
}

fn tips(git: &GitHome, args: &NameRevArgs) -> Result<Vec<Tip>> {
    let mut tips = Vec::new();
    for (full, id) in git.repo.refs().list("refs/")? {
        let from_tag = full.starts_with("refs/tags/");
        if args.tags && !from_tag {
            continue;
        }
        let Some(object) = git.repo.objects().read_opt(&id)? else {
            continue;
        };
        let Some(commit) = peel(git, &id, Some(Kind::Commit))? else {
            continue;
        };
        let annotated = object.kind == Kind::Tag;
        let at = if annotated {
            Tag::parse(git.algorithm(), &object.data)?
                .tagger
                .map_or(0, |t| t.time)
        } else {
            git.repo.commit(&commit)?.committer.time
        };
        let mut name = if args.tags && args.name_only {
            shorten_ref(git, &full, false)
        } else {
            full.strip_prefix("refs/heads/")
                .or_else(|| full.strip_prefix("refs/"))
                .unwrap_or(&full)
                .to_string()
        };
        let exact = name.clone();
        if annotated {
            name.push_str("^0");
        }
        tips.push(Tip {
            annotated,
            exact,
            target: id,
            commit,
            name,
            at,
            from_tag,
        });
    }
    tips.sort_by_key(|t| (!t.from_tag, t.at));
    Ok(tips)
}

fn is_better(old: &RevName, at: i64, distance: usize, from_tag: bool) -> bool {
    if from_tag && old.from_tag {
        return old.at > at;
    }
    if old.from_tag != from_tag {
        return from_tag;
    }
    distance < old.distance
}

fn name_all(git: &GitHome, tips: &[Tip]) -> Result<HashMap<ObjectId, RevName>> {
    let mut names: HashMap<ObjectId, RevName> = HashMap::new();
    for tip in tips {
        if let Some(old) = names.get(&tip.commit)
            && !is_better(old, tip.at, 0, tip.from_tag)
        {
            continue;
        }
        names.insert(
            tip.commit,
            RevName {
                tip: tip.name.clone(),
                at: tip.at,
                generation: 0,
                distance: 0,
                from_tag: tip.from_tag,
            },
        );
        let mut stack = vec![tip.commit];
        while let Some(commit) = stack.pop() {
            let (base, generation, distance) = {
                let n = &names[&commit];
                (n.tip.clone(), n.generation, n.distance)
            };
            let mut next = Vec::new();
            for (i, parent) in git.repo.commit(&commit)?.parents.into_iter().enumerate() {
                let (name, g, d) = if i > 0 {
                    let stem = base.strip_suffix("^0").unwrap_or(&base);
                    let name = if generation > 0 {
                        format!("{stem}~{generation}^{}", i + 1)
                    } else {
                        format!("{stem}^{}", i + 1)
                    };
                    (name, 0, distance + MERGE_WEIGHT)
                } else {
                    (base.clone(), generation + 1, distance + 1)
                };
                if let Some(old) = names.get(&parent)
                    && !is_better(old, tip.at, d, tip.from_tag)
                {
                    continue;
                }
                names.insert(
                    parent,
                    RevName {
                        tip: name,
                        at: tip.at,
                        generation: g,
                        distance: d,
                        from_tag: tip.from_tag,
                    },
                );
                next.push(parent);
            }
            stack.extend(next.into_iter().rev());
        }
    }
    Ok(names)
}

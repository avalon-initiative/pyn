//! Merging several heads at once: the heads reduced to the independent ones, git's merge title for several refs, and the
//! `octopus` strategy, which merges the heads one after another and may leave a conflict only in the last.

use anyhow::{Result, bail};
use pyn_gitwork::Pathspec;
use pyn_object::ObjectId;

use super::{
    Concluding, Done, Landing, Merging, Refusal, Run, Tried, apply_merge_as, describe, fail_start,
    index_changes, refuse_apply, say,
};
use crate::git_home::GitHome;
use crate::git_home::strategy::{Settings, Strategy, Which};
use crate::git_home::treemerge::{Labels, Resolved, merge_trees_with, result_tree};
use crate::git_home::worktree::Blocked;
use crate::merging::MergeArgs;

const KINDS: [(&str, &str, &str); 4] = [
    ("branch", "branch ", "branches "),
    (
        "remote-tracking branch",
        "remote-tracking branch ",
        "remote-tracking branches ",
    ),
    ("tag", "tag ", "tags "),
    ("commit", "commit ", "commits "),
];

/// git's merge title for refs of one origin: `HEAD, branches 'a' and 'b', tag 'v1'`; each item is a kind and a quoted name.
pub(in crate::git_home) fn group(head: bool, items: &[(String, String)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if head {
        parts.push("HEAD".to_string());
    }
    for (kind, one, many) in KINDS {
        let names: Vec<&str> = items
            .iter()
            .filter(|(k, _)| k == kind)
            .map(|(_, n)| n.as_str())
            .collect();
        parts.push(match names.as_slice() {
            [] => continue,
            [only] => format!("{one}{only}"),
            [rest @ .., last] => format!("{many}{} and {last}", rest.join(", ")),
        });
    }
    parts.join(", ")
}

/// The title of a merge of several named refs, as `merge` words it.
pub(super) fn describe_all(git: &GitHome, specs: &[String]) -> String {
    let items: Vec<(String, String)> = specs
        .iter()
        .map(|spec| {
            let text = describe(git, spec);
            let (kind, name) = KINDS
                .iter()
                .map(|(k, _, _)| *k)
                .rev()
                .find_map(|k| text.strip_prefix(&format!("{k} ")).map(|n| (k, n)))
                .unwrap_or(("commit", text.as_str()));
            (kind.to_string(), name.to_string())
        })
        .collect();
    group(false, &items)
}

/// The heads that are neither `HEAD`'s ancestors nor ancestors of another head, each once, in the order given.
pub(in crate::git_home) fn reduce(
    git: &GitHome,
    ours: &ObjectId,
    given: &[(String, ObjectId)],
) -> Result<Vec<(String, ObjectId)>> {
    let mut fresh: Vec<(String, ObjectId)> = Vec::new();
    for (spec, id) in given {
        if !git.is_ancestor(id, ours)? && !fresh.iter().any(|(_, seen)| seen == id) {
            fresh.push((spec.clone(), *id));
        }
    }
    let mut kept = Vec::new();
    for (spec, id) in &fresh {
        let mut covered = false;
        for (_, other) in &fresh {
            if other != id && git.is_ancestor(id, other)? {
                covered = true;
                break;
            }
        }
        if !covered {
            kept.push((spec.clone(), *id));
        }
    }
    Ok(kept)
}

/// Whether `HEAD` is left out of the parents: several heads, one of which already contains it, and `--no-ff` not given.
pub(super) fn drops_head(
    git: &GitHome,
    args: &MergeArgs,
    ours: &ObjectId,
    kept: &[(String, ObjectId)],
) -> Result<bool> {
    if kept.len() < 2 || args.no_ff {
        return Ok(false);
    }
    for (_, id) in kept {
        if git.is_ancestor(ours, id)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// One strategy's attempt at several heads: only `octopus` takes them (and `ours`, handled by the caller).
pub(super) fn attempt(m: &Merging, strategy: &Strategy, run: Run) -> Result<Tried> {
    let f = m.finish;
    let (git, ours, spec, ours_tree) = (f.git, f.ours, f.spec, f.ours_tree);
    let kind = strategy.kind;
    if kind != Which::Octopus {
        eprintln!("error: Not handling anything other than two heads merge.");
        return fail_start(git, &ours, spec, kind, false);
    }
    let changed = index_changes(git, m.opened, Some(&ours_tree))?;
    if !changed.is_empty() {
        let blocked = Blocked {
            local: changed,
            ..Default::default()
        };
        let refusal = Refusal {
            blocked,
            index_differs: true,
            index_check: true,
        };
        return refuse_apply(git, &ours, spec, kind, refusal);
    }
    let talk = run != Run::Silent;
    let (mut reached, mut tree, mut fast) = (vec![ours], ours_tree, true);
    let mut last = None;
    for (i, (name, id)) in f.kept.iter().enumerate() {
        let common = git.merge_bases_from(&reached, id)?;
        if common.contains(id) {
            if talk {
                say(&format!("Already up to date with {name}"));
            }
            continue;
        }
        if fast && common == reached {
            if talk {
                println!("Fast-forwarding to: {name}");
            }
            (reached, tree) = (vec![*id], git.repo.commit(id)?.tree);
            continue;
        }
        fast = false;
        if talk {
            println!("Trying simple merge with {name}");
        }
        let [base] = common.as_slice() else {
            eprintln!("fatal: Unable to find a single common commit with {name}");
            return fail_start(git, &ours, spec, kind, false);
        };
        let (base_tree, theirs_tree) = (git.repo.commit(base)?.tree, git.repo.commit(id)?.tree);
        let merged = merge_trees_with(
            git,
            [Some(&base_tree), Some(&tree), Some(&theirs_tree)],
            &Labels {
                ours: "HEAD",
                theirs: name,
                base: &git.repo.abbreviate(base)?,
            },
            &Settings::default(),
            false,
        )?;
        let conflicted = merged
            .paths
            .values()
            .any(|r| matches!(r, Resolved::Conflict(_)));
        if !conflicted {
            if talk && !merged.trivial {
                println!("Simple merge did not work, trying automatic merge.");
                for path in &merged.line_merged {
                    println!("Auto-merging {}", crate::git_home::path_string(path));
                }
            }
            tree = result_tree(git, &merged)?;
            reached.push(*id);
        } else if i + 1 < f.kept.len() {
            eprintln!("Automated merge did not work.\nShould not be doing an octopus.");
            return fail_start(git, &ours, spec, kind, false);
        }
        last = Some(merged);
    }
    let Some(mut merged) = last else {
        bail!("an octopus merge reached no head to merge");
    };
    let moved = git
        .repo
        .diff_trees(Some(&ours_tree), Some(&tree), &Pathspec::default())?;
    for change in moved {
        merged
            .paths
            .entry(change.path)
            .or_insert(Resolved::Take(change.new));
    }
    let landing = Landing {
        theirs: f.label,
        orig_head: Some(ours),
        strict_index: true,
        quiet_when_clean: false,
        stages_at_origin: false,
    };
    let applied = match apply_merge_as(git, m.opened, Some(&ours_tree), &merged, &landing, run)? {
        Ok(applied) => applied,
        Err(refusal) => return refuse_apply(git, &ours, spec, kind, refusal),
    };
    Ok(Tried::Done(Box::new(Done {
        kind,
        how: Concluding::Strategy(kind),
        conflicted: applied.conflicted,
        unmerged: applied.unmerged,
        landed: Some((merged, applied.index)),
    })))
}

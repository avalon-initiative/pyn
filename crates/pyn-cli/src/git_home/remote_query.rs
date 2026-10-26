//! `pyn remote show`, `update`, `prune` and `set-head`: the subcommands of `pyn remote` that ask a remote what it has.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use pyn_git::{Ref, Refspec, Remote, branches_tracking, map_ref, remote_names, tracking};
use pyn_gitwire::RemoteRef;
use pyn_object::ObjectId;

use super::GitHome;
use super::fetch::{
    Options, default_update_set, fetch_many, fetch_single, group_members, query_refs,
    remotes_in_order,
};
use super::fetch_apply::{delete_ref, stale_refs};
use super::fetch_map::pretty;
use super::remote::fail;

const HEADS: &str = "refs/heads/";

/// A remote as the command line names it: a configured remote, or else the name taken as a URL.
struct Target {
    name: String,
    url: String,
    urls: Vec<String>,
    push_urls: Vec<String>,
    specs: Vec<Refspec>,
}

fn target(git: &GitHome, name: &str) -> Target {
    let remote = Remote::read(git.config(), name).unwrap_or_default();
    let or_name = |urls: &[String]| {
        if urls.is_empty() {
            vec![name.to_owned()]
        } else {
            urls.to_vec()
        }
    };
    Target {
        name: name.to_owned(),
        url: or_name(&remote.urls)[0].clone(),
        urls: or_name(&remote.urls),
        push_urls: or_name(remote.push_targets()),
        specs: remote.fetch_specs(),
    }
}

fn remote_ids(refs: &[RemoteRef]) -> BTreeMap<&str, ObjectId> {
    refs.iter()
        .filter_map(|r| r.id.map(|id| (r.name.as_str(), id)))
        .collect()
}

/// The branches the remote's `HEAD` may be: the one it points at, else every branch at the same commit.
fn head_branches(refs: &[RemoteRef]) -> Vec<String> {
    let Some((head, id)) = refs
        .iter()
        .find(|r| r.name == "HEAD")
        .and_then(|r| r.id.map(|id| (r, id)))
    else {
        return Vec::new();
    };
    let branch = |name: &str| name.strip_prefix(HEADS).map(str::to_owned);
    if let Some(target) = &head.symref_target {
        return refs
            .iter()
            .find(|r| r.name == *target && r.id.is_some())
            .and_then(|r| branch(&r.name))
            .into_iter()
            .collect();
    }
    let mut found: Vec<String> = refs
        .iter()
        .filter(|r| r.id == Some(id))
        .filter_map(|r| branch(&r.name))
        .collect();
    found.sort();
    found
}

enum State {
    Tracked,
    New,
    Stale,
}

/// Remote branches by display name: the ones a fetch refspec maps, and local remote-tracking refs the remote lost.
fn branch_states(git: &GitHome, t: &Target, refs: &[RemoteRef]) -> Result<BTreeMap<String, State>> {
    let local = git.repo.refs();
    let mut states = BTreeMap::new();
    for r in refs.iter().filter(|r| r.id.is_some()) {
        let Some(short) = r.name.strip_prefix(HEADS) else {
            continue;
        };
        if let Some(dst) = map_ref(&t.specs, &r.name) {
            let state = if local.resolve(&dst)?.is_some() {
                State::Tracked
            } else {
                State::New
            };
            states.insert(short.to_owned(), state);
        }
    }
    let names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
    for (name, _) in stale_refs(git, &t.specs, &names)? {
        states.insert(name, State::Stale);
    }
    Ok(states)
}

/// Remote branches without asking the remote: the sources of the remote-tracking refs the fetch refspecs map.
fn stored_branches(git: &GitHome, t: &Target) -> Result<BTreeSet<String>> {
    let refs = git.repo.refs();
    let mut found = BTreeSet::new();
    for (name, _) in refs.list("refs/")? {
        if matches!(refs.read(&name)?, Some(Ref::Symbolic(_))) {
            continue;
        }
        let source = t
            .specs
            .iter()
            .filter(|s| s.dst.as_deref().is_some_and(|d| !d.is_empty()))
            .find_map(|s| s.source_of(&name));
        if let Some(source) = source {
            found.insert(pretty(&source).to_owned());
        }
    }
    Ok(found)
}

/// The text for `branch.<b>.rebase`, or `None` when the branch merges.
fn rebase_verb(value: Option<&str>) -> Option<&'static str> {
    match value?.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some("rebases onto"),
        "interactive" | "i" => Some("rebases interactively onto"),
        "merges" | "m" => Some("rebases interactively (with merges) onto"),
        _ => None,
    }
}

/// The local branches set up to pull from the remote, in git's columns.
fn pull_lines(git: &GitHome, name: &str) -> Vec<String> {
    let config = git.config();
    let rows: Vec<(String, Vec<String>, Option<&'static str>)> = branches_tracking(config, name)
        .into_iter()
        .filter_map(|b| {
            let merge: Vec<String> = tracking(config, &b)
                .merge
                .iter()
                .map(|m| m.strip_prefix(HEADS).unwrap_or(m).to_owned())
                .collect();
            let verb = rebase_verb(config.get(&format!("branch.{b}.rebase")));
            (!merge.is_empty()).then_some((b, merge, verb))
        })
        .collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let width = rows.iter().map(|(b, ..)| b.len()).max().unwrap_or(0);
    let heading = if rows.len() == 1 {
        "  Local branch configured for 'git pull':"
    } else {
        "  Local branches configured for 'git pull':"
    };
    let pad = if rows.iter().any(|r| r.2.is_some()) {
        " "
    } else {
        ""
    };
    let mut lines = vec![heading.to_owned()];
    for (branch, merge, verb) in rows {
        match verb {
            Some(verb) => lines.push(format!("    {branch:<width$} {verb} remote {}", merge[0])),
            None => {
                lines.push(format!(
                    "    {branch:<width$} {pad}merges with remote {}",
                    merge[0]
                ));
                for more in &merge[1..] {
                    lines.push(format!("    {:<width$} {pad}   and with remote {more}", ""));
                }
            }
        }
    }
    lines
}

struct PushRow {
    local: String,
    force: bool,
    dest: String,
    status: &'static str,
}

fn push_specs(git: &GitHome, name: &str) -> Vec<String> {
    git.config()
        .get_all(&format!("remote.{name}.push"))
        .into_iter()
        .map(str::to_owned)
        .collect()
}

fn push_status(git: &GitHome, local: Option<ObjectId>, remote: Option<ObjectId>) -> &'static str {
    match (local, remote) {
        (None, _) => "delete",
        (Some(_), None) => "create",
        (Some(l), Some(r)) if l == r => "up to date",
        (Some(l), Some(r)) => {
            let known = git.repo.objects().contains(&r).unwrap_or(false);
            if known && git.is_ancestor(&r, &l).unwrap_or(false) {
                "fast-forwardable"
            } else {
                "local out of date"
            }
        }
    }
}

/// What each configured push refspec would do against the remote's refs; no refspec means every branch both sides have.
fn push_rows(git: &GitHome, name: &str, remote: &BTreeMap<&str, ObjectId>) -> Result<Vec<PushRow>> {
    let refs = git.repo.refs();
    let strip = |n: &str| n.strip_prefix(HEADS).unwrap_or(n).to_owned();
    let specs = push_specs(git, name);
    let mut rows = Vec::new();
    if specs.is_empty() {
        for (local, id) in refs.list(HEADS)? {
            if let Some(&theirs) = remote.get(local.as_str()) {
                let status = push_status(git, Some(id), Some(theirs));
                rows.push(PushRow {
                    local: strip(&local),
                    force: false,
                    dest: strip(&local),
                    status,
                });
            }
        }
    }
    for text in &specs {
        let Ok(spec) = Refspec::parse(text) else {
            continue;
        };
        let dest_of = |l: &str| -> Option<String> {
            match &spec.dst {
                Some(d) if !d.is_empty() => spec.map(l),
                _ => Some(l.to_owned()),
            }
        };
        if spec.src.is_empty() {
            let Some(dst) = spec.dst.as_deref().filter(|d| remote.contains_key(*d)) else {
                continue;
            };
            let status = push_status(git, None, None);
            rows.push(PushRow {
                local: "(none)".into(),
                force: spec.force,
                dest: strip(dst),
                status,
            });
            continue;
        }
        let candidates: Vec<(String, ObjectId)> = if spec.is_pattern() {
            refs.list("refs/")?
                .into_iter()
                .filter(|(n, _)| spec.matches_src(n))
                .collect()
        } else {
            [spec.src.clone(), format!("{HEADS}{}", spec.src)]
                .into_iter()
                .find_map(|n| refs.resolve(&n).ok().flatten().map(|id| (n, id)))
                .into_iter()
                .collect()
        };
        for (local, id) in candidates {
            let Some(dest) = dest_of(&local) else {
                continue;
            };
            let status = push_status(git, Some(id), remote.get(dest.as_str()).copied());
            rows.push(PushRow {
                local: strip(&local),
                force: spec.force,
                dest: strip(&dest),
                status,
            });
        }
    }
    rows.sort_by(|a, b| (&a.dest, &a.local).cmp(&(&b.dest, &b.local)));
    Ok(rows)
}

fn push_lines(rows: &[PushRow]) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    let heading = match rows.len() {
        1 => "  Local ref configured for 'git push':",
        _ => "  Local refs configured for 'git push':",
    };
    let shown = |r: &PushRow| {
        if r.status == "delete" {
            8
        } else {
            r.local.len()
        }
    };
    let lw = rows.iter().map(shown).max().unwrap_or(0);
    let dw = rows.iter().map(|r| r.dest.len()).max().unwrap_or(0);
    let mut lines = vec![heading.to_owned()];
    for r in rows {
        let verb = if r.force { "forces to" } else { "pushes to" };
        lines.push(format!(
            "    {:<lw$} {verb} {:<dw$} ({})",
            r.local, r.dest, r.status
        ));
    }
    lines
}

/// What `-n` can say about pushes: the configured refspecs as written.
fn stored_push_lines(git: &GitHome, name: &str) -> Vec<String> {
    let mut rows: Vec<(String, bool, String)> = push_specs(git, name)
        .iter()
        .map(|text| {
            let (force, body) = text
                .strip_prefix('+')
                .map_or((false, text.as_str()), |b| (true, b));
            let (src, dst) = body.split_once(':').unwrap_or((body, body));
            match (src, dst) {
                ("", "") => ("(matching)".to_owned(), force, "(matching)".to_owned()),
                ("", d) => ("(delete)".to_owned(), force, d.to_owned()),
                (s, d) => (s.to_owned(), force, d.to_owned()),
            }
        })
        .collect();
    if rows.is_empty() {
        rows.push(("(matching)".into(), false, "(matching)".into()));
    }
    rows.sort();
    let heading = match rows.len() {
        1 => "  Local ref configured for 'git push' (status not queried):",
        _ => "  Local refs configured for 'git push' (status not queried):",
    };
    let width = rows.iter().map(|(s, ..)| s.len()).max().unwrap_or(0);
    let mut lines = vec![heading.to_owned()];
    for (src, force, dst) in rows {
        let verb = if force { "forces to" } else { "pushes to" };
        lines.push(format!("    {src:<width$} {verb} {dst}"));
    }
    lines
}

fn branch_lines(
    name: &str,
    states: &BTreeMap<String, Option<State>>,
    queried: bool,
) -> Vec<String> {
    if states.is_empty() {
        return Vec::new();
    }
    let noun = if states.len() == 1 {
        "branch"
    } else {
        "branches"
    };
    let note = if queried { "" } else { " (status not queried)" };
    let mut lines = vec![format!("  Remote {noun}:{note}")];
    let width = states.keys().map(String::len).max().unwrap_or(0);
    for (branch, state) in states {
        lines.push(match state {
            None => format!("    {branch}"),
            Some(State::Tracked) => format!("    {branch:<width$} tracked"),
            Some(State::New) => {
                format!("    {branch:<width$} new (next fetch will store in remotes/{name})")
            }
            Some(State::Stale) => {
                format!("    {branch:<width$} stale (use 'git remote prune' to remove)")
            }
        });
    }
    lines
}

fn show_one(git: &GitHome, name: &str, no_query: bool) -> Result<()> {
    let t = target(git, name);
    let refs = if no_query {
        None
    } else {
        Some(query_refs(git, &t.url)?)
    };
    let mut lines = vec![
        format!("* remote {name}"),
        format!("  Fetch URL: {}", t.urls[0]),
    ];
    lines.extend(t.push_urls.iter().map(|u| format!("  Push  URL: {u}")));
    let states: BTreeMap<String, Option<State>> = match &refs {
        Some(refs) => {
            let heads = head_branches(refs);
            lines.push(match heads.as_slice() {
                [] => "  HEAD branch: (unknown)".to_owned(),
                [one] => format!("  HEAD branch: {one}"),
                many => {
                    let mut text =
                        "  HEAD branch (remote HEAD is ambiguous, may be one of the following):"
                            .to_owned();
                    for h in many {
                        text.push_str(&format!("\n    {h}"));
                    }
                    text
                }
            });
            branch_states(git, &t, refs)?
                .into_iter()
                .map(|(k, v)| (k, Some(v)))
                .collect()
        }
        None => {
            lines.push("  HEAD branch: (not queried)".into());
            stored_branches(git, &t)?
                .into_iter()
                .map(|k| (k, None))
                .collect()
        }
    };
    lines.extend(branch_lines(name, &states, refs.is_some()));
    lines.extend(pull_lines(git, name));
    let mirror = git
        .config()
        .get_bool(&format!("remote.{name}.mirror"))
        .ok()
        .flatten()
        == Some(true);
    if mirror {
        lines.push("  Local refs will be mirrored by 'git push'".into());
    } else if let Some(refs) = &refs {
        lines.extend(push_lines(&push_rows(git, name, &remote_ids(refs))?));
    } else {
        lines.extend(stored_push_lines(git, name));
    }
    println!("{}", lines.join("\n"));
    Ok(())
}

/// `remote show [-n] [<name>...]`; with no name, the remote names.
pub fn show(git: &GitHome, no_query: bool, names: &[String]) -> Result<()> {
    if names.is_empty() {
        remote_names(git.config())
            .iter()
            .for_each(|n| println!("{n}"));
        return Ok(());
    }
    names.iter().try_for_each(|n| show_one(git, n, no_query))
}

/// `remote prune [-n] <name>...`: deletes the remote-tracking refs whose branch the remote lost.
pub fn prune(git: &GitHome, dry_run: bool, names: &[String]) -> Result<()> {
    for name in names {
        let t = target(git, name);
        let refs = query_refs(git, &t.url)?;
        let remote_names: Vec<&str> = refs.iter().map(|r| r.name.as_str()).collect();
        let stale = stale_refs(git, &t.specs, &remote_names)?;
        if stale.is_empty() {
            continue;
        }
        println!("Pruning {}\nURL: {}", t.name, t.url);
        for (ref_name, id) in stale {
            if !dry_run {
                delete_ref(git, &ref_name, id)?;
            }
            let verb = if dry_run { "would prune" } else { "pruned" };
            println!(" * [{verb}] {}", pretty(&ref_name));
        }
    }
    Ok(())
}

/// The remotes `remote update` takes for one argument: a `remotes.<group>`, else a configured remote.
fn expand(git: &GitHome, arg: &str) -> Vec<String> {
    let members = group_members(git, arg);
    if !members.is_empty() {
        return members;
    }
    if remotes_in_order(git).iter().any(|n| n == arg) {
        return vec![arg.to_owned()];
    }
    fail(1, format!("fatal: no such remote or remote group: {arg}"))
}

/// `remote update [-p] [<group>|<remote>...]`: fetches them as `fetch --multiple` does; a lone default remote gets no heading.
pub fn update(git: &GitHome, prune: Option<bool>, verbose: bool, args: &[String]) -> Result<()> {
    let default = group_members(git, "default");
    let (names, headed) = match (args.is_empty(), default.is_empty()) {
        (true, true) => (default_update_set(git), false),
        (true, false) => (default, true),
        _ => (args.iter().flat_map(|a| expand(git, a)).collect(), true),
    };
    let opts = Options::for_update(prune, verbose);
    let failed = match names.as_slice() {
        [one] if !headed => fetch_single(git, one, &opts),
        _ => fetch_many(git, &names, &opts, None)?,
    };
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

/// What `remote set-head` was asked to do.
#[derive(Clone, Copy)]
pub enum HeadAction<'a> {
    Auto,
    Delete,
    Set(&'a str),
}

pub fn set_head(git: &GitHome, name: &str, action: HeadAction) -> Result<()> {
    let refs = git.repo.refs();
    let head = format!("refs/remotes/{name}/HEAD");
    let branch = match action {
        HeadAction::Delete => {
            if refs.read(&head)?.is_some() {
                refs.delete(&head, pyn_git::Expect::Any)?;
                refs.write_reflog(&head, &[])?;
            }
            return Ok(());
        }
        HeadAction::Set(b) => b.to_owned(),
        HeadAction::Auto => {
            let t = target(git, name);
            let heads = head_branches(&query_refs(git, &t.url)?);
            match heads.as_slice() {
                [one] => one.clone(),
                [] => fail(1, "error: Cannot determine remote HEAD".into()),
                many => {
                    let choices: String = many
                        .iter()
                        .map(|h| format!("\n  pyn remote set-head {name} {h}"))
                        .collect();
                    fail(
                        1,
                        format!(
                            "error: Multiple remote HEAD branches. Please choose one explicitly with:{choices}"
                        ),
                    )
                }
            }
        }
    };
    let tracking = format!("refs/remotes/{name}/{branch}");
    if refs.resolve(&tracking)?.is_none() {
        fail(1, format!("error: Not a valid ref: {tracking}"));
    }
    refs.set_symbolic(&head, &tracking)?;
    if matches!(action, HeadAction::Auto) {
        println!("{name}/HEAD set to {branch}");
    }
    Ok(())
}

//! `pyn merge` on a pyn server (docs/concepts/merging.md): the merge in progress, its conflicts and its completion.
//!
//! The server is stateless between a preview and the write, so a merge in progress is only this workspace's record,
//! `.pyn/local_only/state/merge.toml`. Nothing is sent until `pyn commit` completes the merge with the resolutions.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::transport::Method;
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::Args;
use pyn_merge::{Outcome, merge_text, render_markers};
use pyn_proto as api;
use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::branch;
use crate::client::Api;
use crate::localfile;
use crate::snapshot::short;
use crate::staging::{self, Index};
use crate::stranded;
use crate::sync::{self, mode_name};
use crate::time;
use crate::workspace::{FileState, Workspace, hash_of, set_read_only};

pub(crate) const STATE_VERSION: u32 = 1;

/// Merge a branch (or commit) into the workspace's branch.
#[derive(Debug, Args)]
pub struct MergeArgs {
    /// The branch, or commit, to merge into the current branch.
    pub(crate) source: Option<String>,
    /// The merge commit's message; repeat for more paragraphs.
    #[arg(short = 'm', long = "message")]
    pub(crate) message: Vec<String>,
    /// Make a merge commit even when the branch could fast-forward.
    #[arg(long)]
    pub(crate) no_ff: bool,
    /// Refuse to merge unless the branch can fast-forward.
    #[arg(long, conflicts_with = "no_ff")]
    pub(crate) ff_only: bool,
    /// Perform the merge but stop before committing it.
    #[arg(long)]
    pub(crate) no_commit: bool,
    /// Give up a merge in progress and put the files back.
    #[arg(long, conflicts_with_all = ["source", "no_ff", "ff_only", "no_commit", "message", "preview", "continue_merge"])]
    pub(crate) abort: bool,
    /// Conclude a merge in progress, as `pyn commit` does.
    #[arg(long = "continue", conflicts_with_all = ["source", "no_ff", "ff_only", "no_commit", "preview"])]
    pub(crate) continue_merge: bool,
    /// Show what would merge and what would conflict, and change nothing.
    #[arg(long)]
    pub(crate) preview: bool,
    /// Print the diffstat after the merge (git's default; `merge.stat` turns it off).
    #[arg(long, alias = "summary", overrides_with = "no_stat")]
    pub(crate) stat: bool,
    /// Print no diffstat after the merge.
    #[arg(short = 'n', long, alias = "no-summary", overrides_with = "stat")]
    pub(crate) no_stat: bool,
}

/// One side of a conflicted path, as the workspace needs it to take that side later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SideNote {
    revision: u64,
    file_mode: api::FileMode,
}

/// A path the merge touches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Touched {
    pub(crate) mode: String,
    /// Why the path is unmerged until it is staged; `None` for a path that merged cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) conflict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ours: Option<SideNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) theirs: Option<SideNote>,
    /// The branch's revision of an exclusive path, which a lock must be based on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) line_head: Option<u64>,
}

/// The merge in progress.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct MergeState {
    pub(crate) version: u32,
    /// The branch being merged into: the workspace's.
    pub(crate) branch: String,
    /// What the person named, for messages.
    pub(crate) source: String,
    pub(crate) source_commit: String,
    pub(crate) target_commit: String,
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) no_ff: bool,
    pub(crate) paths: BTreeMap<String, Touched>,
    /// A pull that stopped on conflicts in a workspace with local commits, which is concluded without the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) local: Option<crate::local_merge::LocalOp>,
}

fn state_path(ws: &Workspace) -> PathBuf {
    ws.dir.join("local_only").join("state").join("merge.toml")
}

pub(crate) fn load(ws: &Workspace) -> Result<Option<MergeState>> {
    match std::fs::read_to_string(state_path(ws)) {
        Ok(text) => {
            let state: MergeState = toml::from_str(&text).context("reading the merge state")?;
            if state.version > STATE_VERSION {
                bail!("the merge state is newer than this pyn understands");
            }
            Ok(Some(state))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn save(ws: &Workspace, state: &MergeState) -> Result<()> {
    let path = state_path(ws);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, toml::to_string_pretty(state)?)
        .with_context(|| format!("writing {}", path.display()))
}

pub(crate) fn clear(ws: &Workspace) -> Result<()> {
    match std::fs::remove_file(state_path(ws)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Whether the workspace is in the middle of a merge.
pub fn in_progress(ws: &Workspace) -> bool {
    state_path(ws).exists()
}

/// Refuses a command that cannot run while a merge is unfinished.
pub fn refuse_during_merge(ws: &Workspace, what: &str) -> Result<()> {
    if in_progress(ws) {
        if let Some(op) = load(ws)?.and_then(|s| s.local) {
            bail!(
                "you have not concluded your {}, so `pyn {what}` cannot run; {}",
                op.name(),
                op.how_to_end()
            );
        }
        bail!(
            "you have not concluded your merge, so `pyn {what}` cannot run; \
             conclude it with `pyn commit` or give it up with `pyn merge --abort`"
        );
    }
    Ok(())
}

/// The paths the merge in progress still has unmerged: conflicted and not staged since.
pub fn unmerged(ws: &Workspace) -> Result<BTreeMap<String, String>> {
    let Some(state) = load(ws)? else {
        return Ok(BTreeMap::new());
    };
    let index = Index::load(ws)?;
    Ok(state
        .paths
        .into_iter()
        .filter(|(p, t)| t.conflict.is_some() && !index.entries.contains_key(p))
        .filter_map(|(p, t)| t.conflict.map(|c| (p, c)))
        .collect())
}

/// The conflicted paths of the merge in progress, staged or not; `add` stages them even when they look unchanged.
pub fn conflicted(ws: &Workspace) -> BTreeSet<String> {
    load(ws)
        .ok()
        .flatten()
        .map(|s| {
            s.paths
                .into_iter()
                .filter(|(_, t)| t.conflict.is_some())
                .map(|(p, _)| p)
                .collect()
        })
        .unwrap_or_default()
}

/// The paths a merge in progress touched cleanly, which `status` marks as merged.
pub fn merged_cleanly(ws: &Workspace) -> BTreeSet<String> {
    load(ws)
        .ok()
        .flatten()
        .map(|s| {
            s.paths
                .into_iter()
                .filter(|(_, t)| t.conflict.is_none())
                .map(|(p, _)| p)
                .collect()
        })
        .unwrap_or_default()
}

/// The text `status` prints above its table while a merge is in progress.
pub fn status_header(ws: &Workspace) -> Result<Option<String>> {
    let Some(state) = load(ws)? else {
        return Ok(None);
    };
    let pending = unmerged(ws)?.len();
    let mut text = if pending > 0 {
        format!(
            "You have unmerged paths ({} of {}).\n  (fix conflicts, stage each with \"pyn add <path>\", then run \"pyn commit\")\n",
            pending,
            state
                .paths
                .values()
                .filter(|t| t.conflict.is_some())
                .count()
        )
    } else {
        "All conflicts are fixed but you are still merging.\n  (use \"pyn commit\" to conclude the merge)\n".to_string()
    };
    if let Some(op) = &state.local {
        text.push_str(&op.header());
    } else {
        text.push_str("  (use \"pyn merge --abort\" to abort the merge)\n");
    }
    Ok(Some(text))
}

fn request(source: &str, target: &str, fast_forward: api::MergeFastForward) -> api::MergeRequest {
    api::MergeRequest {
        source: source.to_string(),
        target: Some(target.to_string()),
        fast_forward: Some(fast_forward),
        message: None,
        expect_target: None,
        resolutions: Vec::new(),
    }
}

fn post(api: &Api, tail: &str, body: &api::MergeRequest) -> Result<api::MergeReport> {
    api.send(api.request(Method::POST, &api.repo_route(tail)?).json(body))?
        .json()
}

fn conflict_label(kind: api::MergeConflictKind) -> &'static str {
    match kind {
        api::MergeConflictKind::Content | api::MergeConflictKind::Binary => "content",
        api::MergeConflictKind::BothAdded => "add/add",
        api::MergeConflictKind::DeleteModify => "modify/delete",
        api::MergeConflictKind::FileMode => "file mode",
        api::MergeConflictKind::ExclusiveConversion => "exclusive conversion",
    }
}

fn unmerged_label(kind: api::MergeConflictKind) -> &'static str {
    match kind {
        api::MergeConflictKind::BothAdded => "both added",
        api::MergeConflictKind::DeleteModify => "deleted on one side",
        api::MergeConflictKind::ExclusiveConversion => "converted to exclusive",
        _ => "both modified",
    }
}

fn print_preview(report: &api::MergeReport, source: &str) {
    println!("Merging {source} into {}", report.target);
    match report.outcome {
        api::MergeOutcome::UpToDate => println!("Already up to date."),
        api::MergeOutcome::FastForward => println!("Fast-forward"),
        api::MergeOutcome::Merge => println!("Merge commit"),
        api::MergeOutcome::Conflicted => println!("Conflicts"),
    }
    if report.virtual_base {
        println!("The histories cross-merged; the merge is made against a virtual base.");
    }
    let mut rows: Vec<Vec<String>> = report
        .changes
        .iter()
        .map(|c| {
            let how = if c.revision.is_some() {
                "changed"
            } else {
                "merged"
            };
            vec![how.to_string(), c.path.clone()]
        })
        .collect();
    rows.extend(report.conflicts.iter().map(|c| {
        vec![
            format!("conflict ({})", conflict_label(c.kind)),
            c.path.clone(),
        ]
    }));
    if !rows.is_empty() {
        println!("{}", crate::table::render(&["RESULT", "PATH"], &rows));
    }
    for story in report.conflicts.iter().filter_map(conversion_story) {
        println!("{story}");
    }
}

fn clean_or_bail(ws: &Workspace, report: &api::MergeReport) -> Result<()> {
    let state = ws.load_state()?;
    let mut blocked = Vec::new();
    let touched = report
        .changes
        .iter()
        .map(|c| &c.path)
        .chain(report.conflicts.iter().map(|c| &c.path));
    for path in touched.clone() {
        ws.dest(path)?;
    }
    for path in touched {
        let abs = ws.abs(path);
        match state.get(path) {
            Some(known) if localfile::exists(&abs) => {
                let (file_mode, bytes) = localfile::read(&abs)?;
                if known.changed(file_mode, &bytes) {
                    blocked.push(format!("\t{path}"));
                }
            }
            Some(_) => blocked.push(format!("\t{path}")),
            None if localfile::exists(&abs) => blocked.push(format!("\t{path}")),
            None => {}
        }
    }
    if blocked.is_empty() {
        return Ok(());
    }
    bail!(
        "your local changes to the following files would be overwritten by merge:\n{}\n\
         commit them, or put them back with `pyn checkout -- <path>`, before you merge; nothing was changed",
        blocked.join("\n")
    );
}

pub fn merge_command(backend: &dyn Backend, ws: Option<&Workspace>, args: MergeArgs) -> Result<()> {
    let api = backend.pyn_server("merge")?;
    let ws = ws.context("`pyn merge` works inside a workspace; run `pyn clone` first")?;
    if args.stat || args.no_stat {
        bail!("`pyn merge --stat` and `-n` need a git repository; a pyn server prints no diffstat");
    }
    if args.abort {
        return abort(backend, api, ws);
    }
    if args.continue_merge {
        return finish(backend, api, ws, None);
    }
    if in_progress(ws) {
        bail!(
            "you have not concluded your merge; conclude it with `pyn commit`, \
             or give it up with `pyn merge --abort`"
        );
    }
    let Some(spec) = args.source.clone() else {
        bail!("specify a branch to merge; see `pyn branch`");
    };
    let here = branch::name(Some(ws));
    let index = Index::load(ws)?;
    if !index.entries.is_empty() {
        bail!(
            "you have staged changes; commit them or take them out with `pyn restore --staged` before you merge"
        );
    }
    let source = branch::start_point(backend, api, &spec)?;
    let fast_forward = if args.no_ff {
        api::MergeFastForward::Never
    } else if args.ff_only {
        api::MergeFastForward::Only
    } else {
        api::MergeFastForward::Allow
    };
    let report = post(
        api,
        "/merge/preview",
        &request(&source, &here, fast_forward),
    )?;
    if args.preview {
        print_preview(&report, &spec);
        return Ok(());
    }
    if report.outcome == api::MergeOutcome::UpToDate {
        println!("Already up to date.");
        return Ok(());
    }
    clean_or_bail(ws, &report)?;
    let message = if args.message.is_empty() {
        format!("Merge branch '{spec}' into {here}")
    } else {
        args.message.join("\n\n")
    };
    let pinned = |message: Option<String>| api::MergeRequest {
        source: report.source.clone(),
        message,
        expect_target: report.target_commit.clone(),
        ..request(&report.source, &here, fast_forward)
    };
    match report.outcome {
        api::MergeOutcome::UpToDate => Ok(()),
        api::MergeOutcome::FastForward => {
            let done = post(api, "/merge", &pinned(None))?;
            let commit = done.commit.clone().unwrap_or_default();
            land(backend, api, ws, &commit, &report)?;
            println!(
                "Updating {}..{}\nFast-forward",
                report.target_commit.as_deref().map_or("", short),
                short(&commit)
            );
            for c in &report.changes {
                println!(" {}", c.path);
            }
            stranded::print(
                "The merged policy left shared versions on other branches that can no longer merge:",
                &done.stranded,
            );
            Ok(())
        }
        api::MergeOutcome::Merge if !args.no_commit => {
            let done = post(api, "/merge", &pinned(Some(message)))?;
            let commit = done.commit.clone().unwrap_or_default();
            for c in report.changes.iter().filter(|c| c.revision.is_none()) {
                println!("Auto-merging {}", c.path);
            }
            land(backend, api, ws, &commit, &report)?;
            println!("Merge made; {} is now at {}.", here, short(&commit));
            stranded::print(
                "The merged policy left shared versions on other branches that can no longer merge:",
                &done.stranded,
            );
            Ok(())
        }
        api::MergeOutcome::Merge | api::MergeOutcome::Conflicted => {
            begin(backend, api, ws, &report, &spec, &here, message, args.no_ff)
        }
    }
}

/// Brings the workspace to the commit a merge or fast-forward just made, for the paths it changed.
fn land(
    backend: &dyn Backend,
    api: &Api,
    ws: &Workspace,
    commit: &str,
    report: &api::MergeReport,
) -> Result<()> {
    let (_, snap) = backend.snapshot(Some(commit))?;
    let touched: BTreeSet<&str> = report
        .changes
        .iter()
        .map(|c| c.path.as_str())
        .chain(report.conflicts.iter().map(|c| c.path.as_str()))
        .collect();
    adopt(backend, api, ws, &snap, &touched)
}

/// Sets the workspace's record of each touched path to the revision the commit gives it, fetching the content unless the
/// file already holds it. A path whose content is unchanged but whose revision moved is not in the report, so it is
/// adopted too.
fn adopt(
    backend: &dyn Backend,
    api: &Api,
    ws: &Workspace,
    snap: &[api::SnapshotEntry],
    touched: &BTreeSet<&str>,
) -> Result<()> {
    let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
    let me = backend.whoami()?;
    let mut state = ws.load_state()?;
    let moved = |e: &api::SnapshotEntry| {
        state
            .get(&e.path)
            .is_some_and(|k| k.revision != e.revision && k.hash == e.content)
    };
    let adopted: Vec<&api::SnapshotEntry> = snap
        .iter()
        .filter(|e| touched.contains(e.path.as_str()) || moved(e))
        .collect();
    for entry in adopted {
        let abs = ws.abs(&entry.path);
        let holds = localfile::exists(&abs) && {
            let (file_mode, bytes) = localfile::read(&abs)?;
            hash_of(&bytes) == entry.content && !localfile::mode_differs(entry.file_mode, file_mode)
        };
        let file = api::FileEntry {
            path: entry.path.clone(),
            mode: entry.mode,
            revision: Some(entry.revision),
            file_mode: entry.file_mode,
            lock: locks.iter().find(|l| l.path == entry.path).cloned(),
        };
        let known = if holds {
            set_read_only(
                &abs,
                file.mode == api::Mode::Exclusive
                    && file.lock.as_ref().is_none_or(|l| l.owner != me),
            )?;
            FileState {
                revision: entry.revision,
                hash: entry.content.clone(),
                legacy: false,
                mode: mode_name(entry.mode).to_string(),
                file_mode: entry.file_mode,
            }
        } else {
            sync::fetch(backend, ws, &file, entry.revision, &me)?
        };
        state.insert(entry.path.clone(), known);
    }
    ws.save_state(&state)
}

/// Writes the merge into the working tree and records it, for a merge that has conflicts or that stops before committing.
#[allow(clippy::too_many_arguments)]
fn begin(
    backend: &dyn Backend,
    api: &Api,
    ws: &Workspace,
    report: &api::MergeReport,
    spec: &str,
    here: &str,
    message: String,
    no_ff: bool,
) -> Result<()> {
    let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
    let me = backend.whoami()?;
    let mut state = ws.load_state()?;
    let mut paths = BTreeMap::new();
    for c in &report.changes {
        if c.inputs.is_some() {
            println!("Auto-merging {}", c.path);
        }
        write_change(backend, ws, c, &locks, &me, &mut state)?;
        paths.insert(
            c.path.clone(),
            Touched {
                mode: mode_name(c.mode).to_string(),
                conflict: None,
                ours: None,
                theirs: None,
                line_head: None,
            },
        );
    }
    for c in &report.conflicts {
        write_conflict(backend, ws, c, spec, &locks, &me, &mut state)?;
        let side = |s: &Option<api::MergeSide>| {
            s.as_ref().and_then(|s| {
                s.revision.map(|revision| SideNote {
                    revision,
                    file_mode: s.file_mode,
                })
            })
        };
        let line_head = c.inputs.ours.as_ref().and_then(|s| s.revision);
        paths.insert(
            c.path.clone(),
            Touched {
                mode: mode_name(c.mode).to_string(),
                conflict: Some(unmerged_label(c.kind).to_string()),
                ours: side(&c.inputs.ours),
                theirs: side(&c.inputs.theirs),
                line_head: (c.mode == api::Mode::Exclusive)
                    .then_some(line_head)
                    .flatten(),
            },
        );
    }
    ws.save_state(&state)?;
    save(
        ws,
        &MergeState {
            version: STATE_VERSION,
            branch: here.to_string(),
            source: spec.to_string(),
            source_commit: report.source.clone(),
            target_commit: report.target_commit.clone().unwrap_or_default(),
            message,
            no_ff,
            paths,
            local: None,
        },
    )?;
    if report.conflicts.is_empty() {
        println!(
            "Automatic merge went well; stopped before committing as requested.\n\
             Conclude it with `pyn commit`, or give it up with `pyn merge --abort`."
        );
        return Ok(());
    }
    for c in &report.conflicts {
        if c.kind == api::MergeConflictKind::Binary {
            println!(
                "warning: cannot merge binary files: {} (HEAD vs. {spec})",
                c.path
            );
        }
        println!(
            "CONFLICT ({}): Merge conflict in {}",
            conflict_label(c.kind),
            c.path
        );
        if c.kind == api::MergeConflictKind::ExclusiveConversion {
            println!("{}", conversion_hint(c));
        }
        if c.kind == api::MergeConflictKind::Binary && c.mode == api::Mode::Exclusive {
            println!("{}", divergence_hint(c));
        }
    }
    println!(
        "Automatic merge failed; fix conflicts, stage each with `pyn add <path>`, then run `pyn commit`.\n\
         To keep the version of a file on one side, use `pyn checkout --ours|--theirs -- <path>`."
    );
    std::process::exit(1);
}

fn conversion_story(c: &api::MergeConflict) -> Option<String> {
    let n = c.conversion.as_ref()?;
    let policy = n
        .policy_revision
        .map_or(String::new(), |p| format!(" by policy r{p}"));
    Some(format!(
        "  {} became exclusive at r{}{policy}, made by {} on {}; the shared version r{} by {} on {} was stranded.",
        c.path,
        n.revision,
        n.by,
        time::local(n.at),
        n.diverged_revision,
        n.diverged_by,
        time::local(n.diverged_at),
    ))
}

fn divergence_hint(c: &api::MergeConflict) -> String {
    let base = c
        .inputs
        .ours
        .as_ref()
        .and_then(|s| s.revision)
        .map_or(String::new(), |h| format!(" --base {h}"));
    format!(
        "  {} is exclusive and each branch has its own revision of it; choose one, take the lock (`pyn lock{base} {}`) and commit.",
        c.path, c.path
    )
}

fn conversion_hint(c: &api::MergeConflict) -> String {
    let head = c
        .inputs
        .ours
        .as_ref()
        .and_then(|s| s.revision)
        .map_or(String::new(), |h| format!(" --base {h}"));
    let story = conversion_story(c).map_or(String::new(), |s| format!("{s}\n"));
    format!(
        "{story}  {} is exclusive now and was changed on a branch that kept it shared; choose its next revision. \
         To keep a version that is not the newest, take the lock first: `pyn lock{head} {}`.",
        c.path, c.path
    )
}

fn side_bytes(backend: &dyn Backend, path: &str, side: &Option<api::MergeSide>) -> Result<Vec<u8>> {
    match side {
        Some(s) => match s.revision {
            Some(r) => backend.content(path, r, None),
            None => bail!("{path} has a version no revision holds"),
        },
        None => Ok(Vec::new()),
    }
}

fn base_bytes(backend: &dyn Backend, path: &str, inputs: &api::MergeInputs) -> Result<Vec<u8>> {
    match (&inputs.base_inline, &inputs.base) {
        (Some(inline), _) => STANDARD
            .decode(inline)
            .context("the server sent a base that is not base64"),
        (None, base) => side_bytes(backend, path, base),
    }
}

/// Writes `bytes` as the file at `path` with `file_mode`.
fn put_file(ws: &Workspace, path: &str, bytes: &[u8], file_mode: api::FileMode) -> Result<()> {
    let abs = ws.write_file(path, bytes)?;
    localfile::apply(&abs, path, file_mode)
}

/// A cleanly merged path in the working tree: the revision that holds the result, or the text merged here.
fn write_change(
    backend: &dyn Backend,
    ws: &Workspace,
    change: &api::MergeChange,
    locks: &[api::Lock],
    me: &str,
    state: &mut BTreeMap<String, FileState>,
) -> Result<()> {
    let lock = locks.iter().find(|l| l.path == change.path).cloned();
    if let Some(revision) = change.revision {
        let entry = api::FileEntry {
            path: change.path.clone(),
            mode: change.mode,
            revision: Some(revision),
            file_mode: change.file_mode,
            lock,
        };
        let fetched = sync::fetch(backend, ws, &entry, revision, me)?;
        state.entry(change.path.clone()).or_insert(fetched);
        return Ok(());
    }
    let inputs = change
        .inputs
        .as_ref()
        .with_context(|| format!("the server gave no way to merge {}", change.path))?;
    let base = base_bytes(backend, &change.path, inputs)?;
    let ours = side_bytes(backend, &change.path, &inputs.ours)?;
    let theirs = side_bytes(backend, &change.path, &inputs.theirs)?;
    let Outcome::Clean(merged) = merge_text(&base, &ours, &theirs) else {
        bail!("{} merged on the server but not here", change.path);
    };
    if hash_of(&merged) != change.content {
        bail!(
            "merging {} here gave different content than the server's; update pyn and try again",
            change.path
        );
    }
    put_file(ws, &change.path, &merged, change.file_mode)?;
    set_read_only(
        &ws.dest(&change.path)?,
        change.mode == api::Mode::Exclusive && lock.is_none_or(|l| l.owner != me),
    )
}

/// A conflicted path in the working tree: conflict markers for text, otherwise our version.
fn write_conflict(
    backend: &dyn Backend,
    ws: &Workspace,
    conflict: &api::MergeConflict,
    spec: &str,
    locks: &[api::Lock],
    me: &str,
    state: &mut BTreeMap<String, FileState>,
) -> Result<()> {
    let path = &conflict.path;
    let inputs = &conflict.inputs;
    let Some(ours) = &inputs.ours else {
        return Ok(());
    };
    let ours_bytes = side_bytes(backend, path, &inputs.ours)?;
    let mut written = ours_bytes.clone();
    if matches!(
        conflict.kind,
        api::MergeConflictKind::Content | api::MergeConflictKind::BothAdded
    ) {
        let base = base_bytes(backend, path, inputs)?;
        let theirs = side_bytes(backend, path, &inputs.theirs)?;
        if let Outcome::Conflicted(chunks) = merge_text(&base, &ours_bytes, &theirs) {
            let base_label = if inputs.base_inline.is_some() {
                "merged common ancestors"
            } else {
                "base"
            };
            written = render_markers(&chunks, "HEAD", base_label, spec);
        }
    }
    put_file(ws, path, &written, ours.file_mode)?;
    if let Some(revision) = ours.revision {
        let lock = locks.iter().find(|l| &l.path == path);
        set_read_only(
            &ws.dest(path)?,
            conflict.mode == api::Mode::Exclusive && lock.is_none_or(|l| l.owner != me),
        )?;
        state.insert(
            path.clone(),
            FileState {
                revision,
                hash: ours.content.clone(),
                legacy: false,
                mode: mode_name(conflict.mode).to_string(),
                file_mode: ours.file_mode,
            },
        );
    }
    Ok(())
}

/// Takes one side of a conflicted path into the working tree, as `pyn checkout --ours|--theirs -- <path>`.
pub fn take_side(
    backend: &dyn Backend,
    api: &Api,
    ws: &Workspace,
    theirs: bool,
    paths: &[String],
) -> Result<()> {
    let Some(state) = load(ws)? else {
        bail!("`--ours` and `--theirs` need a merge in progress");
    };
    if state.local.is_some() {
        return crate::local_merge::take_side(backend, ws, theirs, paths);
    }
    let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
    let me = backend.whoami()?;
    for path in paths {
        let Some(touched) = state.paths.get(path).filter(|t| t.conflict.is_some()) else {
            bail!("{path} is not unmerged");
        };
        let side = if theirs {
            &touched.theirs
        } else {
            &touched.ours
        };
        let Some(side) = side else {
            bail!("{path} does not exist on that side of the merge");
        };
        let mode = if touched.mode == "exclusive" {
            api::Mode::Exclusive
        } else {
            api::Mode::Shared
        };
        let entry = api::FileEntry {
            path: path.clone(),
            mode,
            revision: Some(side.revision),
            file_mode: side.file_mode,
            lock: locks.iter().find(|l| &l.path == path).cloned(),
        };
        sync::fetch(backend, ws, &entry, side.revision, &me)?;
    }
    Ok(())
}

fn abort(backend: &dyn Backend, api: &Api, ws: &Workspace) -> Result<()> {
    let Some(state) = load(ws)? else {
        bail!("there is no merge to abort");
    };
    if state.local.is_some() {
        return crate::local_merge::abort(backend, ws);
    }
    let (_, snap) = backend.snapshot(Some(&state.target_commit))?;
    let wanted: BTreeMap<&str, &api::SnapshotEntry> =
        snap.iter().map(|e| (e.path.as_str(), e)).collect();
    let locks: Vec<api::Lock> = api.send(api.get(&api.repo_route("/locks")?))?.json()?;
    let me = backend.whoami()?;
    let mut files = ws.load_state()?;
    ws.check_paths(state.paths.keys())?;
    for path in state.paths.keys() {
        match wanted.get(path.as_str()) {
            Some(entry) => {
                let file = api::FileEntry {
                    path: path.clone(),
                    mode: entry.mode,
                    revision: Some(entry.revision),
                    file_mode: entry.file_mode,
                    lock: locks.iter().find(|l| &l.path == path).cloned(),
                };
                files.insert(
                    path.clone(),
                    sync::fetch(backend, ws, &file, entry.revision, &me)?,
                );
            }
            None => {
                let abs = ws.dest(path)?;
                set_read_only(&abs, false)?;
                let _ = std::fs::remove_file(&abs);
                files.remove(path);
            }
        }
    }
    ws.save_state(&files)?;
    let mut index = Index::load(ws)?;
    index.entries.retain(|p, _| !state.paths.contains_key(p));
    index.save(ws)?;
    clear(ws)
}

/// Concludes the merge in progress with the staged resolutions, as `pyn commit` does; `message` overrides the recorded one.
pub fn finish(
    backend: &dyn Backend,
    api: &Api,
    ws: &Workspace,
    message: Option<String>,
) -> Result<()> {
    let Some(state) = load(ws)? else {
        bail!("there is no merge in progress");
    };
    if state.local.is_some() {
        return crate::local_merge::finish(backend, ws, message);
    }
    let index = Index::load(ws)?;
    check_resolved(&state, &index)?;
    let mut resolutions = Vec::new();
    for (path, staged) in &index.entries {
        if staged.deleted {
            bail!("{path} is staged as deleted; a merge cannot be concluded by deleting a file");
        }
        let file = staging::content_path(ws, &staged.hash);
        let sent = if staged.file_mode == api::FileMode::Symlink {
            backend.upload_bytes(std::fs::read(&file)?)?
        } else {
            backend.upload(&file)?
        };
        if sent != staged.hash {
            bail!(
                "the server stored {path} as {sent}, not the staged {}",
                staged.hash
            );
        }
        resolutions.push(api::MergeResolution {
            path: path.clone(),
            content: sent,
            file_mode: Some(staged.file_mode),
        });
    }
    let body = api::MergeRequest {
        source: state.source_commit.clone(),
        target: Some(state.branch.clone()),
        fast_forward: Some(api::MergeFastForward::Never),
        message: Some(message.unwrap_or_else(|| state.message.clone())),
        expect_target: Some(state.target_commit.clone()),
        resolutions,
    };
    let done = post(api, "/merge", &body).map_err(|e| lock_hint(e, &state))?;
    let commit = done.commit.clone().context("the server wrote no commit")?;
    let (_, snap) = backend.snapshot(Some(&commit))?;
    let touched: BTreeSet<&str> = state.paths.keys().map(String::as_str).collect();
    adopt(backend, api, ws, &snap, &touched)?;
    Index::default().save(ws)?;
    clear(ws)?;
    println!("Merge made; {} is now at {}.", state.branch, short(&commit));
    stranded::print(
        "The merged policy left shared versions on other branches that can no longer merge:",
        &done.stranded,
    );
    Ok(())
}

/// Refuses to conclude while a conflicted path is unstaged or a staged path is not part of the merge.
pub(crate) fn check_resolved(state: &MergeState, index: &Index) -> Result<()> {
    let unresolved: Vec<&String> = state
        .paths
        .iter()
        .filter(|(p, t)| t.conflict.is_some() && !index.entries.contains_key(*p))
        .map(|(p, _)| p)
        .collect();
    if !unresolved.is_empty() {
        let list: Vec<String> = unresolved.iter().map(|p| format!("\t{p}")).collect();
        bail!(
            "committing is not possible because you have unmerged files:\n{}\n\
             fix them, stage them with `pyn add <path>`, then commit",
            list.join("\n")
        );
    }
    if let Some(stray) = index.entries.keys().find(|p| !state.paths.contains_key(*p)) {
        bail!(
            "{stray} is staged but is not part of the merge; take it out with `pyn restore --staged {stray}` \
             and commit it separately"
        );
    }
    Ok(())
}

/// Whether the merge in progress is a pull of a workspace with local commits.
pub fn is_local(ws: &Workspace) -> bool {
    load(ws).ok().flatten().is_some_and(|s| s.local.is_some())
}

/// Explains what to do when the server wants the lock of a path resolved to a different version than the newest.
fn lock_hint(err: anyhow::Error, state: &MergeState) -> anyhow::Error {
    let Some(e) = err.downcast_ref::<crate::client::ApiError>() else {
        return err;
    };
    match e.code.as_str() {
        "lock_required" | "lock_held" | "stale_base" => {
            let hints: Vec<String> = state
                .paths
                .iter()
                .filter(|(_, t)| t.conflict.is_some() && t.mode == "exclusive")
                .map(|(p, t)| {
                    let base = t
                        .line_head
                        .map_or(String::new(), |h| format!(" --base {h}"));
                    format!("\t`pyn lock{base} {p}`")
                })
                .collect();
            anyhow::anyhow!(
                "{err}\nA resolution that is not the newest version of an exclusive path needs its lock; take it with:\n{}\nthen commit again",
                hints.join("\n")
            )
        }
        "merge_stale" => anyhow::anyhow!(
            "{err}\nthe branch moved since the merge started; give it up with `pyn merge --abort` and merge again"
        ),
        _ => err,
    }
}

/// Whether the file at `path` still has the conflict markers a merge wrote.
pub fn has_markers(path: &Path) -> bool {
    std::fs::read(path).is_ok_and(|b| {
        let text = String::from_utf8_lossy(&b);
        text.lines().any(|l| l.starts_with("<<<<<<< "))
            && text.lines().any(|l| l.starts_with(">>>>>>> "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_recognised_only_when_both_ends_are_there() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a");
        std::fs::write(&f, "x\n<<<<<<< HEAD\na\n=======\nb\n>>>>>>> topic\n").unwrap();
        assert!(has_markers(&f));
        std::fs::write(&f, "<<<<<<< HEAD only\n").unwrap();
        assert!(!has_markers(&f));
    }

    #[test]
    fn the_state_round_trips_through_toml() {
        let mut paths = BTreeMap::new();
        paths.insert(
            "Source/a.cpp".to_string(),
            Touched {
                mode: "shared".into(),
                conflict: Some("both modified".into()),
                ours: Some(SideNote {
                    revision: 3,
                    file_mode: api::FileMode::Regular,
                }),
                theirs: None,
                line_head: None,
            },
        );
        let state = MergeState {
            version: STATE_VERSION,
            branch: "main".into(),
            source: "topic".into(),
            source_commit: "ab".repeat(32),
            target_commit: "cd".repeat(32),
            message: "Merge".into(),
            no_ff: false,
            paths,
            local: None,
        };
        let text = toml::to_string_pretty(&state).unwrap();
        let back: MergeState = toml::from_str(&text).unwrap();
        assert_eq!(back.paths, state.paths);
        assert_eq!(back.source_commit, state.source_commit);
    }
}

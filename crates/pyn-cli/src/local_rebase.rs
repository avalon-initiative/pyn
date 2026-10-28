//! `pyn rebase` in a `local_store` workspace: reorders, rewords, edits, squashes and drops the unpublished local
//! commits (docs/concepts/commits.md). The replay and its record are `local_merge`'s; the sheet is the git home's.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result, anyhow, bail};
use pyn_object::ObjectId;

use crate::backend::Backend;
use crate::branch;
use crate::git_home::RebaseArgs;
use crate::git_home::editor::{self, Which};
use crate::git_home::todo::{self, Cmd, Commits, Flag, Item};
use crate::local_commit::signature;
use crate::local_merge::{self, Move, Op, Rewrite, Stashed};
use crate::local_pick::{ancestors, require_nothing_staged, walk};
use crate::local_store::LocalStore;
use crate::merging;
use crate::snapshot::short;
use crate::workspace::Workspace;

/// Names commits of the local store for the sheet: a hash, `HEAD~N` or a branch.
struct Named<'a> {
    store: &'a LocalStore,
    own: String,
}

impl Commits for Named<'_> {
    fn find(&self, spec: &str) -> Option<ObjectId> {
        let full = match spec.strip_prefix("HEAD") {
            Some(rest) => format!("{}{rest}", self.own),
            None => spec.to_string(),
        };
        let id = self.store.repo().resolve(&full).ok()?;
        self.store.repo().commit(&id).ok().map(|_| id)
    }

    fn subject(&self, id: &ObjectId) -> Result<String> {
        let commit = self.store.repo().commit(id)?;
        Ok(String::from_utf8_lossy(&commit.message)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string())
    }

    fn label(&self, id: &ObjectId) -> Result<String> {
        self.subject(id)
    }

    fn is_empty(&self, _: &ObjectId) -> Result<bool> {
        Ok(false)
    }
}

fn state_dir(ws: &Workspace) -> std::path::PathBuf {
    ws.dir.join("local_only").join("state")
}

/// The message the person makes of `message` in their editor; an empty result aborts.
pub(crate) fn edit_message(ws: &Workspace, message: &[u8]) -> Result<Vec<u8>> {
    let mut text = message.to_vec();
    if !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    text.extend_from_slice(
        b"\n# Please enter the commit message for your changes. Lines starting\n# with '#' will be ignored, and an empty message aborts the commit.\n",
    );
    let path = state_dir(ws).join("rebase-message");
    std::fs::create_dir_all(state_dir(ws))?;
    std::fs::write(&path, &text)?;
    let command = editor::command_from(Which::Message, &|_| None)?;
    editor::run_command(&command, &ws.root, &path)?;
    let edited = std::fs::read(&path)?;
    let _ = std::fs::remove_file(&path);
    editor::edited_message(&edited)
        .ok_or_else(|| anyhow!("Aborting commit due to empty commit message."))
}

fn edit_sheet(ws: &Workspace, text: &str) -> Result<String> {
    std::fs::create_dir_all(state_dir(ws))?;
    let path = state_dir(ws).join("rebase-todo");
    std::fs::write(&path, text)?;
    let command = editor::command_from(Which::Sequence, &|_| None)?;
    let ran = editor::run_command(&command, &ws.root, &path);
    let edited = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    ran?;
    Ok(edited?)
}

fn refuse_unbuilt(args: &RebaseArgs) -> Result<()> {
    let given = [
        (
            "--rebase-merges",
            args.rebase_merges.is_some(),
            "a workspace cannot rebase a merge",
        ),
        (
            "--root",
            args.root,
            "a workspace rebases onto an upstream, not from the root",
        ),
        (
            "--fork-point",
            args.fork_point,
            "the upstream's reflog is not kept",
        ),
        (
            "--update-refs",
            args.update_refs,
            "other branches are not moved",
        ),
    ];
    if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
        bail!("`pyn rebase {flag}` is not available in a workspace: {why}");
    }
    if args.strategy.is_some() || !args.strategy_option.is_empty() {
        bail!("`pyn rebase -s` and `-X` are not available in a workspace: it merges one way");
    }
    if let Some(flag) = args.sign.first_given() {
        bail!("`pyn rebase {flag}` is not available in a workspace: its commits are not signed");
    }
    Ok(())
}

/// Which commits the commands of a sheet may name: unpublished ones, and no merges.
struct Guard<'a> {
    store: &'a LocalStore,
    published: ObjectId,
    already: HashSet<ObjectId>,
}

impl<'a> Guard<'a> {
    fn new(store: &'a LocalStore, published: ObjectId) -> Result<Self> {
        Ok(Guard {
            store,
            published,
            already: ancestors(store.repo(), &published)?,
        })
    }

    fn usable(&self, id: &ObjectId, subject: &str) -> Result<()> {
        if self.already.contains(id) {
            bail!(
                "commit {} ({subject}) is already published; `pyn rebase` rewrites only local commits, so name an upstream at or after the published tip {}; nothing was changed",
                short(&id.to_hex()),
                short(&self.published.to_hex())
            );
        }
        if self.store.repo().commit(id)?.parents.len() > 1 {
            bail!(
                "commit {} ({subject}) is a merge, which a workspace cannot rebase; nothing was changed",
                short(&id.to_hex())
            );
        }
        Ok(())
    }

    /// The commands of a sheet the replay can run; anything else is refused before anything changes.
    fn moves(&self, names: &Named<'_>, items: Vec<Item>) -> Result<Vec<Move>> {
        let mut moves = Vec::new();
        for item in items {
            if item.cmd == Cmd::Fixup && item.flag != Flag::None {
                bail!(
                    "`fixup -C` and `fixup -c` are not available in a workspace; nothing was changed"
                );
            }
            match item.cmd {
                Cmd::Noop | Cmd::Drop | Cmd::Comment => {}
                Cmd::Exec | Cmd::Break => moves.push(item),
                Cmd::Label | Cmd::Reset | Cmd::Merge | Cmd::UpdateRef => bail!(
                    "'{}' is not available in a workspace; use pick, reword, edit, squash, fixup, drop, exec or break; nothing was changed",
                    item.cmd.word()
                ),
                _ => {
                    let id = item.id.ok_or_else(|| {
                        anyhow!("a command without a commit; nothing was changed")
                    })?;
                    self.usable(&id, &names.subject(&id)?)?;
                    moves.push(item);
                }
            }
        }
        Ok(moves)
    }
}

fn edit_todo(ws: &Workspace, store: &LocalStore) -> Result<()> {
    local_merge::edit_todo(ws, |branch, todo| {
        let published = store
            .published(branch)?
            .context("the rebase has no published tip")?;
        let names = Named {
            store,
            own: format!("refs/heads/{branch}"),
        };
        let mut text = todo::sheet(&names, todo, true)?;
        text.push_str(&todo::edit_help(false));
        let edited = edit_sheet(ws, &text)?;
        let items = todo::parse(&names, &todo::stripspace(&edited), true)
            .map_err(|errors| anyhow!("{}\nnothing was changed", errors.join("\n")))?;
        Guard::new(store, published)?.moves(&names, items)
    })
}

pub fn rebase(backend: &dyn Backend, ws: &Workspace, args: &RebaseArgs) -> Result<()> {
    args.refuse_unsupported();
    refuse_unbuilt(args)?;
    let Some(store) = LocalStore::open(ws) else {
        bail!(
            "`pyn rebase` works on the local commits of a workspace that keeps them; `pyn migrate` converts an older one"
        );
    };
    if args.abort || args.continue_rebase || args.skip || args.quit || args.edit_todo {
        local_merge::require_op(ws, Op::Rewrite)?;
        return if args.abort {
            local_merge::abort(backend, ws)
        } else if args.quit {
            local_merge::quit(backend, ws)
        } else if args.edit_todo {
            edit_todo(ws, &store)
        } else if args.skip {
            local_merge::skip(backend, ws)
        } else {
            local_merge::finish(backend, ws, None)
        };
    }
    merging::refuse_during_merge(ws, "rebase")?;
    require_nothing_staged(ws, "rebase")?;
    let mut stash = BTreeMap::new();
    if args.autostash
        && let Some(tip) = store.tip(&branch::name(Some(ws)))?
    {
        stash = local_merge::stash_dirty(backend, ws, &store, tip)?;
    }
    let mut handed = false;
    let result = start(backend, ws, &store, args, &stash, &mut handed);
    if !handed && !stash.is_empty() && !merging::in_progress(ws) {
        let branch = branch::name(Some(ws));
        if let Some(tip) = store.tip(&branch)? {
            let onto = store.published(&branch)?.unwrap_or(tip);
            local_merge::unstash(backend, ws, &store, &stash, (tip, onto))?;
        }
    }
    result
}

/// Plans the rebase and runs it; `handed` says the autostash now belongs to the run.
fn start(
    backend: &dyn Backend,
    ws: &Workspace,
    store: &LocalStore,
    args: &RebaseArgs,
    stash: &BTreeMap<String, Stashed>,
    handed: &mut bool,
) -> Result<()> {
    if let Some(target) = &args.branch
        && *target != branch::name(Some(ws))
    {
        let switch = branch::SwitchArgs {
            start: Some(target.clone()),
            create: None,
            force_create: None,
            orphan: None,
            force: false,
            detach: false,
            recurse_submodules: false,
            no_recurse_submodules: false,
            guess: false,
            no_guess: false,
            quiet: false,
            track: branch::TrackArgs::default(),
        };
        branch::switch_command(backend, Some(ws), switch)?;
    }
    let branch = branch::name(Some(ws));
    let Some(tip) = store.tip(&branch)? else {
        bail!("branch '{branch}' has no local commits");
    };
    let Some(published) = store.published_or_base(&branch)? else {
        bail!("branch '{branch}' has not been fetched yet; run `pyn fetch` first");
    };
    let names = Named {
        store,
        own: format!("refs/heads/{branch}"),
    };
    let named = |spec: &Option<String>, what: &str, default: ObjectId| match spec {
        Some(s) => names
            .find(s)
            .ok_or_else(|| anyhow!("{what} '{s}'; nothing was changed")),
        None => Ok(default),
    };
    let upstream = named(&args.upstream, "invalid upstream", published)?;
    let base = named(&args.onto, "does not point to a valid commit", upstream)?;
    let repo = store.repo();
    let guard = Guard::new(store, published)?;
    let range = walk(repo, &[tip], ancestors(repo, &upstream)?)?;
    for id in &range {
        guard.usable(id, &names.subject(id)?)?;
    }
    if !ancestors(repo, &base)?.contains(&published) {
        bail!(
            "the new base does not contain the published tip {}, so the result could not be pushed; nothing was changed",
            short(&published.to_hex())
        );
    }
    let sheet_run = args.interactive || args.autosquash || !args.exec.is_empty();
    if range.is_empty() {
        println!("Current branch {branch} is up to date.");
        return Ok(());
    }
    if !sheet_run && args.onto.is_none() && ancestors(repo, &tip)?.contains(&upstream) {
        if !args.no_ff {
            println!("Current branch {branch} is up to date.");
            return Ok(());
        }
        println!("Current branch {branch} is up to date, rebase forced.");
    }
    let mut items: Vec<Item> = range.into_iter().map(Item::pick).collect();
    if args.autosquash && !args.no_autosquash {
        items = todo::autosquash(&names, items, false)?;
    }
    if !args.exec.is_empty() {
        items = todo::with_exec(items, &args.exec);
    }
    if args.interactive {
        let span = format!("{}..{}", &upstream.to_hex()[..7], &tip.to_hex()[..7]);
        let text = todo::editor_text(&names, &items, &span, &base)?;
        let edited = edit_sheet(ws, &text)?;
        items = todo::parse(&names, &todo::stripspace(&edited), false)
            .map_err(|errors| anyhow!("{}\nnothing was changed", errors.join("\n")))?;
        if items.iter().all(|i| !i.counts()) {
            bail!("nothing to do");
        }
    }
    let moves = guard.moves(&names, items)?;
    let who = signature(&backend.whoami()?);
    local_merge::begin_rewrite(
        backend,
        ws,
        store,
        &branch,
        Rewrite {
            moves,
            base,
            tip,
            published,
            no_ff: args.no_ff,
            stash: stash.clone(),
        },
        &who,
    )?;
    *handed = true;
    Ok(())
}

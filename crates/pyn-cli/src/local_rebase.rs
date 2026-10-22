//! `pyn rebase` in a `local_store` workspace: reorders, rewords, squashes and drops the unpublished local commits
//! (docs/concepts/commits.md). The replay and its record are `local_merge`'s; the sheet is the git home's.

use anyhow::{Result, anyhow, bail};
use pyn_object::ObjectId;

use crate::backend::Backend;
use crate::branch;
use crate::git_home::RebaseArgs;
use crate::git_home::editor::{self, Which};
use crate::git_home::todo::{self, Cmd, Commits, Flag, Item};
use crate::local_commit::signature;
use crate::local_merge::{self, Move, Op, Rewrite};
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
            "<branch>",
            args.branch.is_some(),
            "a workspace rebases the branch it is on; `pyn switch` first",
        ),
        (
            "--autostash",
            args.autostash || args.no_autostash,
            "commit or put back your changes first",
        ),
        (
            "--no-ff",
            args.no_ff || args.ff,
            "commits are reused when nothing changes",
        ),
        ("--quit", args.quit, "`--abort` is the way out"),
        (
            "--exec",
            !args.exec.is_empty(),
            "running commands between commits is not built",
        ),
        (
            "--edit-todo",
            args.edit_todo,
            "the sheet is edited once, before the rebase starts",
        ),
    ];
    if let Some((flag, _, why)) = given.iter().find(|(_, on, _)| *on) {
        bail!("`pyn rebase {flag}` is not available in a workspace: {why}");
    }
    Ok(())
}

pub fn rebase(backend: &dyn Backend, ws: &Workspace, args: &RebaseArgs) -> Result<()> {
    args.refuse_unsupported();
    refuse_unbuilt(args)?;
    let Some(store) = LocalStore::open(ws) else {
        bail!(
            "`pyn rebase` works on the local commits of a workspace that keeps them; `pyn migrate` converts an older one"
        );
    };
    if args.abort || args.continue_rebase || args.skip {
        local_merge::require_op(ws, Op::Rewrite)?;
        return if args.abort {
            local_merge::abort(backend, ws)
        } else if args.skip {
            local_merge::skip(backend, ws)
        } else {
            local_merge::finish(backend, ws, None)
        };
    }
    merging::refuse_during_merge(ws, "rebase")?;
    require_nothing_staged(ws, "rebase")?;
    let branch = branch::name(Some(ws));
    let Some(tip) = store.tip(&branch)? else {
        bail!("branch '{branch}' has no local commits");
    };
    let Some(published) = store.published(&branch)? else {
        bail!("branch '{branch}' has not been fetched yet; run `pyn fetch` first");
    };
    let names = Named {
        store: &store,
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
    let already = ancestors(repo, &published)?;
    let range = walk(repo, &[tip], ancestors(repo, &upstream)?)?;
    let usable = |id: &ObjectId| -> Result<()> {
        let subject = names.subject(id)?;
        if already.contains(id) {
            bail!(
                "commit {} ({subject}) is already published; `pyn rebase` rewrites only local commits, so name an upstream at or after the published tip {}; nothing was changed",
                short(&id.to_hex()),
                short(&published.to_hex())
            );
        }
        if repo.commit(id)?.parents.len() > 1 {
            bail!(
                "commit {} ({subject}) is a merge, which a workspace cannot rebase; nothing was changed",
                short(&id.to_hex())
            );
        }
        Ok(())
    };
    for id in &range {
        usable(id)?;
    }
    if !ancestors(repo, &base)?.contains(&published) {
        bail!(
            "the new base does not contain the published tip {}, so the result could not be pushed; nothing was changed",
            short(&published.to_hex())
        );
    }
    let sheet_run = args.interactive || args.autosquash;
    if range.is_empty()
        || (!sheet_run && args.onto.is_none() && ancestors(repo, &tip)?.contains(&upstream))
    {
        println!("Current branch {branch} is up to date.");
        return Ok(());
    }
    let mut items: Vec<Item> = range.into_iter().map(Item::pick).collect();
    if args.autosquash && !args.no_autosquash {
        items = todo::autosquash(&names, items, false)?;
    }
    if args.interactive {
        let span = format!("{}..{}", &upstream.to_hex()[..7], &tip.to_hex()[..7]);
        let text = todo::editor_text(&names, &items, &span, &base)?;
        let edited = edit_sheet(ws, &text)?;
        items = todo::parse(&names, &edited, false)
            .map_err(|errors| anyhow!("{}\nnothing was changed", errors.join("\n")))?;
        if items.is_empty() {
            bail!("nothing to do");
        }
    }
    let mut moves: Vec<Move> = Vec::new();
    for item in items {
        if item.flag != Flag::None {
            bail!(
                "`fixup -C` and `fixup -c` are not available in a workspace; nothing was changed"
            );
        }
        match (item.cmd, item.id) {
            (Cmd::Noop | Cmd::Drop, _) => {}
            (Cmd::Edit | Cmd::Exec | Cmd::Break, _) => bail!(
                "'{}' is not available in a workspace; use pick, reword, squash, fixup or drop; nothing was changed",
                item.cmd.word()
            ),
            (cmd, Some(id)) => {
                usable(&id)?;
                moves.push((cmd, id));
            }
            _ => bail!("a command without a commit; nothing was changed"),
        }
    }
    let who = signature(&backend.whoami()?);
    local_merge::begin_rewrite(
        backend,
        ws,
        &store,
        &branch,
        Rewrite {
            moves,
            base,
            tip,
            published,
        },
        &who,
    )
}

//! `pyn merge` on a git repository: fast-forward, a merge commit with two parents, or a merge left in progress with
//! unmerged index entries and `MERGE_HEAD`, `MERGE_MSG` and `MERGE_MODE` written as git writes them, so either tool
//! can finish or abort it.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow, bail};
use pyn_git::IndexEntry;
use pyn_gitwork::{Entry, Head, Kind, Pathspec};
use pyn_object::{Commit, Kind as ObjectKind, ObjectId};

use super::commands::add_changes;
use super::merge_options::{self as options, Cleanup};
use super::stage::{
    Entries, clear_merge_state, entries_of, identity, index_of, merge_heads, put_entry, raw_mode,
    refuse_in_progress, remove_entry, write_tree,
};
use super::strategy::{BadOption, Strategy, Which, shift_tree};
use super::treemerge::{
    Conflicting, Labels, Merge, Resolved, Worktree, merge_trees_with, result_tree,
};
use super::virtual_base::{VIRTUAL_LABEL, virtual_tree};
use super::worktree::{
    Blocked, Mode, blocking_path, check_writes, move_tree, remove_entry_file, within, write_bytes,
    write_entry,
};
use super::{GitHome, Refused, convert, path_string};
use crate::commit_format::{Context, DateStyle, Pretty, render};
use crate::diff::{self, Tree};
use crate::diffstat;
use crate::merging::MergeArgs;

const ABBREV: usize = 7;

/// Set by `pyn pull -q`: the informational lines below are not printed.
/// The reflog action when a command other than `merge` runs the merge (`pull ...`).
pub(super) static ACTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn action(spec: &str) -> String {
    ACTION
        .get()
        .cloned()
        .unwrap_or_else(|| format!("merge {spec}"))
}

pub(super) static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The exit status when local changes block a fast-forward: 2 for `merge`, 1 for `pull`.
pub(super) static BLOCKED_STATUS: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(2);

fn blocked_status() -> i32 {
    BLOCKED_STATUS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The stash `--autostash` made, until the merge ends or stops.
static AUTOSTASH: std::sync::Mutex<Option<ObjectId>> = std::sync::Mutex::new(None);

/// Puts the autostash back when the merge is over, or holds it while a merge or squash waits to be concluded.
fn settle(git: &GitHome) {
    let Some(stash) = AUTOSTASH.lock().ok().and_then(|mut s| s.take()) else {
        return;
    };
    let dir = git.repo.git().git_dir();
    if dir.join("MERGE_HEAD").exists() || dir.join("SQUASH_MSG").exists() {
        options::hold_autostash(git, &stash);
    } else {
        options::restore_autostash(git, &stash);
    }
}

/// Ends the command with `status` after settling the autostash.
fn leave(git: &GitHome, status: i32) -> ! {
    settle(git);
    std::process::exit(status);
}

fn say(text: &str) {
    if !QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        println!("{text}");
    }
}
const MERGE_HOOKS: [&str; 4] = [
    "pre-merge-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-merge",
];

pub(super) fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

/// How git names what is being merged in a message: `branch 'x'`, `tag 'x'`, `commit 'x'`.
fn describe(git: &GitHome, spec: &str) -> String {
    let refs = git.repo.refs();
    let exists = |name: &str| {
        pyn_git::check_ref_name(name).is_ok() && matches!(refs.resolve(name), Ok(Some(_)))
    };
    for (prefix, what) in [
        ("refs/tags/", "tag"),
        ("refs/heads/", "branch"),
        ("refs/remotes/", "remote-tracking branch"),
    ] {
        if let Some(short) = spec.strip_prefix(prefix)
            && exists(spec)
        {
            return format!("{what} '{short}'");
        }
        if exists(&format!("{prefix}{spec}")) {
            return format!("{what} '{spec}'");
        }
    }
    format!("commit '{spec}'")
}

/// The message of the annotated tag a merge names, which git appends to the merge message.
fn tag_message(git: &GitHome, spec: &str) -> Option<String> {
    let name = format!("refs/tags/{spec}");
    if pyn_git::check_ref_name(&name).is_err() {
        return None;
    }
    let id = git.repo.refs().resolve(&name).ok()??;
    let object = git.repo.objects().read(&id).ok()?;
    if object.kind != ObjectKind::Tag {
        return None;
    }
    let tag = pyn_object::Tag::parse(git.repo.algorithm(), &object.data).ok()?;
    Some(String::from_utf8_lossy(&tag.message).trim_end().to_string())
}

pub(super) fn default_message(git: &GitHome, what: &str) -> Result<String> {
    default_message_as(git, what, None)
}

fn default_message_as(git: &GitHome, what: &str, into_name: Option<&str>) -> Result<String> {
    let branch = match git.repo.head()? {
        Head::Branch { name, .. } => Some(name),
        Head::Detached(_) => None,
    };
    let into = options::destination(git, into_name, branch.as_deref());
    Ok(format!("Merge {what}{into}"))
}

pub(super) fn write_state(git: &GitHome, name: &str, text: &str) -> Result<()> {
    let path = git.repo.git().git_dir().join(name);
    std::fs::write(&path, text).map_err(|e| anyhow!("writing {}: {e}", path.display()))
}

/// `ORIG_HEAD` is where `HEAD` was before a merge, as git leaves it for `git reset --merge`.
pub(super) fn write_orig_head(git: &GitHome, id: &ObjectId) -> Result<()> {
    write_orig_head_as(git, id, None)
}

/// Writes `ORIG_HEAD`, logging it as git does when it already has a reflog or `core.logAllRefUpdates` is `always`.
pub(super) fn write_orig_head_as(git: &GitHome, id: &ObjectId, action: Option<&str>) -> Result<()> {
    let old = git.repo.refs().resolve("ORIG_HEAD")?;
    let update = pyn_git::RefUpdate {
        name: "ORIG_HEAD".into(),
        old: None,
        new: Some(*id),
    };
    git.repo.refs().transaction(&[update], || {
        write_state(git, "ORIG_HEAD", &format!("{id}\n"))
    })?;
    let message = match action {
        Some(a) => format!("{a}: updating ORIG_HEAD"),
        None => "updating ORIG_HEAD".to_string(),
    };
    git.log_ref("ORIG_HEAD", old.as_ref(), id, &message, false)
}

pub fn merge(git: &GitHome, args: MergeArgs) -> Result<()> {
    let result = start(git, args);
    settle(git);
    result
}

fn start(git: &GitHome, mut args: MergeArgs) -> Result<()> {
    git.repo.validate_diff_ignore_submodules()?;
    if args.preview {
        bail!("`pyn merge --preview` needs a pyn server; a git repository has no merge preview");
    }
    if args.abort {
        return abort(git);
    }
    if args.quit {
        return options::quit(git);
    }
    if args.continue_merge {
        if merge_heads(git)?.is_empty() {
            bail!("There is no merge in progress (MERGE_HEAD missing).");
        }
        return super::commit::commit(
            git,
            &super::commit::CommitOptions {
                messages: args.message.clone(),
                ..Default::default()
            },
        );
    }
    let Some(spec) = args.source.clone() else {
        bail!("no merge target given; name a branch or a commit");
    };
    if args.squash && args.no_ff {
        bail!("options '--squash' and '--no-ff.' cannot be used together");
    }
    options::apply_ff_setting(git, &mut args);
    args.edit = wants_edit(&args);
    args.no_edit = !args.edit;
    options::check_flags(git, &args);
    if args.quiet {
        QUIET.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let configured: Vec<String> = match git.repo.config().get("pull.twohead") {
        Some(names) if args.strategy.is_empty() => {
            names.split_whitespace().map(str::to_string).collect()
        }
        _ => args.strategy.clone(),
    };
    let strategy = Strategy::parse(&configured, &args.strategy_option);
    let opened = git.open_index()?;
    refuse_unfinished(git, &opened);
    refuse_in_progress(git)?;
    let head = git.repo.head()?;
    let theirs = git
        .repo
        .resolve(&spec)
        .map_err(|_| Refused(format!("{spec} - not something we can merge")))?;
    if options::wants_signature_check(git, &args) {
        options::verify_signature(git, &theirs);
    }
    let what = describe(git, &spec);
    let Some(ours) = head.id() else {
        if args.squash {
            bail!("Squash commit into empty head not supported yet");
        }
        return fast_forward(git, &opened, None, &theirs, &spec, &args);
    };
    if git.is_ancestor(&theirs, &ours)? {
        write_orig_head(git, &ours)?;
        say(if args.squash {
            "Already up to date. (nothing to squash)"
        } else {
            "Already up to date."
        });
        return Ok(());
    }
    if strategy.kind.no_fast_forward() {
        (args.no_ff, args.ff_only) = (true, false);
    }
    let can_ff = git.is_ancestor(&ours, &theirs)?;
    let opened = if options::wants_autostash(git, &args) {
        let made = options::create_autostash(git)?;
        if let Ok(mut held) = AUTOSTASH.lock() {
            *held = made;
        }
        git.open_index()?
    } else {
        opened
    };
    if can_ff && !args.no_ff {
        return fast_forward(git, &opened, Some(ours), &theirs, &spec, &args);
    }
    if args.ff_only {
        write_orig_head(git, &ours)?;
        bail!("Not possible to fast-forward, aborting.");
    }
    true_merge(
        git,
        &args,
        &opened,
        (ours, theirs),
        (&spec, &what),
        &strategy,
    )
}

/// `SQUASH_MSG`: the commits `theirs` brings in, newest first, as `log` prints them.
fn squash_message(git: &GitHome, ours: &ObjectId, theirs: &ObjectId) -> Result<String> {
    let mut text = String::from("Squashed commit of the following:\n");
    let ctx = Context {
        style: DateStyle::Default,
        now: 0,
        decorations: &Default::default(),
        commit_color: None,
        tabs: 8,
        abbrev_commit: false,
    };
    let mut walk = git
        .repo
        .log_hiding(&[*theirs], git.reachable(ours)?, Pathspec::default())?;
    while let Some(entry) = walk.next_entry()? {
        text.push('\n');
        text.push_str(&render(&Pretty::Medium, &ctx, &entry.id, &entry.commit));
    }
    Ok(text)
}

/// Whether the merge message opens in the editor: the flags, then an explicit `-m` (no), `GIT_MERGE_AUTOEDIT`, a terminal.
fn wants_edit(args: &MergeArgs) -> bool {
    use std::io::IsTerminal;
    if args.edit || args.no_edit {
        return args.edit;
    }
    if !args.message.is_empty() && !args.generated_message {
        return false;
    }
    if let Ok(value) = std::env::var("GIT_MERGE_AUTOEDIT") {
        let Some(on) = super::decorate::maybe_bool(&value) else {
            eprintln!("fatal: Bad value '{value}' in environment 'GIT_MERGE_AUTOEDIT'");
            std::process::exit(128);
        };
        return on;
    }
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

const EDITOR_COMMENT: &str = "# Please enter a commit message to explain why this merge is necessary,\n# especially if it merges an updated upstream into a topic branch.\n#\n# Lines starting with '#' will be ignored, and an empty message aborts\n# the commit.\n";

/// Git's refusals to start a merge on unmerged paths or over a merge or cherry-pick not yet concluded.
fn refuse_unfinished(git: &GitHome, opened: &super::stage::OpenIndex) {
    let unmerged: Vec<&[u8]> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.stage > 0)
        .map(|e| e.path.as_slice())
        .collect();
    if !unmerged.is_empty() {
        eprintln!("error: Merging is not possible because you have unmerged files.");
        eprintln!("hint: Fix them up in the work tree, and then use 'git add/rm <file>'");
        eprintln!("hint: as appropriate to mark resolution and make a commit.");
        eprintln!("fatal: Exiting because of an unresolved conflict.");
        std::process::exit(128);
    }
    let dir = git.repo.git().git_dir();
    for (file, what) in [("MERGE_HEAD", "merge"), ("CHERRY_PICK_HEAD", "cherry-pick")] {
        if dir.join(file).exists() {
            eprintln!(
                "fatal: You have not concluded your {what} ({file} exists).\nPlease, commit your changes before you merge."
            );
            std::process::exit(128);
        }
    }
}

/// Checks `theirs` out onto an unborn branch and points the branch at it, without a word, as `git pull` does.
pub(super) fn initial_checkout(git: &GitHome, theirs: &ObjectId, action: &str) -> Result<()> {
    let opened = git.open_index()?;
    let tree = git.repo.commit(theirs)?.tree;
    if let Err(blocked) = move_tree(git, &opened, None, Some(&tree), Mode::Safe)? {
        blocked.report("merge", "merge", blocked_status());
    }
    git.move_head(theirs, action)
}

/// Prints git's diffstat for a merge or fast-forward unless `-n`, `merge.stat=false` or `-q` says not to.
fn show_stat(
    git: &GitHome,
    args: &MergeArgs,
    old: Option<&ObjectId>,
    new: &ObjectId,
) -> Result<()> {
    let config = git.config();
    let enabled = match (args.stat, args.no_stat) {
        (true, _) => true,
        (_, true) => false,
        _ => ["merge.stat", "merge.diffstat"]
            .iter()
            .find_map(|k| {
                let v = config.get(k)?;
                config
                    .get_bool(k)
                    .ok()
                    .flatten()
                    .or_else(|| v.eq_ignore_ascii_case("compact").then_some(true))
            })
            .unwrap_or(true),
    };
    if !enabled || QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    let (mut before, mut after) = (Tree::new(), Tree::new());
    let changes = git.repo.diff_trees(old, Some(new), &Pathspec::default())?;
    add_changes(git, &mut before, &mut after, &changes)?;
    let graph = config
        .get("diff.statgraphwidth")
        .and_then(|w| w.parse::<usize>().ok());
    let text = diff::merge_stat(git, &before, &after, (diffstat::terminal_width(), graph))?;
    print!("{text}");
    Ok(())
}

fn fast_forward(
    git: &GitHome,
    opened: &super::stage::OpenIndex,
    ours: Option<ObjectId>,
    theirs: &ObjectId,
    spec: &str,
    args: &MergeArgs,
) -> Result<()> {
    let old_tree = match &ours {
        Some(c) => Some(git.repo.commit(c)?.tree),
        None => None,
    };
    let new_tree = git.repo.commit(theirs)?.tree;
    if let Some(ours) = &ours {
        say(&format!("Updating {}..{}", short(ours), short(theirs)));
    }
    if let Err(blocked) = move_tree(git, opened, old_tree.as_ref(), Some(&new_tree), Mode::Safe)? {
        if let Some(ours) = &ours {
            write_orig_head(git, ours)?;
        }
        blocked.print("merge", "merge");
        eprintln!("Aborting");
        leave(git, 1);
    }
    if let Some(ours) = &ours {
        write_orig_head(git, ours)?;
    }
    if args.squash {
        let ours = ours.expect("a squash needs a commit to merge into");
        say("Fast-forward");
        write_state(git, "SQUASH_MSG", &squash_message(git, &ours, theirs)?)?;
        say("Squash commit -- not updating HEAD");
        show_stat(git, args, old_tree.as_ref(), &new_tree)?;
        git.warn_if_hooks_off(&MERGE_HOOKS);
        git.run_hook("post-merge", &["1".to_string()], None, &[])?;
        return Ok(());
    }
    git.move_head(theirs, &format!("{}: Fast-forward", action(spec)))?;
    say("Fast-forward");
    if ours.is_some() {
        show_stat(git, args, old_tree.as_ref(), &new_tree)?;
    }
    git.warn_if_hooks_off(&MERGE_HOOKS);
    git.run_hook("post-merge", &["0".to_string()], None, &[])?;
    Ok(())
}

/// The paths whose index entry differs from `HEAD`'s tree, sorted.
fn index_changes(
    git: &GitHome,
    opened: &super::stage::OpenIndex,
    ours_tree: Option<&ObjectId>,
) -> Result<Vec<Vec<u8>>> {
    let head_leaves = git.repo.flatten(ours_tree, &Pathspec::default())?;
    let index_leaves = git.repo.index_leaves(&opened.index);
    let mut changed: Vec<Vec<u8>> = head_leaves
        .keys()
        .chain(index_leaves.keys())
        .filter(|p| head_leaves.get(*p) != index_leaves.get(*p))
        .cloned()
        .collect();
    changed.sort();
    changed.dedup();
    Ok(changed)
}

/// Ends a merge a strategy could not start, as git does: `ORIG_HEAD` is kept and nothing else changes.
fn fail_start(
    git: &GitHome,
    ours: &ObjectId,
    spec: &str,
    kind: Which,
    index_differs: bool,
) -> Result<std::convert::Infallible> {
    write_orig_head(git, ours)?;
    if index_differs {
        git.log_ref(
            "HEAD",
            Some(ours),
            ours,
            &format!("{}: updating HEAD", action(spec)),
            false,
        )?;
    }
    eprintln!("Merge with strategy {} failed.", kind.name());
    leave(git, 2);
}

fn true_merge(
    git: &GitHome,
    args: &MergeArgs,
    opened: &super::stage::OpenIndex,
    (ours, theirs): (ObjectId, ObjectId),
    (spec, what): (&str, &str),
    strategy: &Strategy,
) -> Result<()> {
    let kind = strategy.kind;
    let bases = git.merge_bases(&ours, &theirs)?;
    let unrelated = bases.is_empty();
    if unrelated && !args.allow_unrelated_histories {
        write_orig_head(git, &ours)?;
        bail!("refusing to merge unrelated histories");
    }
    let tree_of = |c: &ObjectId| -> Result<ObjectId> { Ok(git.repo.commit(c)?.tree) };
    let ours_tree = tree_of(&ours)?;
    let finish = Finish {
        git,
        args,
        ours,
        theirs,
        spec,
        what,
        ours_tree,
    };
    if kind == Which::Ours {
        if !index_changes(git, opened, Some(&ours_tree))?.is_empty() {
            match fail_start(git, &ours, spec, kind, true)? {}
        }
        write_orig_head(git, &ours)?;
        return finish.conclude(Vec::new(), None, &Concluding::Strategy(kind), || {
            Ok(ours_tree)
        });
    }
    let mut theirs_tree = tree_of(&theirs)?;
    let settings = if kind.is_tree_merge() {
        match strategy.settings() {
            Ok(mut settings) => {
                settings.resolve_renames(git);
                settings
            }
            Err(BadOption::Unknown(opt) | BadOption::Unsupported(opt, _)) => {
                write_orig_head(git, &ours)?;
                eprintln!("fatal: unknown strategy option: -X{opt}");
                leave(git, 128);
            }
        }
    } else {
        Default::default()
    };
    if let Some(prefix) = &settings.shift {
        theirs_tree = shift_tree(git, &ours_tree, &theirs_tree, prefix)?;
    }
    let (mut base_tree, base_label) = match bases.as_slice() {
        [] => {
            if !kind.is_tree_merge() || settings.shift.is_some() {
                bail!(
                    "the strategy '{}' with unrelated histories is not built, and nothing was changed",
                    kind.name()
                );
            }
            (None, "empty tree".to_string())
        }
        [one] => (Some(tree_of(one)?), short(one)),
        many => {
            if !kind.is_tree_merge() {
                bail!(
                    "the strategy '{}' with more than one merge base is not built, and nothing was changed",
                    kind.name()
                );
            }
            if settings.shift.is_some() {
                bail!(
                    "a subtree merge of histories with more than one merge base is not built, and nothing was changed"
                );
            }
            (
                Some(virtual_tree(git, many, &settings, 0)?),
                VIRTUAL_LABEL.to_string(),
            )
        }
    };
    if let (Some(prefix), Some(base)) = (&settings.shift, &base_tree) {
        base_tree = Some(shift_tree(git, &ours_tree, base, prefix)?);
    }
    let incoming =
        git.repo
            .diff_trees(base_tree.as_ref(), Some(&theirs_tree), &Pathspec::default())?;
    convert::refuse_unsupported(
        git,
        incoming
            .iter()
            .filter_map(|c| c.new.map(|e| (c.path.as_slice(), e.mode))),
    )?;
    let merged = merge_trees_with(
        git,
        [base_tree.as_ref(), Some(&ours_tree), Some(&theirs_tree)],
        &Labels {
            ours: "HEAD",
            theirs: spec,
            base: &base_label,
        },
        &settings,
        kind.is_tree_merge(),
    )?;
    let mut how = Concluding::Strategy(kind);
    if kind.tries_trivial() && !args.no_commit {
        let changed = index_changes(git, opened, Some(&ours_tree))?;
        if !changed.is_empty() {
            write_orig_head(git, &ours)?;
            let names: Vec<String> = changed.iter().map(|p| path_string(p)).collect();
            eprintln!(
                "error: Your local changes to the following files would be overwritten by merge:\n  {}",
                names.join(" ")
            );
            leave(git, 2);
        }
        println!("Trying really trivial in-index merge...");
        if merged.trivial {
            println!("Wonderful.");
            how = Concluding::InIndex;
        } else {
            println!("Nope.");
        }
    }
    if matches!(how, Concluding::Strategy(_))
        && (kind == Which::Octopus || (kind == Which::Resolve && strategy.has_options()))
    {
        if let (Which::Resolve, Some(opt)) = (kind, strategy.first_option()) {
            eprintln!("error: unknown option `{opt}'");
        }
        match fail_start(git, &ours, spec, kind, false)? {}
    }
    let landing = Landing {
        theirs: spec,
        orig_head: Some(ours),
        strict_index: true,
        quiet_when_clean: false,
        stages_at_origin: false,
    };
    let applied = match apply_merge(git, opened, Some(&ours_tree), &merged, &landing)? {
        Ok(applied) => applied,
        Err(refusal) => {
            write_orig_head(git, &ours)?;
            if refusal.index_differs {
                git.log_ref(
                    "HEAD",
                    Some(&ours),
                    &ours,
                    &format!("{}: updating HEAD", action(spec)),
                    false,
                )?;
            }
            if refusal.index_check {
                let names: Vec<String> = refusal
                    .blocked
                    .local
                    .iter()
                    .map(|p| path_string(p))
                    .collect();
                eprintln!(
                    "error: Your local changes to the following files would be overwritten by merge:\n  {}",
                    names.join(" ")
                );
            } else {
                refusal.blocked.print("merge", "merge");
                eprintln!("Aborting");
            }
            eprintln!("Merge with strategy {} failed.", kind.name());
            leave(git, 2);
        }
    };
    let (index, conflicted) = (applied.index, applied.conflicted);
    let stops = !conflicted.is_empty() || args.no_commit || args.squash || finish.message_stage();
    let auto_merge = if kind.writes_auto_merge() && stops {
        Some(result_tree(git, &merged)?)
    } else {
        None
    };
    finish.conclude(conflicted, auto_merge, &how, || write_tree(git, &index))
}

/// How a merge that did not conflict is described: by its strategy, or as the plain in-index merge.
enum Concluding {
    Strategy(Which),
    InIndex,
}

impl Finish<'_> {
    /// Whether the message goes through the editor or a message hook, any of which can stop the merge.
    fn message_stage(&self) -> bool {
        let (git, args) = (self.git, self.args);
        let wanted = |h: &str| git.hook_present(h);
        args.edit
            || wanted("prepare-commit-msg")
            || (!args.no_verify && (wanted("pre-merge-commit") || wanted("commit-msg")))
    }

    /// Runs the merge commit's hooks and editor over `MERGE_MSG`; a veto or an empty message leaves the merge in progress.
    fn message_hooks(&self, message: &str, mode: &str) -> Result<String> {
        let (git, args) = (self.git, self.args);
        git.warn_if_hooks_off(&MERGE_HOOKS);
        let verify = !args.no_verify;
        if !self.message_stage() {
            return Ok(message.to_string());
        }
        write_state(git, "MERGE_HEAD", &format!("{}\n", self.theirs))?;
        write_state(git, "MERGE_MSG", message)?;
        write_state(git, "MERGE_MODE", mode)?;
        let mut env = vec![("GIT_INDEX_FILE", git.hook_index())];
        if !args.edit {
            env.push(("GIT_EDITOR", ":".to_string()));
        }
        let path = git.repo.git().git_dir().join("MERGE_MSG");
        let file = git.hook_path(&path);
        let abort = |reason: Option<String>| -> ! {
            // git leaves the message as generated, whatever the hooks and editor did to it
            let _ = write_state(git, "MERGE_MSG", message);
            if let Some(reason) = reason {
                eprintln!("error: {reason}");
            }
            eprintln!("Not committing merge; use 'pyn commit' to complete the merge.");
            leave(git, 1);
        };
        if verify && git.vetoed("pre-merge-commit", &[], &env)? {
            abort(None);
        }
        let cleanup = options::cleanup_mode(args);
        if args.edit && cleanup == Cleanup::Strip {
            write_state(git, "MERGE_MSG", &format!("{message}{EDITOR_COMMENT}"))?;
        }
        if git.vetoed(
            "prepare-commit-msg",
            &[file.clone(), "merge".to_string()],
            &env,
        )? {
            abort(None);
        }
        if args.edit
            && let Err(e) = super::editor::run(git, super::editor::Which::Message, &path)
        {
            abort(Some(e.to_string()));
        }
        if verify && git.vetoed("commit-msg", &[file], &env)? {
            abort(None);
        }
        let text = std::fs::read(&path)?;
        let cleaned = match (args.edit, cleanup) {
            (true, Cleanup::Strip) => super::editor::edited_message(&text),
            (true, _) => {
                let kept = options::cleaned(text, cleanup);
                (kept.iter().any(|b| !b.is_ascii_whitespace())).then_some(kept)
            }
            (false, _) => Some(options::cleaned(text, cleanup)),
        };
        match cleaned {
            Some(text) => Ok(String::from_utf8_lossy(&text).into_owned()),
            None => abort(Some("Empty commit message.".into())),
        }
    }
}

/// What a merge needs to end: stop for conflicts or `--no-commit`, or make the merge commit.
struct Finish<'a> {
    git: &'a GitHome,
    args: &'a MergeArgs,
    ours: ObjectId,
    theirs: ObjectId,
    spec: &'a str,
    what: &'a str,
    ours_tree: ObjectId,
}

impl Finish<'_> {
    /// The message before hooks and the editor: `-m` or `-F`, else the generated title, then the `--log` shortlog.
    fn initial_message(&self) -> Result<String> {
        let (git, args) = (self.git, self.args);
        let mut text = match options::given_message(args)? {
            Some(raw) => raw,
            None => {
                let title = default_message_as(git, self.what, args.into_name.as_deref())?;
                let made = match tag_message(git, self.spec) {
                    Some(tag) if self.what.starts_with("tag ") => format!("{title}\n\n{tag}\n"),
                    _ => format!("{title}\n"),
                };
                match options::cleanup_mode(args) {
                    Cleanup::Verbatim => made.trim_end_matches('\n').as_bytes().to_vec(),
                    _ => made.into_bytes(),
                }
            }
        };
        let limit = options::log_limit(git, args);
        if limit > 0 {
            let label = match self.what {
                w if w.starts_with("branch '") || w.starts_with("remote-tracking branch '") => {
                    self.spec.to_string()
                }
                w => w.to_string(),
            };
            let log = options::shortlog(git, &self.ours, &self.theirs, &label, limit)?;
            if !log.is_empty() {
                if text.last() != Some(&b'\n') {
                    text.push(b'\n');
                }
                text.push(b'\n');
                text.extend_from_slice(log.as_bytes());
            }
        }
        let mode = options::cleanup_mode(args);
        let editing = args.edit && mode == Cleanup::Strip;
        let text = if editing {
            options::cleaned(text, Cleanup::Whitespace)
        } else {
            options::cleaned(text, mode)
        };
        Ok(String::from_utf8_lossy(&text).into_owned())
    }

    fn conclude(
        &self,
        conflicted: Vec<Vec<u8>>,
        auto_merge: Option<ObjectId>,
        how: &Concluding,
        tree: impl FnOnce() -> Result<ObjectId>,
    ) -> Result<()> {
        let Self {
            git,
            args,
            ours,
            theirs,
            spec,
            ours_tree,
            ..
        } = self;
        let message = self.initial_message()?;
        let mode = if args.no_ff { "no-ff" } else { "" };
        if let Some(id) = auto_merge {
            write_state(git, "AUTO_MERGE", &format!("{id}\n"))?;
        }
        if args.squash {
            write_state(git, "SQUASH_MSG", &squash_message(git, ours, theirs)?)?;
            if !conflicted.is_empty() {
                let mut text = String::from("\n# Conflicts:\n");
                for p in &conflicted {
                    text.push_str(&format!("#\t{}\n", path_string(p)));
                }
                write_state(git, "MERGE_MSG", &text)?;
                println!("Squash commit -- not updating HEAD");
                println!("Automatic merge failed; fix conflicts and then commit the result.");
                leave(git, 1);
            }
            eprintln!("Automatic merge went well; stopped before committing as requested");
            println!("Squash commit -- not updating HEAD");
            git.warn_if_hooks_off(&MERGE_HOOKS);
            git.run_hook("post-merge", &["1".to_string()], None, &[])?;
            return Ok(());
        }
        if !conflicted.is_empty() {
            let mut text = format!("{message}\n# Conflicts:\n");
            for p in &conflicted {
                text.push_str(&format!("#\t{}\n", path_string(p)));
            }
            write_state(git, "MERGE_HEAD", &format!("{theirs}\n"))?;
            write_state(git, "MERGE_MSG", &text)?;
            write_state(git, "MERGE_MODE", mode)?;
            println!("Automatic merge failed; fix conflicts and then commit the result.");
            leave(git, 1);
        }
        if args.no_commit {
            write_state(git, "MERGE_HEAD", &format!("{theirs}\n"))?;
            write_state(git, "MERGE_MSG", &message)?;
            write_state(git, "MERGE_MODE", mode)?;
            println!("Automatic merge went well; stopped before committing as requested");
            return Ok(());
        }
        let message = if args.signoff {
            let me = identity(git, "committer")?;
            let signed = super::commit::append_signoff(message.into_bytes(), &me.name, &me.email);
            String::from_utf8_lossy(&signed).into_owned()
        } else {
            message
        };
        let message = self.message_hooks(&message, mode)?;
        let tree = tree()?;
        let mut record = Commit {
            tree,
            parents: vec![*ours, *theirs],
            author: identity(git, "author")?,
            committer: identity(git, "committer")?,
            extra_headers: Vec::new(),
            message: message.clone().into_bytes(),
        };
        if let Err(e) = super::signing::sign_commit(git, &args.sign, &mut record) {
            // As git does, leave the merge to conclude with `pyn commit` once signing works.
            write_state(git, "MERGE_HEAD", &format!("{theirs}\n"))?;
            write_state(git, "MERGE_MSG", &message)?;
            write_state(git, "MERGE_MODE", mode)?;
            return Err(e);
        }
        let id = git
            .repo
            .objects()
            .write(ObjectKind::Commit, &record.encode(git.algorithm())?)?;
        let summary = match how {
            Concluding::Strategy(kind) => format!("Merge made by the '{}' strategy.", kind.name()),
            Concluding::InIndex => "In-index merge".to_string(),
        };
        git.move_head(&id, &format!("{}: {summary}", action(spec)))?;
        clear_merge_state(git);
        say(&summary);
        show_stat(git, args, Some(ours_tree), &tree)?;
        git.warn_if_hooks_off(&MERGE_HOOKS);
        git.run_hook("post-merge", &["0".to_string()], None, &[])?;
        Ok(())
    }
}

/// How a tree merge reaches the index and work tree.
pub(super) struct Landing<'a> {
    /// The label of the incoming side in conflict reports.
    pub theirs: &'a str,
    /// Saved as `ORIG_HEAD` once the merge is allowed to go on.
    pub orig_head: Option<ObjectId>,
    /// A merge needs the whole index to equal `HEAD`; a pick only the paths it touches.
    pub strict_index: bool,
    /// A clean merge prints nothing (a rebase); a conflicted one lists the line-merged paths.
    pub quiet_when_clean: bool,
    /// A file moved aside for a folder keeps its unmerged stages at its own path, as `am --3way` leaves it.
    pub stages_at_origin: bool,
}

pub(super) struct Applied {
    pub index: pyn_git::Index,
    pub conflicted: Vec<Vec<u8>>,
}

/// A merge that could not touch the work tree and index; nothing was changed.
pub(super) struct Refusal {
    pub blocked: Blocked,
    /// The index differed from `HEAD` before the merge.
    pub index_differs: bool,
    /// The refusal is the merge strategy's own check that the index equals `HEAD`, not the work tree check.
    pub index_check: bool,
}

/// Writes the merged paths to the work tree and index, leaving conflicts as unmerged entries, or refuses.
pub(super) fn apply_merge(
    git: &GitHome,
    opened: &super::stage::OpenIndex,
    ours_tree: Option<&ObjectId>,
    merged: &Merge,
    landing: &Landing,
) -> Result<std::result::Result<Applied, Refusal>> {
    super::sparse::refuse_unsupported(&opened.index)?;
    check_writes(merged.paths.keys())?;
    let head_leaves = git.repo.flatten(ours_tree, &Pathspec::default())?;

    let mut blocked = Blocked::default();
    let index_leaves = git.repo.index_leaves(&opened.index);
    let differing = index_changes(git, opened, ours_tree)?;
    let index_differs = !differing.is_empty();
    if landing.strict_index && index_differs {
        blocked.local = differing;
        return Ok(Err(Refusal {
            blocked,
            index_differs,
            index_check: true,
        }));
    }
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let ignores = super::stage::Ignores {
        ignore: git.repo.ignore()?,
    };
    let removed: std::collections::HashSet<&[u8]> = merged
        .paths
        .iter()
        .filter(|(p, r)| matches!(r, Resolved::Take(None)) && head_leaves.contains_key(*p))
        .map(|(p, _)| p.as_slice())
        .collect();
    for (path, resolved) in &merged.paths {
        let here = head_leaves.get(path);
        let touches_tree = match resolved {
            Resolved::Take(e) => e.as_ref() != here,
            Resolved::Conflict(c) => !matches!(c.worktree, Worktree::Keep),
        };
        if !touches_tree {
            continue;
        }
        if !landing.strict_index && index_leaves.get(path) != here {
            blocked.local.push(path.clone());
            continue;
        }
        match here {
            Some(_) if matches!(dirty.get(path), Some(Kind::Modified | Kind::TypeChange)) => {
                blocked.local.push(path.clone());
            }
            None => {
                if let Some(way) = blocking_path(git, &ignores, path, &removed)? {
                    blocked.add_way(git.repo.root()?, way);
                }
            }
            _ => {}
        }
    }
    let local = blocked.local.clone();
    for list in [&mut blocked.untracked, &mut blocked.directories] {
        list.sort();
        list.retain(|u| !local.iter().any(|l| within(l, u)));
    }
    if !blocked.local.is_empty() || !blocked.untracked.is_empty() || !blocked.directories.is_empty()
    {
        blocked.local.sort();
        blocked.local.dedup();
        return Ok(Err(Refusal {
            blocked,
            index_differs,
            index_check: false,
        }));
    }
    if let Some(orig) = &landing.orig_head {
        write_orig_head(git, orig)?;
    }
    let mut entries: Entries = entries_of(&opened.index);
    let mut conflicted: Vec<Vec<u8>> = Vec::new();
    let notes = !landing.quiet_when_clean
        || merged
            .paths
            .values()
            .any(|r| matches!(r, Resolved::Conflict(_)));
    for (path, resolved) in &merged.paths {
        if notes && merged.line_merged.contains(path) {
            say(&format!("Auto-merging {}", path_string(path)));
        }
        for line in merged.updates.get(path).into_iter().flatten() {
            println!("{line}");
        }
        match resolved {
            Resolved::Take(Some(entry)) => {
                if head_leaves.get(path) != Some(entry) {
                    put_entry(&mut entries, write_entry(git, path, entry)?);
                }
            }
            Resolved::Take(None) => {
                if head_leaves.contains_key(path) {
                    remove_entry_file(git, path)?;
                    remove_entry(&mut entries, path);
                }
            }
            Resolved::Conflict(c) => {
                conflicted.push(path.clone());
                for line in &c.notes {
                    println!("{line}");
                }
                if let Some(from) = &c.moved_from {
                    let (label, side) = (
                        path_string(from),
                        if c.ours.is_some() {
                            "HEAD"
                        } else {
                            landing.theirs
                        },
                    );
                    println!(
                        "CONFLICT (file/directory): directory in the way of {label} from {side}; moving it to {} instead.",
                        path_string(path)
                    );
                }
                report_conflict(
                    path,
                    c.kind,
                    landing.theirs,
                    c.ours.is_some() && c.theirs.is_some(),
                )?;
                if let Worktree::Write { mode, bytes } = &c.worktree {
                    write_bytes(git, path, *mode, bytes)?;
                }
                let at = match &c.moved_from {
                    Some(from) if landing.stages_at_origin => from,
                    _ => path,
                };
                remove_entry(&mut entries, path);
                for (stage, side) in [(1u8, &c.base), (2, &c.ours), (3, &c.theirs)] {
                    if let Some(e) = side {
                        let mut entry = IndexEntry::new(at.clone(), raw_mode(e.mode), e.id);
                        entry.stage = stage;
                        entries.insert((at.clone(), stage), entry);
                    }
                }
            }
        }
    }
    let index = index_of(opened.index.version, entries);
    git.save_index(opened, &index)?;
    Ok(Ok(Applied { index, conflicted }))
}

fn report_conflict(path: &[u8], kind: Conflicting, theirs: &str, both: bool) -> Result<()> {
    let name = path_string(path);
    match kind {
        Conflicting::Content | Conflicting::Binary if both => {
            if kind == Conflicting::Binary {
                println!("warning: Cannot merge binary files: {name} (HEAD vs. {theirs})");
            }
            println!("CONFLICT (content): Merge conflict in {name}");
        }
        Conflicting::AddAdd => println!("CONFLICT (add/add): Merge conflict in {name}"),
        Conflicting::ModifyDelete { ours_deleted } => {
            let (deleted, kept) = if ours_deleted {
                ("HEAD", theirs)
            } else {
                (theirs, "HEAD")
            };
            println!(
                "CONFLICT (modify/delete): {name} deleted in {deleted} and modified in {kept}.  Version {kept} of {name} left in tree."
            );
        }
        Conflicting::FileDirectory | Conflicting::Renamed => {}
        _ => println!("CONFLICT (content): Merge conflict in {name}"),
    }
    Ok(())
}

/// `pyn merge --abort`: puts back what the merge changed and keeps other local changes, as `git merge --abort`.
fn abort(git: &GitHome) -> Result<()> {
    if merge_heads(git)?.is_empty() {
        bail!("There is no merge to abort (MERGE_HEAD missing).");
    }
    let Some(head) = git.repo.head_commit()? else {
        bail!("could not resolve HEAD");
    };
    let tree = git.repo.commit(&head)?.tree;
    let committed: BTreeMap<Vec<u8>, Entry> =
        git.repo.flatten(Some(&tree), &Pathspec::default())?;
    let opened = git.open_index()?;
    super::sparse::refuse_unsupported(&opened.index)?;
    let mut entries: Entries = entries_of(&opened.index);
    let leaves = git.repo.index_leaves(&opened.index);
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let unmerged: std::collections::BTreeSet<Vec<u8>> = opened
        .index
        .entries
        .iter()
        .filter(|e| e.stage > 0)
        .map(|e| e.path.clone())
        .collect();
    let mut redo: Vec<Vec<u8>> = Vec::new();
    let mut stale: Vec<Vec<u8>> = Vec::new();
    let paths: std::collections::BTreeSet<&Vec<u8>> = committed
        .keys()
        .chain(leaves.keys())
        .chain(unmerged.iter())
        .collect();
    for path in paths {
        if unmerged.contains(path) {
            redo.push(path.clone());
        } else if committed.get(path) != leaves.get(path) {
            if matches!(dirty.get(path), Some(Kind::Modified | Kind::TypeChange)) {
                stale.push(path.clone());
            } else {
                redo.push(path.clone());
            }
        }
    }
    if !stale.is_empty() {
        let names: Vec<String> = stale.iter().map(|p| path_string(p)).collect();
        bail!(
            "Entry '{}' not uptodate. Cannot merge; could not reset the index to HEAD",
            names.join("', '")
        );
    }
    check_writes(&redo)?;
    convert::refuse_unsupported(
        git,
        redo.iter()
            .filter_map(|p| committed.get(p).map(|e| (p.as_slice(), e.mode))),
    )?;
    // Removals first, so a folder is gone before the file that replaces it is written.
    redo.sort_by_key(|p| committed.contains_key(p));
    for path in &redo {
        remove_entry(&mut entries, path);
        match committed.get(path) {
            Some(entry) => put_entry(&mut entries, write_entry(git, path, entry)?),
            None => remove_entry_file(git, path)?,
        }
    }
    git.save_index(&opened, &index_of(opened.index.version, entries))?;
    clear_merge_state(git);
    git.log_ref("HEAD", Some(&head), &head, "reset: moving to HEAD", false)?;
    options::apply_held_autostash(git);
    Ok(())
}

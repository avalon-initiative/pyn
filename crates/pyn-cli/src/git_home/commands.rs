//! The commands a git repository runs: `status`, `diff`, `log`, `show` and `init`, in git's output shapes.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use pyn_git::Refspec;
use pyn_gitwork::{
    Change, Conflict, Entry, Head, Kind, Pathspec, StatusEntry, SubmoduleIgnore, Submodules,
    fs_path,
};
use pyn_object::ObjectId;

use super::decorate::{DecorateArgs, with_decoration};
use super::merge_diff::{Against, Body, MergeArgs, Shape, Want, with_from};
use super::show_object::Item;
use super::{GitHome, path_string, sparse, stage};
use crate::backend::Backend;
use crate::commit_format::{Context, FormatArgs, Pretty, Rendering, render};
use crate::diff::{self, Blob, Source, Tree};
use crate::rename;
use crate::short_status::{self, Format, Row, StatusArgs};
use crate::{Command, table, unsupported};

const EXCLUDE_LINE: &str = ".pyn/";

/// Runs `command` on a git repository, or refuses it with the home that provides it.
pub fn run(command: Command, git: &GitHome, cwd: &Path) -> Result<()> {
    // Validate before writes; this repository caches the configured/default length.
    git.repo.abbreviation_len(None)?;
    let server_only = |feature: &str| -> Result<()> {
        match git.pyn_server(feature) {
            Ok(_) => unreachable!("a git home has no pyn server"),
            Err(e) => Err(e),
        }
    };
    if git.is_bare() && needs_work_tree(&command) {
        git.need_work_tree()?;
    }
    match command {
        Command::Status(args) => {
            if args.revision.is_some() {
                bail!(
                    "`pyn status --revision` compares with a pyn commit; a git repository has none"
                );
            }
            status(git, cwd, &args)
        }
        Command::Diff {
            args,
            paths,
            format,
            exit_code,
            quiet,
            cached,
            no_patch,
            ignore_submodules,
            ..
        } => {
            if let Some(when) = &ignore_submodules {
                git.repo.force_submodule_ignore(when)?;
            }
            let format = format.format().silenced(no_patch);
            let differs = diff(git, cwd, &args, &paths, &format, quiet, cached)?;
            if differs && (exit_code || quiet) {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Show {
            args,
            paths,
            no_patch,
            fmt,
            decorate,
            show_signature,
            format,
            first_parent,
            merges,
        } => show(
            git,
            cwd,
            (&args, &paths),
            (no_patch, first_parent, &merges, show_signature),
            (&fmt, &decorate),
            &format.format(),
        ),
        Command::Log {
            path,
            filter,
            limit,
            commits,
            git: args,
        } => {
            if filter.is_some() {
                bail!(
                    "`pyn log --filter` is not available on a git repository; name a path instead"
                );
            }
            if commits {
                if path.is_some()
                    || args.first_given().is_some()
                    || args.diff_given()
                    || args.format().first_given().is_some()
                {
                    bail!("`pyn log --commits` takes no revision, path or git log option");
                }
                return super::log::log_table(git, limit.unwrap_or(50));
            }
            super::log::log(git, cwd, path, limit, *args)
        }
        Command::Bundle(_) => unreachable!("handled before a repository is detected"),
        Command::Convert(_) => init(git),
        Command::Init(_) => unreachable!("handled before a repository is detected"),
        Command::InitGit(_) => unreachable!("handled before a repository is detected"),
        Command::Files => crate::files_command(git),
        Command::Ls { path } => crate::ls_command(git, path.as_deref()),
        Command::Merge(args) => {
            super::merge::merge(git, args)?;
            super::gc::auto(git);
            Ok(())
        }
        Command::Branch(args) => super::branches::branch(git, args),
        Command::Switch(args) => super::switch::switch(git, args),
        Command::Checkout(args) => super::switch::checkout(git, cwd, args),
        Command::Tag(args) => super::tags::tag(git, args),
        Command::Stash(args) => super::stash::stash(git, cwd, args),
        Command::Worktree(args) => super::worktrees::worktree(git, cwd, args),
        Command::Submodule(args) => super::submodule::submodule(git, cwd, args),
        Command::Clean(args) => {
            if super::clean::clean(git, cwd, &args)? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Grep(_) => {
            if !super::grep::grep(git, cwd)? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Bisect(args) => super::bisect::bisect(git, cwd, args),
        Command::Blame(args) => super::blame::blame(git, cwd, args, false),
        Command::Annotate(args) => super::blame::blame(git, cwd, args, true),
        Command::Reflog(args) => super::reflog::reflog(git, args),
        Command::RevParse(args) => super::rev_parse::rev_parse(git, cwd, args),
        Command::LsFiles(args) => super::ls_files::ls_files(git, cwd, args),
        Command::LsTree(args) => super::ls_tree::ls_tree(git, cwd, args),
        Command::CatFile(args) => super::cat_file::cat_file(git, cwd, args),
        Command::VerifyCommit(args) => super::verify_sig::verify_commit(git, cwd, args),
        Command::VerifyTag(args) => super::verify_sig::verify_tag(git, cwd, args),
        Command::ShowRef(args) => super::show_ref::show_ref(git, args),
        Command::SparseCheckout(args) => super::sparse::sparse_checkout(git, args),
        Command::CountObjects(args) => super::count_objects::count_objects(git, args),
        Command::Fsck(args) => super::fsck::fsck(git, args),
        Command::Prune(args) => super::prune::prune(git, args),
        Command::Gc(args) => super::gc::gc(git, args),
        Command::PackRefs(args) => super::pack_refs::pack_refs(git, args),
        Command::PrunePacked(args) => super::pack_plumbing::prune_packed(git, args),
        Command::PackObjects(args) => super::pack_plumbing::pack_objects(git, args),
        Command::UnpackObjects(args) => super::pack_plumbing::unpack_objects(git, args),
        Command::Maintenance(args) => super::maintenance::maintenance(git, args),
        Command::Repack(args) => super::repack::repack(git, args),
        Command::Shortlog(args) => super::shortlog::shortlog(git, cwd, args),
        Command::CheckMailmap(args) => super::mailmap::check_mailmap(git, args),
        Command::Describe(args) => super::describe::describe(git, cwd, args),
        Command::NameRev(args) => super::name_rev::name_rev(git, cwd, args),
        Command::Am(args) => {
            super::am::am(git, cwd, args)?;
            super::gc::auto(git);
            Ok(())
        }
        Command::Apply(args) => super::apply::apply(git, cwd, args),
        Command::FormatPatch(args) => super::format_patch::format_patch(git, cwd, args),
        Command::Rebase(args) => {
            super::rebase::rebase(git, &args)?;
            super::gc::auto(git);
            Ok(())
        }
        Command::Remote(args) => super::remote::remote(git, args),
        Command::Fetch(args) => super::fetch::fetch(git, args),
        Command::Pull(args) => {
            super::pull::pull(git, args)?;
            super::gc::auto(git);
            Ok(())
        }
        Command::Push(args) => super::push::push(git, args),
        Command::Add {
            dry_run: true,
            patch: true,
            ..
        }
        | Command::Add {
            dry_run: true,
            interactive: true,
            ..
        } => bail!("options '--dry-run' and '--interactive/--patch' cannot be used together"),
        Command::Add {
            paths, patch: true, ..
        } => super::patch_mode::add(git, cwd, &paths),
        Command::Add {
            paths,
            interactive: true,
            ..
        } => super::add_interactive::add_interactive(git, cwd, &paths),
        Command::Add {
            paths,
            update,
            dry_run,
            verbose,
            force,
            intent_to_add,
            ..
        } => {
            let options = stage::AddOptions {
                paths,
                update,
                dry_run,
                verbose,
                force,
                intent_to_add,
            };
            if stage::add(git, cwd, &options)? {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Rm(args) => super::remove::rm(git, cwd, &args),
        Command::Mv(args) => super::remove::mv(git, cwd, &args),
        Command::Commit {
            paths,
            file,
            base,
            message,
            all,
            git: flags,
        } => {
            if !paths.is_empty() {
                bail!(
                    "`pyn commit <path>` is not available on a git repository; \
                     stage the paths with `pyn add` and run `pyn commit`"
                );
            }
            if base.is_some() {
                bail!("`pyn commit --base` needs a pyn server");
            }
            super::commit::commit(
                git,
                &super::commit::CommitOptions {
                    messages: message,
                    all,
                    file,
                    flags,
                },
            )?;
            super::gc::auto(git);
            Ok(())
        }
        Command::Restore {
            args,
            source,
            staged,
            worktree,
            patch: true,
            ..
        } => super::patch_mode::restore(git, cwd, &args, source.as_deref(), staged, worktree),
        Command::Restore {
            args,
            source,
            staged,
            ours,
            theirs,
            merge,
            ignore_skip_worktree_bits,
            ..
        } if ours || theirs || merge => super::switch::restore_sides(
            git,
            cwd,
            &args,
            super::switch::SideFlags {
                source: source.as_deref(),
                staged,
                stage: if ours {
                    Some(2)
                } else if theirs {
                    Some(3)
                } else {
                    None
                },
                merge,
                ignore_skip: ignore_skip_worktree_bits,
            },
        ),
        Command::Restore {
            args,
            source,
            staged: true,
            worktree: false,
            ..
        } => {
            stage::restore_staged(git, cwd, &args, source.as_deref())?;
            super::switch::post_checkout_files(git)
        }
        Command::Restore {
            args,
            source,
            staged,
            worktree,
            ignore_skip_worktree_bits,
            ..
        } => super::switch::restore(
            git,
            cwd,
            &args,
            source.as_deref(),
            staged,
            worktree,
            ignore_skip_worktree_bits,
        ),
        Command::Reset(args) => super::reset::reset(git, cwd, &args),
        Command::CherryPick(args) => super::pick::cherry_pick(git, &args),
        Command::Revert(args) => super::pick::revert(git, &args.into()),
        Command::Migrate => {
            bail!("`pyn migrate` converts a pyn server workspace; this is a git repository")
        }
        Command::Update { .. } => refuse("update"),
        Command::Get { .. } => refuse("get"),
        Command::Locks { .. } => server_only("locks"),
        Command::Lock { .. } => server_only("lock"),
        Command::Unlock { .. } => server_only("unlock"),
        Command::Policy(_) => server_only("policy"),
        Command::Audit { .. } => server_only("audit"),
        Command::Verify { .. } => server_only("verify"),
        Command::Summary => server_only("summary"),
        Command::Login { .. }
        | Command::Logout
        | Command::Register { .. }
        | Command::Password { .. }
        | Command::Key(_)
        | Command::Invite(_)
        | Command::User(_)
        | Command::Whoami
        | Command::Repo(_)
        | Command::Org(_)
        | Command::Team(_)
        | Command::Setup { .. }
        | Command::Admin(_)
        | Command::Usage { .. }
        | Command::Token(_)
        | Command::Member(_)
        | Command::Role(_)
        | Command::LsRemote(_) => unreachable!("run from anywhere, not through a git home"),
        Command::Clone { .. } | Command::Config(_) | Command::External(_) => {
            unreachable!("handled before a git home is consulted")
        }
    }
}

/// The commands git runs only with a work tree; it checks before reading their options.
fn needs_work_tree(command: &Command) -> bool {
    match command {
        Command::Rm(args) => !args.cached,
        Command::Status(_)
        | Command::Merge(_)
        | Command::Switch(_)
        | Command::Checkout(_)
        | Command::Stash(_)
        | Command::Submodule(_)
        | Command::Clean(_)
        | Command::Add { .. }
        | Command::Mv(_)
        | Command::Commit { .. }
        | Command::Restore { .. }
        | Command::CherryPick(_)
        | Command::Revert(_)
        | Command::Rebase(_)
        | Command::Pull(_)
        | Command::SparseCheckout(_)
        | Command::Am(_) => true,
        _ => false,
    }
}

fn refuse(name: &str) -> ! {
    eprintln!("{}", unsupported::message_on_git(name));
    std::process::exit(1);
}

/// Writes to standard output, ending quietly when the reader has gone (`pyn log | head`).
pub(super) fn emit(text: &str) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(text.as_bytes()).and_then(|()| out.flush())
        && e.kind() == std::io::ErrorKind::BrokenPipe
    {
        std::process::exit(0);
    }
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Added => "added",
        Kind::Modified => "modified",
        Kind::Deleted => "deleted",
        Kind::TypeChange => "typechange",
        Kind::Untracked => "untracked",
        Kind::Unmerged => "unmerged",
    }
}

pub(super) fn status(git: &GitHome, cwd: &Path, args: &StatusArgs) -> Result<()> {
    if let Some(when) = &args.ignore_submodules {
        git.repo.force_submodule_ignore(when)?;
    }
    git.repo.validate_diff_ignore_submodules()?;
    let specs = stage::resolve_specs(git, cwd, &args.paths)?;
    let mut status = git.repo.status_in(&specs.spec)?;
    status.entries.retain(|e| specs.spec.matches(&e.path));
    let format = args.format();
    if format != Format::Table {
        let branch = args
            .branch
            .then(|| branch_line(git, &status.head))
            .transpose()?;
        let prefix = git.ws.repo_prefix(cwd, ".")?;
        let quote_high = git.repo.bool_setting("core.quotePath", true);
        let moves = status_moves(git, &status, args)?;
        let rows = short_rows(&status.entries, format, &moves);
        return short_status::print(
            &rows,
            branch.as_deref(),
            format,
            prefix.as_bytes(),
            quote_high,
        );
    }
    let sparse = sparse::percentage(git, &git.open_index()?.index)
        .map(|p| format!("You are in a sparse checkout with {p}% of tracked files present."));
    status_table_with(git, &status, sparse.as_deref())
}

pub(super) fn status_table(git: &GitHome, status: &pyn_gitwork::Status) -> Result<()> {
    status_table_with(git, status, None)
}

fn status_table_with(
    git: &GitHome,
    status: &pyn_gitwork::Status,
    note: Option<&str>,
) -> Result<()> {
    let line = match &status.head {
        Head::Branch { name, id: Some(_) } => format!("On branch {name}"),
        Head::Branch { name, id: None } => format!("On branch {name} (no commits yet)"),
        Head::Detached(id) => format!("HEAD detached at {}", git.repo.abbreviate(id)?),
    };
    println!("{line}");
    if let Some(note) = note {
        println!("{note}");
    }
    if status.entries.is_empty() {
        println!("nothing to commit, working tree clean");
        return Ok(());
    }
    let word = |k: Option<Kind>| k.map_or("-", kind_word).to_string();
    let rows: Vec<Vec<String>> = status
        .entries
        .iter()
        .map(|e| vec![word(e.staged), word(e.unstaged), path_string(&e.path)])
        .collect();
    println!("{}", table::render(&["STAGED", "UNSTAGED", "PATH"], &rows));
    let count = |f: &dyn Fn(&pyn_gitwork::StatusEntry) -> bool| {
        status.entries.iter().filter(|e| f(e)).count()
    };
    let staged = count(&|e| e.staged.is_some());
    let modified = count(&|e| {
        e.unstaged
            .is_some_and(|k| !matches!(k, Kind::Untracked | Kind::Unmerged))
    });
    let untracked = count(&|e| e.unstaged == Some(Kind::Untracked));
    let unmerged = count(&|e| e.unstaged == Some(Kind::Unmerged));
    let conflicts = if unmerged > 0 {
        format!(", {unmerged} unmerged")
    } else {
        String::new()
    };
    println!("{staged} staged, {modified} modified, {untracked} untracked{conflicts}");
    Ok(())
}

fn move_letter(m: &rename::Moved) -> char {
    if m.copy { 'C' } else { 'R' }
}

fn letter(kind: Option<Kind>) -> char {
    match kind {
        Some(Kind::Added) => 'A',
        Some(Kind::Modified) => 'M',
        Some(Kind::Deleted) => 'D',
        Some(Kind::TypeChange) => 'T',
        _ => ' ',
    }
}

/// git's two-letter code for an unmerged path, from the stages the index holds.
fn conflict_code(c: Conflict) -> (char, char) {
    match (c.base, c.ours, c.theirs) {
        (true, true, true) => ('U', 'U'),
        (false, true, true) => ('A', 'A'),
        (true, false, true) => ('D', 'U'),
        (true, true, false) => ('U', 'D'),
        (false, false, true) => ('U', 'A'),
        (false, true, false) => ('A', 'U'),
        _ => ('D', 'D'),
    }
}

/// The renames (and copies, with `status.renames=copies`) among the staged changes and among the work tree's.
struct Moves {
    staged: rename::Detected,
    unstaged: rename::Detected,
}

fn status_moves(git: &GitHome, status: &pyn_gitwork::Status, args: &StatusArgs) -> Result<Moves> {
    let Some(opts) = args.rename_options(git.rename_settings("status")) else {
        return Ok(Moves {
            staged: rename::Detected::default(),
            unstaged: rename::Detected::default(),
        });
    };
    let repo = &git.repo;
    let added = |k: Option<Kind>| k == Some(Kind::Added);
    let (mut old, mut new) = (Tree::new(), Tree::new());
    let (mut old_wt, mut new_wt) = (Tree::new(), Tree::new());
    if !status
        .entries
        .iter()
        .any(|e| added(e.staged) || added(e.unstaged))
    {
        return Ok(Moves {
            staged: rename::Detected::default(),
            unstaged: rename::Detected::default(),
        });
    }
    let tree = match status.head.id() {
        Some(id) => Some(repo.commit(&id)?.tree),
        None => None,
    };
    let committed = repo.flatten(tree.as_ref(), &Pathspec::default())?;
    let staged = repo.index_leaves(&repo.read_index()?);
    for e in status.entries.iter().filter(|e| e.staged.is_some()) {
        let name = path_string(&e.path);
        if let Some(b) = committed.get(&e.path).and_then(entry_blob) {
            old.insert(name.clone(), b);
        }
        if let Some(b) = staged.get(&e.path).and_then(entry_blob) {
            new.insert(name, b);
        }
    }
    if status.entries.iter().any(|e| added(e.unstaged)) {
        let modules = repo.submodules();
        let worked =
            |k: Option<Kind>| matches!(k, Some(Kind::Added | Kind::Deleted | Kind::Modified));
        for e in status.entries.iter().filter(|e| worked(e.unstaged)) {
            let name = path_string(&e.path);
            if let Some(b) = staged.get(&e.path).and_then(entry_blob) {
                old_wt.insert(name.clone(), b);
            }
            if let Some(b) = disk_blob(git, &modules, &e.path, staged.get(&e.path))? {
                new_wt.insert(name, b);
            }
        }
    }
    Ok(Moves {
        staged: rename::detect(git, &old, &new, &opts)?,
        unstaged: rename::detect(git, &old_wt, &new_wt, &opts)?,
    })
}

fn short_rows(entries: &[StatusEntry], format: Format, moves: &Moves) -> Vec<Row> {
    let mut rows = Vec::new();
    for e in entries {
        let mut e = e.clone();
        let name = path_string(&e.path);
        if moves.staged.consumed.contains(&name) {
            e.staged = None;
        }
        if moves.unstaged.consumed.contains(&name) {
            e.unstaged = None;
        }
        if e.staged.is_none() && e.unstaged.is_none() && e.conflict.is_none() {
            continue;
        }
        let (staged_move, unstaged_move) = (
            moves.staged.moved.get(&name),
            moves.unstaged.moved.get(&name),
        );
        if let Some(c) = e.conflict {
            let (x, y) = conflict_code(c);
            rows.push(Row {
                x,
                y,
                path: e.path.clone(),
                from: None,
            });
        } else if e.unstaged == Some(Kind::Untracked) {
            if e.staged.is_some() {
                rows.push(Row {
                    x: letter(e.staged),
                    y: ' ',
                    path: e.path.clone(),
                    from: None,
                });
            }
            rows.push(Row::untracked(e.path.clone()));
        } else {
            rows.push(Row {
                x: match staged_move {
                    Some(m) => move_letter(m),
                    None => letter(e.staged),
                },
                y: match e.submodule {
                    Some(s) if format == Format::Short && !s.new_commits => {
                        if s.modified {
                            'm'
                        } else {
                            '?'
                        }
                    }
                    _ => unstaged_move.map_or_else(|| letter(e.unstaged), move_letter),
                },
                path: e.path.clone(),
                from: staged_move
                    .or(unstaged_move)
                    .map(|m| m.src.clone().into_bytes()),
            });
        }
    }
    rows
}

/// The ref a branch follows, shortened, and the commit it is at (`None` when the ref is gone).
pub(super) struct Upstream {
    pub shown: String,
    pub id: Option<ObjectId>,
}

pub(super) fn upstream_of(git: &GitHome, name: &str) -> Result<Option<Upstream>> {
    let config = git.config();
    let tracking = pyn_git::tracking(config, name);
    let (Some(remote), Some(merge)) = (tracking.remote, tracking.merge.first()) else {
        return Ok(None);
    };
    let upstream = if remote == "." {
        Some(merge.clone())
    } else {
        config
            .get_all(&format!("remote.{remote}.fetch"))
            .into_iter()
            .filter_map(|s| Refspec::parse(s).ok())
            .find_map(|s| s.map(merge))
    };
    let Some(upstream) = upstream else {
        return Ok(None);
    };
    let shown = upstream
        .strip_prefix("refs/remotes/")
        .or_else(|| upstream.strip_prefix("refs/heads/"))
        .unwrap_or(&upstream)
        .to_string();
    let id = git.repo.refs().resolve(&upstream)?;
    Ok(Some(Upstream { shown, id }))
}

/// The commits only `local` has and only `upstream` has.
pub(super) fn divergence(
    git: &GitHome,
    local: &ObjectId,
    upstream: &ObjectId,
) -> Result<(usize, usize)> {
    let (mine, theirs) = (git.reachable(local)?, git.reachable(upstream)?);
    Ok((
        mine.difference(&theirs).count(),
        theirs.difference(&mine).count(),
    ))
}

/// `ahead N`, `behind M`, both, or nothing when in step.
pub(super) fn track_words(ahead: usize, behind: usize) -> Option<String> {
    match (ahead, behind) {
        (0, 0) => None,
        (a, 0) => Some(format!("ahead {a}")),
        (0, b) => Some(format!("behind {b}")),
        (a, b) => Some(format!("ahead {a}, behind {b}")),
    }
}

/// `<branch>...<upstream> [ahead N, behind M]`, as the branch line of the short formats.
fn branch_line(git: &GitHome, head: &Head) -> Result<String> {
    let (name, local) = match head {
        Head::Detached(_) => return Ok("HEAD (no branch)".into()),
        Head::Branch { name, id } => (name, *id),
    };
    let mut line = match local {
        Some(_) => name.clone(),
        None => format!("No commits yet on {name}"),
    };
    let Some(up) = upstream_of(git, name)? else {
        return Ok(line);
    };
    line.push_str(&format!("...{}", up.shown));
    let Some(up_id) = up.id else {
        line.push_str(" [gone]");
        return Ok(line);
    };
    if let Some(local) = local {
        let (ahead, behind) = divergence(git, &local, &up_id)?;
        if let Some(words) = track_words(ahead, behind) {
            line.push_str(&format!(" [{words}]"));
        }
    }
    Ok(line)
}

fn entry_blob(e: &Entry) -> Option<Blob> {
    if e.mode == pyn_object::FileMode::Gitlink {
        return Some(Blob::gitlink(e.id.to_hex(), false));
    }
    Some(Blob {
        hash: e.id.to_hex(),
        mode: e.mode,
        source: Source::Object { id: e.id.to_hex() },
    })
}

fn disk_blob(
    git: &GitHome,
    modules: &Submodules<'_>,
    path: &[u8],
    recorded: Option<&Entry>,
) -> Result<Option<Blob>> {
    let file = fs_path(git.repo.root()?, path);
    let Ok(meta) = std::fs::symlink_metadata(&file) else {
        return Ok(None);
    };
    if meta.is_dir()
        && let Some(e) = recorded.filter(|e| e.mode == pyn_object::FileMode::Gitlink)
    {
        return Ok(Some(
            match modules.status(path, e.id, SubmoduleIgnore::Untracked)? {
                Some(s) => Blob::gitlink(s.head.to_hex(), s.modified || s.untracked),
                None => Blob::gitlink(e.id.to_hex(), false),
            },
        ));
    }
    if !meta.is_file() && !meta.file_type().is_symlink() {
        return Ok(None);
    }
    let recorded_id = recorded.map(|e| e.id);
    let (mode, id, cleaned) = if meta.is_file() && !git.repo.conversion(path)?.is_identity() {
        let data = git.repo.read_clean(path, &file, recorded_id, false)?.data;
        let id = pyn_object::hash_object(git.repo.algorithm(), pyn_object::Kind::Blob, &data)?;
        let mode = pyn_gitwork::worktree_mode(&meta).unwrap_or(pyn_object::FileMode::Regular);
        (mode, id, Some(std::sync::Arc::new(data.into_owned())))
    } else {
        let (mode, id) = git.repo.hash_worktree_entry(path, &file, recorded_id)?;
        (mode, id, None)
    };
    Ok(Some(Blob {
        hash: id.to_hex(),
        mode,
        source: Source::Worktree { file, cleaned },
    }))
}

pub(super) fn add_changes(
    git: &GitHome,
    old: &mut Tree,
    new: &mut Tree,
    changes: &[Change],
) -> Result<()> {
    add_changes_with_submodules(&git.repo.submodules(), old, new, changes)
}

/// Adds the files two trees share unchanged to both sides: `-C -C` takes them as copy sources.
pub(super) fn add_unchanged(
    git: &GitHome,
    (old, new): (&mut Tree, &mut Tree),
    (old_tree, new_tree): (Option<&ObjectId>, Option<&ObjectId>),
    spec: &Pathspec,
) -> Result<()> {
    let (a, b) = (
        git.repo.flatten(old_tree, spec)?,
        git.repo.flatten(new_tree, spec)?,
    );
    for (path, x) in &a {
        let name = path_string(path);
        if b.get(path)
            .is_some_and(|y| x.id == y.id && x.mode == y.mode)
            && !old.contains_key(&name)
            && !new.contains_key(&name)
            && let Some(blob) = entry_blob(x)
        {
            old.insert(name.clone(), blob.clone());
            new.insert(name, blob);
        }
    }
    Ok(())
}

/// Whether the command line asks for `-C -C`.
pub(super) fn wants_harder(git: &GitHome, format: &diff::Format) -> bool {
    format.renames.is_harder(git.rename_defaults())
}

fn add_changes_with_submodules(
    modules: &Submodules<'_>,
    old: &mut Tree,
    new: &mut Tree,
    changes: &[Change],
) -> Result<()> {
    for c in changes {
        let path = path_string(&c.path);
        if let Some(b) = c.old.as_ref().and_then(entry_blob) {
            old.insert(path.clone(), b);
        }
        if let Some(b) = c.new.as_ref().and_then(entry_blob) {
            new.insert(path, b);
        }
    }
    filter_gitlinks(modules, old, new)
}

fn filter_gitlinks(modules: &Submodules<'_>, old: &mut Tree, new: &mut Tree) -> Result<()> {
    let mut ignored = Vec::new();
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for path in paths {
        if old
            .get(path)
            .into_iter()
            .chain(new.get(path))
            .all(Blob::is_gitlink)
            && modules.ignore(path.as_bytes(), SubmoduleIgnore::Untracked)? == SubmoduleIgnore::All
        {
            ignored.push(path.clone());
        }
    }
    for path in ignored {
        old.remove(&path);
        new.remove(&path);
    }
    Ok(())
}

/// Up to `max` revisions (an `a..b` pair when `max` is 2), then paths; a name that is both a revision and a file is refused.
fn split_revisions(
    git: &GitHome,
    cwd: &Path,
    args: &[String],
    separated: bool,
    max: usize,
) -> Result<(Vec<ObjectId>, Vec<String>)> {
    let mut revs: Vec<ObjectId> = Vec::new();
    let mut rest = args;
    while revs.len() < max
        && let Some((first, tail)) = rest.split_first()
    {
        if let Some((a, b)) = first.split_once("..")
            && max == 2
            && !b.starts_with('.')
            && revs.is_empty()
        {
            let side = |s: &str| git.repo.resolve(if s.is_empty() { "HEAD" } else { s });
            if let (Ok(a), Ok(b)) = (side(a), side(b)) {
                revs.extend([a, b]);
                rest = tail;
                continue;
            }
        }
        let Ok(id) = git.repo.resolve(first) else {
            break;
        };
        if !separated && git.cwd_has(cwd, first) {
            bail!(
                "ambiguous argument '{first}': both revision and filename; \
                 use '--' to separate paths from revisions"
            );
        }
        revs.push(id);
        rest = tail;
    }
    Ok((revs, rest.to_vec()))
}

/// Arguments left after the revisions must be existing paths, or follow `--`; git refuses the rest.
fn check_loose(git: &GitHome, cwd: &Path, loose: &[String], separated: bool) -> Result<()> {
    if separated {
        if let Some(bad) = loose.first() {
            bail!("bad revision '{bad}'");
        }
        return Ok(());
    }
    for (i, arg) in loose.iter().enumerate() {
        if stage::looks_like_pathspec(arg) || git.cwd_has(cwd, arg) {
            continue;
        }
        if i > 0 {
            bail!("{arg}: no such path in the working tree.");
        }
        bail!(
            "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
             Use '--' to separate paths from revisions, like this:\n\
             'git <command> [<revision>...] -- [<file>...]'"
        );
    }
    Ok(())
}

fn diff(
    git: &GitHome,
    cwd: &Path,
    args: &[String],
    paths: &[String],
    format: &diff::Format,
    quiet: bool,
    cached: bool,
) -> Result<bool> {
    let separated = !paths.is_empty() || std::env::args_os().any(|a| a == "--");
    let (revs, loose) = split_revisions(git, cwd, args, separated, 2)?;
    check_loose(git, cwd, &loose, separated)?;
    if !cached && revs.len() < 2 {
        git.need_work_tree()?;
    }
    let spec = git.pathspec(cwd, &[loose.as_slice(), paths].concat(), false)?;
    let repo = &git.repo;
    repo.validate_diff_ignore_submodules()?;
    let modules = repo.submodules();
    let (mut old, mut new) = (Tree::new(), Tree::new());
    match revs.as_slice() {
        [] | [_] if cached => {
            let tree = match revs.first() {
                Some(id) => Some(repo.commit(id)?.tree),
                None => match repo.head_commit()? {
                    Some(head) => Some(repo.commit(&head)?.tree),
                    None => None,
                },
            };
            let committed = repo.flatten(tree.as_ref(), &spec)?;
            let index = repo.read_index()?;
            let mut staged = repo.index_leaves(&index);
            staged.retain(|p, _| spec.matches(p));
            for (path, e) in &committed {
                if let Some(b) = entry_blob(e) {
                    old.insert(path_string(path), b);
                }
            }
            for (path, e) in &staged {
                if let Some(b) = entry_blob(e) {
                    new.insert(path_string(path), b);
                }
            }
        }
        [_, _] if cached => bail!("`pyn diff --cached` takes at most one commit"),
        [] => {
            let index = repo.read_index()?;
            let leaves = repo.index_leaves(&index);
            for (path, kind) in repo.unstaged_changes_with_submodules(&index, &modules)? {
                if !spec.matches(&path)
                    || !matches!(
                        kind,
                        Kind::Added | Kind::Modified | Kind::Deleted | Kind::TypeChange
                    )
                {
                    continue;
                }
                let name = path_string(&path);
                if let Some(b) = leaves.get(&path).and_then(entry_blob) {
                    old.insert(name.clone(), b);
                }
                if let Some(b) = disk_blob(git, &modules, &path, leaves.get(&path))? {
                    new.insert(name, b);
                }
            }
            if wants_harder(git, format) {
                let skipped: BTreeSet<&Vec<u8>> = index
                    .entries
                    .iter()
                    .filter(|e| e.skip_worktree)
                    .map(|e| &e.path)
                    .collect();
                for (path, e) in leaves.iter().filter(|(p, _)| spec.matches(p)) {
                    let name = path_string(path);
                    if let (false, false, Some(b)) = (
                        skipped.contains(path) || old.contains_key(&name),
                        new.contains_key(&name),
                        entry_blob(e),
                    ) {
                        old.insert(name.clone(), b.clone());
                        new.insert(name, b);
                    }
                }
            }
        }
        [one] => {
            let tree = repo.commit(one)?.tree;
            let committed = repo.flatten(Some(&tree), &spec)?;
            let index = repo.read_index()?;
            let leaves = repo.index_leaves(&index);
            let unstaged = repo.unstaged_changes_with_submodules(&index, &modules)?;
            let intent: Vec<&Vec<u8>> = index
                .entries
                .iter()
                .filter(|e| e.intent_to_add)
                .map(|e| &e.path)
                .collect();
            let paths: BTreeSet<&Vec<u8>> = committed
                .keys()
                .chain(leaves.keys())
                .chain(intent)
                .collect();
            for path in paths.into_iter().filter(|p| spec.matches(p)) {
                let name = path_string(path);
                if let Some(b) = committed.get(path).and_then(entry_blob) {
                    old.insert(name.clone(), b);
                }
                let current = match (leaves.get(path), unstaged.contains_key(path)) {
                    (Some(e), false) => entry_blob(e),
                    _ => disk_blob(git, &modules, path, leaves.get(path))?,
                };
                if let Some(b) = current {
                    new.insert(name, b);
                }
            }
        }
        [a, b] => {
            let (ta, tb) = (repo.commit(a)?.tree, repo.commit(b)?.tree);
            add_changes_with_submodules(
                &modules,
                &mut old,
                &mut new,
                &repo.diff_trees(Some(&ta), Some(&tb), &spec)?,
            )?;
            if wants_harder(git, format) {
                add_unchanged(git, (&mut old, &mut new), (Some(&ta), Some(&tb)), &spec)?;
            }
        }
        _ => unreachable!("at most two revisions are taken"),
    }
    filter_gitlinks(&modules, &mut old, &mut new)?;
    let mut out = String::new();
    let differs = diff::compare(git, &old, &new, &[], format, &mut out)?;
    if !quiet {
        emit(&out);
        diff::exit_if_check_failed(format, &out);
    }
    Ok(differs)
}

pub(super) fn message_lines(message: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(message);
    let mut lines: Vec<&str> = text.split('\n').collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let skip = lines.iter().take_while(|l| l.trim().is_empty()).count();
    lines[skip..]
        .iter()
        .map(|l| format!("    {}", l.trim_end()))
        .collect()
}

pub(super) fn head_or_refuse(git: &GitHome) -> Result<ObjectId> {
    match git.repo.head()? {
        Head::Branch { name, id: None } => {
            bail!("your current branch '{name}' does not have any commits yet")
        }
        head => Ok(head
            .id()
            .expect("a head with no commit is the unborn branch")),
    }
}

/// The `tag`, tagger and message lines `show` prints for an annotated tag, by format.
fn tag_header(pretty: &Pretty, ctx: &Context<'_>, tag: &pyn_object::Tag) -> String {
    let mut out = ctx.paint(&format!("tag {}", String::from_utf8_lossy(&tag.name)));
    out.push('\n');
    if let Some(who) = &tag.tagger {
        let name = format!(
            "{} <{}>",
            String::from_utf8_lossy(&who.name),
            String::from_utf8_lossy(&who.email)
        );
        let date = ctx.style.format(who, ctx.now);
        match pretty {
            Pretty::Oneline { .. } => {}
            Pretty::Medium => out.push_str(&format!("Tagger: {name}\nDate:   {date}\n")),
            Pretty::Fuller => out.push_str(&format!("Tagger:     {name}\nTaggerDate: {date}\n")),
            _ => out.push_str(&format!("Tagger: {name}\n")),
        }
    }
    out.push('\n');
    out.push_str(&String::from_utf8_lossy(&tag.message));
    out
}

fn show(
    git: &GitHome,
    cwd: &Path,
    (args, paths): (&[String], &[String]),
    (no_patch, first_parent, merges, signatures): (bool, bool, &MergeArgs, bool),
    (fmt, decorate): (&FormatArgs, &DecorateArgs),
    format: &diff::Format,
) -> Result<()> {
    git.repo.validate_diff_ignore_submodules()?;
    let separated = !paths.is_empty() || std::env::args_os().any(|a| a == "--");
    let items = if paths.is_empty() {
        super::show_object::items(git, cwd, args, separated)?
    } else {
        None
    };
    if let Some(items) = &items
        && items
            .iter()
            .all(|i| !matches!(i, Item::Tag(_) | Item::Commit(_)))
    {
        for item in items {
            match item {
                Item::Blob(data) => super::plumbing::emit_bytes(data),
                Item::Tree { name, id } => {
                    let entries = super::show_object::tree_entries(git, id)?;
                    emit(&format!("tree {name}\n\n{entries}"));
                }
                Item::Tag(_) | Item::Commit(_) => unreachable!("filtered above"),
            }
        }
        return Ok(());
    }
    let rendering = fmt.rendering()?;
    let (tabs, abbrev_commit) = (rendering.tab_width(), rendering.abbrev_commit);
    let Rendering { pretty, style, .. } = rendering;
    if signatures {
        super::verify_sig::check_format(&pretty);
    }
    let decorate = decorate.resolve(git, &pretty)?;
    let plan = merges.plan(git, first_parent, true)?;
    let commit_color = diff::commit_color(git, &format.opts, decorate.on(), false);
    if signatures
        && items
            .as_ref()
            .is_some_and(|v| v.iter().any(|i| matches!(i, Item::Tag(_))))
    {
        bail!("`pyn show --show-signature` on a tag is not built");
    }
    let (seq, loose) = match items {
        Some(items) => (items, Vec::new()),
        None => {
            let (mut ids, loose) = split_revisions(git, cwd, args, separated, usize::MAX)?;
            check_loose(git, cwd, &loose, separated)?;
            if ids.is_empty() {
                ids.push(head_or_refuse(git)?);
            }
            (ids.into_iter().map(Item::Commit).collect(), loose)
        }
    };
    let spec = git.pathspec(cwd, &[loose.as_slice(), paths].concat(), false)?;
    let names = decorate.names(git, pretty.uses_decorations())?;
    let mailmap = git.mailmap()?;
    // `--no-abbrev` shows whole commit ids; the patch keeps `format.opts.abbrev`.
    let width = if format.opts.no_abbrev {
        Some(usize::MAX)
    } else {
        format.opts.abbrev
    };
    let abbreviations = crate::commit_format::IdFormat::Git(git.repo.abbreviations(width)?);
    let ctx = Context {
        style,
        now: chrono::Utc::now().timestamp(),
        decorations: &names,
        commit_color: commit_color.as_deref(),
        tabs,
        abbrev_commit,
        mailmap: Some(&mailmap),
        map_headers: fmt.mapped_headers(git.repo.bool_setting("log.mailmap", true)),
        abbreviations: &abbreviations,
    };
    let shape = Shape {
        git,
        pretty: &pretty,
        spec: &spec,
        only: &[],
        quiet: no_patch,
    };
    let mut out = String::new();
    let mut check = String::new();
    let mut shown = false;
    let mut seen = std::collections::HashSet::new();
    for item in &seq {
        let id = match item {
            Item::Commit(id) if !seen.insert(*id) => continue,
            Item::Commit(id) => id,
            Item::Blob(data) => {
                emit(&std::mem::take(&mut out));
                super::plumbing::emit_bytes(data);
                continue;
            }
            Item::Tree { name, id } => {
                if shown {
                    out.push('\n');
                }
                shown = true;
                out.push_str(&ctx.paint(&format!("tree {name}")));
                out.push_str("\n\n");
                out.push_str(&super::show_object::tree_entries(git, id)?);
                continue;
            }
            Item::Tag(tag) => {
                if shown {
                    out.push('\n');
                }
                shown = true;
                out.push_str(&tag_header(&pretty, &ctx, tag));
                continue;
            }
        };
        let c = git.repo.commit(id)?;
        let mut against = |source: Against| -> Result<Option<Body>> {
            let old_tree = match source {
                Against::Parent(Some(p)) => Some(git.repo.commit(&p)?.tree),
                Against::Parent(None) => None,
                Against::Tree(t) => Some(t),
            };
            let (mut old, mut new) = (Tree::new(), Tree::new());
            let changes = git
                .repo
                .diff_trees(old_tree.as_ref(), Some(&c.tree), &spec)?;
            add_changes(git, &mut old, &mut new, &changes)?;
            if wants_harder(git, format) {
                add_unchanged(
                    git,
                    (&mut old, &mut new),
                    (old_tree.as_ref(), Some(&c.tree)),
                    &spec,
                )?;
            }
            let mut text = String::new();
            let differs = diff::compare(git, &old, &new, &[], format, &mut text)?;
            if no_patch {
                return Ok(differs.then(|| Body {
                    text: String::new(),
                    dashes: false,
                }));
            }
            check.push_str(&text);
            let dashes = format.with_patch && format.kind == diff::Kind::Stat;
            Ok(diff::has_body(format, differs, &text).then_some(Body { text, dashes }))
        };
        let segments = shape.segments(&plan, &c, &|| Want::of_format(format), &mut against)?;
        let report = if signatures {
            super::verify_sig::commit_report(git, id)?
        } else {
            None
        };
        for seg in segments {
            if shown && !pretty.terminated() {
                out.push('\n');
            }
            shown = true;
            let text = render(&pretty, &ctx, id, &c);
            let text = if decorate.on() {
                with_decoration(text, &pretty, names.get(id))
            } else {
                text
            };
            let text = match &report {
                Some(r) => super::verify_sig::with_report(text, r),
                None => text,
            };
            out.push_str(&with_from(text, &pretty, seg.from.as_ref(), &abbreviations));
            out.push_str(pretty.terminator());
            out.push_str(&seg.tail);
        }
    }
    emit(&out);
    diff::exit_if_check_failed(format, &check);
    Ok(())
}

/// Adds `.pyn/` to `.git/info/exclude` unless a line already hides it; true when the file was changed.
fn add_exclude(exclude: &Path) -> Result<bool> {
    let existing = match std::fs::read_to_string(exclude) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let hidden = existing
        .lines()
        .map(str::trim)
        .any(|l| matches!(l, ".pyn" | ".pyn/" | "/.pyn" | "/.pyn/"));
    if hidden {
        return Ok(false);
    }
    if let Some(dir) = exclude.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lead = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(exclude)?;
    file.write_all(format!("{lead}{EXCLUDE_LINE}\n").as_bytes())?;
    Ok(true)
}

/// Hides `.pyn/` from git and records the `git` repository tool; returns the exclude file and whether it changed.
pub(super) fn hide_pyn(git: &GitHome) -> Result<(PathBuf, bool)> {
    let exclude = git.repo.git().common_dir().join("info/exclude");
    let added = add_exclude(&exclude)?;
    std::fs::create_dir_all(git.ws.dir.join("local_only"))?;
    crate::repo_tool::set(&git.ws, crate::repo_tool::RepoTool::Git)?;
    Ok((exclude, added))
}

fn init(git: &GitHome) -> Result<()> {
    let root = git.repo.root()?;
    let (exclude, added) = hide_pyn(git)?;
    let note = if added {
        format!("added {EXCLUDE_LINE} to {}", exclude.display())
    } else {
        format!("{EXCLUDE_LINE} is already excluded")
    };
    println!(
        "initialized pyn in the git repository at {} ({note})",
        root.display()
    );
    Ok(())
}

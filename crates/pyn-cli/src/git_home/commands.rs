//! The commands a git repository runs: `status`, `diff`, `log`, `show` and `init`, in git's output shapes.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use pyn_git::Refspec;
use pyn_gitwork::{
    Change, Conflict, Entry, Head, Kind, Pathspec, StatusEntry, SubmoduleIgnore, Submodules,
    fs_path, hash_worktree_file,
};
use pyn_object::ObjectId;

use super::{GitHome, path_string, stage};
use crate::backend::Backend;
use crate::commit_format::{Context, Decorations, FormatArgs, Rendering, render};
use crate::diff::{self, Blob, Source, Tree};
use crate::short_status::{self, Format, Row, StatusArgs};
use crate::{Command, table, unsupported};

/// The commit abbreviation `Merge:` lines use.
const ABBREV: usize = 7;
const EXCLUDE_LINE: &str = ".pyn/";

/// Runs `command` on a git repository, or refuses it with the home that provides it.
pub fn run(command: Command, git: &GitHome, cwd: &Path) -> Result<()> {
    let server_only = |feature: &str| -> Result<()> {
        match git.pyn_server(feature) {
            Ok(_) => unreachable!("a git home has no pyn server"),
            Err(e) => Err(e),
        }
    };
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
        } => {
            let differs = diff(git, cwd, &args, &paths, &format.format(), quiet, cached)?;
            if differs && (exit_code || quiet) {
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Show {
            commit,
            no_patch,
            fmt,
            format,
        } => show(git, commit.as_deref(), no_patch, &fmt, &format.format()),
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
            super::log::log(git, cwd, path, limit, args)
        }
        Command::Convert(_) => init(git),
        Command::Init(_) => unreachable!("handled before a repository is detected"),
        Command::InitGit(_) => unreachable!("handled before a repository is detected"),
        Command::Files => crate::files_command(git),
        Command::Ls { path } => crate::ls_command(git, path.as_deref()),
        Command::Merge(args) => super::merge::merge(git, args),
        Command::Branch(args) => super::branches::branch(git, args),
        Command::Switch(args) => super::switch::switch(git, args),
        Command::Checkout(args) => super::switch::checkout(git, cwd, args),
        Command::Tag(args) => super::tags::tag(git, args),
        Command::Stash(args) => super::stash::stash(git, cwd, args),
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
        Command::Reflog(args) => super::reflog::reflog(git, args),
        Command::Rebase(args) => super::rebase::rebase(git, &args),
        Command::Remote(args) => super::remote::remote(git, args),
        Command::Fetch(args) => super::fetch::fetch(git, args),
        Command::Pull(args) => super::pull::pull(git, args),
        Command::Push(args) => super::push::push(git, args),
        Command::Add {
            paths, patch: true, ..
        } => super::patch_mode::add(git, cwd, &paths),
        Command::Add {
            paths,
            update,
            dry_run,
            verbose,
            force,
            ..
        } => {
            let options = stage::AddOptions {
                paths,
                update,
                dry_run,
                verbose,
                force,
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
            )
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
            },
        ),
        Command::Restore {
            args,
            source,
            staged: true,
            worktree: false,
            ..
        } => stage::restore_staged(git, cwd, &args, source.as_deref()),
        Command::Restore {
            args,
            source,
            staged,
            worktree,
            ..
        } => super::switch::restore(git, cwd, &args, source.as_deref(), staged, worktree),
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

fn short(id: &ObjectId) -> String {
    id.to_hex()[..ABBREV].to_string()
}

pub(super) fn status(git: &GitHome, cwd: &Path, args: &StatusArgs) -> Result<()> {
    git.repo.validate_diff_ignore_submodules()?;
    let mut status = git.repo.status()?;
    let specs = stage::resolve_specs(git, cwd, &args.paths)?;
    status.entries.retain(|e| specs.spec.matches(&e.path));
    let format = args.format();
    if format != Format::Table {
        let branch = args
            .branch
            .then(|| branch_line(git, &status.head))
            .transpose()?;
        let prefix = git.ws.repo_prefix(cwd, ".")?;
        let quote_high = git.repo.bool_setting("core.quotePath", true);
        let rows = short_rows(&status.entries, format);
        return short_status::print(
            &rows,
            branch.as_deref(),
            format,
            prefix.as_bytes(),
            quote_high,
        );
    }
    status_table(&status)
}

pub(super) fn status_table(status: &pyn_gitwork::Status) -> Result<()> {
    let line = match &status.head {
        Head::Branch { name, id: Some(_) } => format!("On branch {name}"),
        Head::Branch { name, id: None } => format!("On branch {name} (no commits yet)"),
        Head::Detached(id) => format!("HEAD detached at {}", short(id)),
    };
    println!("{line}");
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

fn short_rows(entries: &[StatusEntry], format: Format) -> Vec<Row> {
    let mut rows = Vec::new();
    for e in entries {
        if let Some(c) = e.conflict {
            let (x, y) = conflict_code(c);
            rows.push(Row {
                x,
                y,
                path: e.path.clone(),
            });
        } else if e.unstaged == Some(Kind::Untracked) {
            if e.staged.is_some() {
                rows.push(Row {
                    x: letter(e.staged),
                    y: ' ',
                    path: e.path.clone(),
                });
            }
            rows.push(Row::untracked(e.path.clone()));
        } else {
            rows.push(Row {
                x: letter(e.staged),
                y: match e.submodule {
                    Some(s) if format == Format::Short && !s.new_commits => {
                        if s.modified {
                            'm'
                        } else {
                            '?'
                        }
                    }
                    _ => letter(e.unstaged),
                },
                path: e.path.clone(),
            });
        }
    }
    rows
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
    let config = git.config();
    let tracking = pyn_git::tracking(config, name);
    let (Some(remote), Some(merge)) = (tracking.remote, tracking.merge.first()) else {
        return Ok(line);
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
        return Ok(line);
    };
    let shown = upstream
        .strip_prefix("refs/remotes/")
        .or_else(|| upstream.strip_prefix("refs/heads/"))
        .unwrap_or(&upstream);
    line.push_str(&format!("...{shown}"));
    let Some(up_id) = git.repo.refs().resolve(&upstream)? else {
        line.push_str(" [gone]");
        return Ok(line);
    };
    if let Some(local) = local {
        let (mine, theirs) = (git.reachable(&local)?, git.reachable(&up_id)?);
        let ahead = mine.difference(&theirs).count();
        let behind = theirs.difference(&mine).count();
        match (ahead, behind) {
            (0, 0) => {}
            (a, 0) => line.push_str(&format!(" [ahead {a}]")),
            (0, b) => line.push_str(&format!(" [behind {b}]")),
            (a, b) => line.push_str(&format!(" [ahead {a}, behind {b}]")),
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
    let (mode, id) = hash_worktree_file(git.repo.algorithm(), &file)?;
    Ok(Some(Blob {
        hash: id.to_hex(),
        mode,
        source: Source::Local { file },
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

/// A `a..b` pair, or a leading revision followed by paths; a name that is both a revision and a file is refused.
fn split_revisions(
    git: &GitHome,
    cwd: &Path,
    args: &[String],
    separated: bool,
) -> Result<(Vec<ObjectId>, Vec<String>)> {
    let mut revs: Vec<ObjectId> = Vec::new();
    let mut rest = args;
    while revs.len() < 2
        && let Some((first, tail)) = rest.split_first()
    {
        if let Some((a, b)) = first.split_once("..")
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
        if !separated && std::fs::symlink_metadata(cwd.join(first)).is_ok() {
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
fn check_loose(cwd: &Path, loose: &[String], separated: bool) -> Result<()> {
    if separated {
        if let Some(bad) = loose.first() {
            bail!("bad revision '{bad}'");
        }
        return Ok(());
    }
    for (i, arg) in loose.iter().enumerate() {
        if arg.starts_with(':') || std::fs::symlink_metadata(cwd.join(arg)).is_ok() {
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

pub(super) fn repo_paths(git: &GitHome, cwd: &Path, paths: &[String]) -> Result<Vec<String>> {
    paths
        .iter()
        .filter(|p| p.as_str() != ".")
        .map(|p| git.ws.repo_path(cwd, p))
        .collect()
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
    let (revs, loose) = split_revisions(git, cwd, args, separated)?;
    check_loose(cwd, &loose, separated)?;
    let only = repo_paths(git, cwd, &[loose.as_slice(), paths].concat())?;
    let spec = Pathspec::new(only.iter().map(|p| p.as_bytes().to_vec()));
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
                    || !matches!(kind, Kind::Modified | Kind::Deleted | Kind::TypeChange)
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
        }
        [one] => {
            let tree = repo.commit(one)?.tree;
            let committed = repo.flatten(Some(&tree), &spec)?;
            let index = repo.read_index()?;
            let leaves = repo.index_leaves(&index);
            let unstaged = repo.unstaged_changes_with_submodules(&index, &modules)?;
            let paths: BTreeSet<&Vec<u8>> = committed.keys().chain(leaves.keys()).collect();
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
        }
        _ => unreachable!("at most two revisions are taken"),
    }
    filter_gitlinks(&modules, &mut old, &mut new)?;
    let mut out = String::new();
    let differs = diff::compare(git, &old, &new, &only, format, &mut out)?;
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

fn show(
    git: &GitHome,
    commit: Option<&str>,
    no_patch: bool,
    fmt: &FormatArgs,
    format: &diff::Format,
) -> Result<()> {
    git.repo.validate_diff_ignore_submodules()?;
    let Rendering { pretty, style } = fmt.rendering()?;
    let id = match commit {
        Some(c) => git.repo.resolve(c)?,
        None => head_or_refuse(git)?,
    };
    let c = git.repo.commit(&id)?;
    let names = if pretty.uses_decorations() {
        super::log::decorations(git)?
    } else {
        Decorations::new()
    };
    let ctx = Context {
        style,
        now: chrono::Utc::now().timestamp(),
        decorations: &names,
    };
    let mut out = render(&pretty, &ctx, &id, &c);
    out.push_str(pretty.terminator());
    if no_patch {
        emit(&out);
        return Ok(());
    }
    let mut body = String::new();
    let mut differs = false;
    if c.parents.len() <= 1 {
        let old_tree = match c.parents.first() {
            Some(p) => Some(git.repo.commit(p)?.tree),
            None => None,
        };
        let (mut old, mut new) = (Tree::new(), Tree::new());
        let changes =
            git.repo
                .diff_trees(old_tree.as_ref(), Some(&c.tree), &Pathspec::default())?;
        add_changes(git, &mut old, &mut new, &changes)?;
        differs = diff::compare(git, &old, &new, &[], format, &mut body)?;
    }
    if diff::has_body(format, differs, &body) || c.parents.len() > 1 {
        if pretty.blank_before_diff() {
            out.push('\n');
        }
        out.push_str(&body);
    }
    emit(&out);
    diff::exit_if_check_failed(format, &body);
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

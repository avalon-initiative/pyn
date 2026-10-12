//! The commands a git repository runs: `status`, `diff`, `log`, `show` and `init`, in git's output shapes.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use anyhow::{Result, bail};
use pyn_gitwork::{Change, Entry, Head, Kind, Pathspec, fs_path, hash_worktree_file};
use pyn_object::{Commit as GitCommit, ObjectId};

use super::{GitHome, file_mode, path_string, stage};
use crate::backend::Backend;
use crate::diff::{self, Blob, Source, Tree};
use crate::{Command, table, time, unsupported};

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
        Command::Status { revision } => {
            if revision.is_some() {
                bail!(
                    "`pyn status --revision` compares with a pyn commit; a git repository has none"
                );
            }
            status(git)
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
        Command::Show { commit, format } => show(git, commit.as_deref(), &format.format()),
        Command::Log {
            path,
            filter,
            limit,
            commits,
        } => {
            if filter.is_some() {
                bail!(
                    "`pyn log --filter` is not available on a git repository; name a path instead"
                );
            }
            log(git, cwd, path.as_deref(), limit, commits)
        }
        Command::Init => init(git),
        Command::Files => crate::files_command(git),
        Command::Ls { path } => crate::ls_command(git, path.as_deref()),
        Command::Merge(args) => super::merge::merge(git, args),
        Command::Branch(args) => super::branches::branch(git, args),
        Command::Switch(args) => super::switch::switch(git, args),
        Command::Checkout(args) => super::switch::checkout(git, cwd, args),
        Command::Tag(args) => super::tags::tag(git, args),
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
        Command::Commit {
            paths,
            file,
            base,
            message,
            all,
        } => {
            if !paths.is_empty() {
                bail!(
                    "`pyn commit <path>` is not available on a git repository; \
                     stage the paths with `pyn add` and run `pyn commit`"
                );
            }
            if file.is_some() || base.is_some() {
                bail!("`pyn commit --file` and `--base` need a pyn server");
            }
            stage::commit(git, &message, all)
        }
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
        | Command::Role(_) => unreachable!("run from anywhere, not through a git home"),
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
fn emit(text: &str) {
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

fn status(git: &GitHome) -> Result<()> {
    let status = git.repo.status()?;
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

fn entry_blob(e: &Entry) -> Option<Blob> {
    Some(Blob {
        hash: e.id.to_hex(),
        mode: file_mode(e.mode)?,
        source: Source::Object { id: e.id.to_hex() },
    })
}

fn disk_blob(git: &GitHome, path: &[u8]) -> Result<Option<Blob>> {
    let file = fs_path(git.repo.root()?, path);
    let Ok(meta) = std::fs::symlink_metadata(&file) else {
        return Ok(None);
    };
    if !meta.is_file() && !meta.file_type().is_symlink() {
        return Ok(None);
    }
    let (mode, id) = hash_worktree_file(git.repo.algorithm(), &file)?;
    Ok(file_mode(mode).map(|mode| Blob {
        hash: id.to_hex(),
        mode,
        source: Source::Local { file },
    }))
}

fn add_changes(old: &mut Tree, new: &mut Tree, changes: &[Change]) {
    for c in changes {
        let path = path_string(&c.path);
        if let Some(b) = c.old.as_ref().and_then(entry_blob) {
            old.insert(path.clone(), b);
        }
        if let Some(b) = c.new.as_ref().and_then(entry_blob) {
            new.insert(path, b);
        }
    }
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

fn repo_paths(git: &GitHome, cwd: &Path, paths: &[String]) -> Result<Vec<String>> {
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
    let (revs, loose) = split_revisions(git, cwd, args, !paths.is_empty())?;
    let only = repo_paths(git, cwd, &[loose.as_slice(), paths].concat())?;
    let spec = Pathspec::new(only.iter().map(|p| p.as_bytes().to_vec()));
    let repo = &git.repo;
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
            for (path, kind) in repo.unstaged_changes(&index)? {
                if !spec.matches(&path)
                    || !matches!(kind, Kind::Modified | Kind::Deleted | Kind::TypeChange)
                {
                    continue;
                }
                let name = path_string(&path);
                if let Some(b) = leaves.get(&path).and_then(entry_blob) {
                    old.insert(name.clone(), b);
                }
                if let Some(b) = disk_blob(git, &path)? {
                    new.insert(name, b);
                }
            }
        }
        [one] => {
            let tree = repo.commit(one)?.tree;
            let committed = repo.flatten(Some(&tree), &spec)?;
            let index = repo.read_index()?;
            let leaves = repo.index_leaves(&index);
            let unstaged = repo.unstaged_changes(&index)?;
            let paths: BTreeSet<&Vec<u8>> = committed.keys().chain(leaves.keys()).collect();
            for path in paths.into_iter().filter(|p| spec.matches(p)) {
                let name = path_string(path);
                if let Some(b) = committed.get(path).and_then(entry_blob) {
                    old.insert(name.clone(), b);
                }
                let current = match (leaves.get(path), unstaged.contains_key(path)) {
                    (Some(e), false) => entry_blob(e),
                    _ => disk_blob(git, path)?,
                };
                if let Some(b) = current {
                    new.insert(name, b);
                }
            }
        }
        [a, b] => {
            let (ta, tb) = (repo.commit(a)?.tree, repo.commit(b)?.tree);
            add_changes(
                &mut old,
                &mut new,
                &repo.diff_trees(Some(&ta), Some(&tb), &spec)?,
            );
        }
        _ => unreachable!("at most two revisions are taken"),
    }
    let mut out = String::new();
    let differs = diff::compare(git, &old, &new, &only, format, &mut out)?;
    if !quiet {
        emit(&out);
    }
    Ok(differs)
}

fn message_lines(message: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(message);
    let mut lines: Vec<&str> = text.split('\n').collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let skip = lines.iter().take_while(|l| l.trim().is_empty()).count();
    lines[skip..].iter().map(|l| format!("    {l}")).collect()
}

/// The `commit`, `Merge:`, `Author:` and `Date:` lines and the indented message, as `git log` and `git show` print them.
fn commit_block(id: &ObjectId, c: &GitCommit) -> String {
    let mut out = format!("commit {id}\n");
    if c.parents.len() > 1 {
        let merged: Vec<String> = c.parents.iter().map(short).collect();
        out.push_str(&format!("Merge: {}\n", merged.join(" ")));
    }
    out.push_str(&format!(
        "Author: {} <{}>\nDate:   {}\n\n",
        String::from_utf8_lossy(&c.author.name),
        String::from_utf8_lossy(&c.author.email),
        time::git_date(c.author.time, c.author.offset_minutes)
    ));
    for line in message_lines(&c.message) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn head_or_refuse(git: &GitHome) -> Result<ObjectId> {
    match git.repo.head()? {
        Head::Branch { name, id: None } => {
            bail!("your current branch '{name}' does not have any commits yet")
        }
        head => Ok(head
            .id()
            .expect("a head with no commit is the unborn branch")),
    }
}

fn show(git: &GitHome, commit: Option<&str>, format: &diff::Format) -> Result<()> {
    let id = match commit {
        Some(c) => git.repo.resolve(c)?,
        None => head_or_refuse(git)?,
    };
    let c = git.repo.commit(&id)?;
    let mut body = String::new();
    if c.parents.len() <= 1 {
        let old_tree = match c.parents.first() {
            Some(p) => Some(git.repo.commit(p)?.tree),
            None => None,
        };
        let (mut old, mut new) = (Tree::new(), Tree::new());
        let changes =
            git.repo
                .diff_trees(old_tree.as_ref(), Some(&c.tree), &Pathspec::default())?;
        add_changes(&mut old, &mut new, &changes);
        diff::compare(git, &old, &new, &[], format, &mut body)?;
    }
    let mut out = commit_block(&id, &c);
    if !body.is_empty() || c.parents.len() > 1 {
        out.push('\n');
        out.push_str(&body);
    }
    emit(&out);
    Ok(())
}

/// What the one argument of `pyn log` names: a path that exists or existed, else a revision.
enum LogTarget {
    Head,
    Path(String),
    Revision(ObjectId),
}

fn log_target(git: &GitHome, cwd: &Path, arg: Option<&str>) -> Result<LogTarget> {
    let Some(arg) = arg else {
        return Ok(LogTarget::Head);
    };
    if arg == "." {
        return Ok(LogTarget::Head);
    }
    let known = |rel: &str| -> Result<bool> {
        if std::fs::symlink_metadata(fs_path(git.repo.root()?, rel.as_bytes())).is_ok() {
            return Ok(true);
        }
        let Some(head) = git.repo.head_commit()? else {
            return Ok(false);
        };
        let tree = git.repo.commit(&head)?.tree;
        Ok(git.repo.entry_at(&tree, rel.as_bytes())?.is_some())
    };
    if let Ok(rel) = git.ws.repo_path(cwd, arg)
        && known(&rel)?
    {
        return Ok(LogTarget::Path(rel));
    }
    match git.repo.resolve(arg) {
        Ok(id) => Ok(LogTarget::Revision(id)),
        Err(_) => Ok(LogTarget::Path(git.ws.repo_path(cwd, arg)?)),
    }
}

fn log(
    git: &GitHome,
    cwd: &Path,
    arg: Option<&str>,
    limit: Option<usize>,
    commits: bool,
) -> Result<()> {
    let (start, spec) = match log_target(git, cwd, arg)? {
        LogTarget::Head => (head_or_refuse(git)?, Pathspec::default()),
        LogTarget::Path(p) => (head_or_refuse(git)?, Pathspec::new([p.into_bytes()])),
        LogTarget::Revision(id) => (id, Pathspec::default()),
    };
    let mut walk = git.repo.log(&[start], spec)?;
    if commits {
        return log_table(git, &mut walk, limit.unwrap_or(50));
    }
    let mut out = String::new();
    let mut shown = 0usize;
    while limit.is_none_or(|n| shown < n)
        && let Some(entry) = walk.next_entry()?
    {
        if shown > 0 {
            out.push('\n');
        }
        out.push_str(&commit_block(&entry.id, &entry.commit));
        shown += 1;
        if out.len() > 64 * 1024 {
            emit(&std::mem::take(&mut out));
        }
    }
    emit(&out);
    Ok(())
}

fn log_table(git: &GitHome, walk: &mut pyn_gitwork::Log<'_>, limit: usize) -> Result<()> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    while rows.len() < limit
        && let Some(entry) = walk.next_entry()?
    {
        let c = &entry.commit;
        let parent_tree = match c.parents.first() {
            Some(p) => Some(git.repo.commit(p)?.tree),
            None => None,
        };
        let files = git
            .repo
            .diff_trees(parent_tree.as_ref(), Some(&c.tree), &Pathspec::default())?
            .len();
        rows.push(vec![
            crate::snapshot::short(&entry.id.to_hex()).to_string(),
            String::from_utf8_lossy(&c.author.name).into_owned(),
            time::local(chrono::DateTime::from_timestamp(c.author.time, 0).unwrap_or_default()),
            files.to_string(),
            message_lines(&c.message)
                .first()
                .map_or(String::new(), |l| l.trim_start().to_string()),
        ]);
    }
    table::show(
        &["COMMIT", "AUTHOR", "WHEN", "FILES", "MESSAGE"],
        &rows,
        "no commits",
    );
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

fn init(git: &GitHome) -> Result<()> {
    let root = git.repo.root()?;
    let exclude = git.repo.git().common_dir().join("info/exclude");
    let added = add_exclude(&exclude)?;
    std::fs::create_dir_all(git.ws.dir.join("local_only"))?;
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

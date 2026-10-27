//! `pyn add -i`: git's status, update, revert, add untracked, patch, diff and help menu, with its list prompt.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::Result;
use pyn_diff::{is_binary, line_counts};
use pyn_git::IndexEntry;
use pyn_gitwork::{Entry, Pathspec, fs_path};
use pyn_object::{FileMode, ObjectId};

use super::patch_mode;
use super::stage::{
    OpenIndex, entries_of, index_of, link_target, put_entry, raw_mode, remove_entry, resolve_specs,
    stage_file, untracked_files,
};
use super::{GitHome, path_string};

const SEPARATORS: &[u8] = b" \t\r\n,";
const MAX_PREFIX: usize = 4;

#[derive(Clone, Copy, Default)]
struct AddDel {
    add: usize,
    del: usize,
    seen: bool,
    binary: bool,
}

impl AddDel {
    fn render(&self, none: &str) -> String {
        if self.binary {
            "binary".into()
        } else if self.seen {
            format!("+{}/-{}", self.add, self.del)
        } else {
            none.into()
        }
    }
}

struct Row {
    name: Vec<u8>,
    prefix: usize,
    unmerged: bool,
    index: AddDel,
    worktree: AddDel,
}

impl Row {
    fn named(name: Vec<u8>) -> Self {
        Self {
            name,
            prefix: 0,
            unmerged: false,
            index: AddDel::default(),
            worktree: AddDel::default(),
        }
    }
}

/// The items of a list, as the prompt shows them, with the shortest unique prefix of each name.
struct Items {
    rows: Vec<Row>,
    selected: Vec<bool>,
}

impl Items {
    fn new(rows: Vec<Row>) -> Self {
        Self {
            selected: vec![false; rows.len()],
            rows,
        }
    }

    fn sorted(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.rows.len()).collect();
        order.sort_by(|a, b| self.rows[*a].name.cmp(&self.rows[*b].name));
        order
    }

    fn find_unique_prefixes(&mut self) {
        let order = self.sorted();
        for (at, &i) in order.iter().enumerate() {
            let name = self.rows[i].name.clone();
            let mut len = 0;
            while len < 1 {
                match name.get(len) {
                    Some(c) if c.is_ascii() => len += 1,
                    _ => {
                        len = 0;
                        break;
                    }
                }
            }
            if at > 0 {
                extend_prefix(&name, &mut len, &self.rows[order[at - 1]].name);
            }
            if at + 1 < order.len() {
                extend_prefix(&name, &mut len, &self.rows[order[at + 1]].name);
            }
            self.rows[i].prefix = len;
        }
    }

    fn find_unique(&self, given: &[u8]) -> Option<usize> {
        let order = self.sorted();
        let name = |at: usize| self.rows[order[at]].name.as_slice();
        let at = order.partition_point(|&i| self.rows[i].name.as_slice() < given);
        if at < order.len() && name(at) == given {
            return Some(order[at]);
        }
        if at > 0 && name(at - 1).starts_with(given) {
            return None;
        }
        if at + 1 < order.len() && name(at + 1).starts_with(given) {
            return None;
        }
        if at < order.len() && name(at).starts_with(given) {
            return Some(order[at]);
        }
        None
    }
}

fn extend_prefix(name: &[u8], len: &mut usize, other: &[u8]) {
    if *len == 0 || name[..*len] != other[..(*len).min(other.len())] || other.len() < *len {
        return;
    }
    loop {
        let Some(&c) = name.get(*len) else {
            *len = 0;
            return;
        };
        *len += 1;
        if *len > MAX_PREFIX || !c.is_ascii() {
            *len = 0;
            return;
        }
        if other.get(*len - 1) != Some(&c) {
            return;
        }
    }
}

/// A prefix the prompt would read as something else is not highlighted.
fn valid_prefix(name: &[u8], len: usize) -> bool {
    let prefix = &name[..len];
    len > 0
        && !prefix.iter().any(|b| SEPARATORS.contains(b))
        && prefix[0] != b'-'
        && !prefix[0].is_ascii_digit()
        && !(len == 1 && matches!(prefix[0], b'*' | b'?'))
}

fn highlighted(row: &Row) -> String {
    let text = String::from_utf8_lossy(&row.name).into_owned();
    if row.prefix > 0 && valid_prefix(&row.name, row.prefix) {
        let head = String::from_utf8_lossy(&row.name[..row.prefix]);
        let tail = String::from_utf8_lossy(&row.name[row.prefix..]);
        format!("[{head}]{tail}")
    } else {
        text
    }
}

fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn read_line() -> Option<Vec<u8>> {
    let mut line = Vec::new();
    match std::io::stdin().lock().read_until(b'\n', &mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_ascii().to_vec()),
    }
}

const ERROR: isize = -1;
const QUIT: isize = -2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Multiple,
    Singleton,
    Immediate,
}

struct Style<'a> {
    prompt: &'a str,
    header: Option<&'a str>,
    columns: usize,
    mode: Mode,
    names_only: bool,
    help: fn(),
}

fn print_list(items: &Items, selected: bool, style: &Style) {
    if items.rows.is_empty() {
        return;
    }
    let mut out = String::new();
    if let Some(h) = style.header {
        out.push_str(h);
        out.push('\n');
    }
    let mut last_lf = false;
    for (i, row) in items.rows.iter().enumerate() {
        let mark = if selected && items.selected[i] {
            '*'
        } else {
            ' '
        };
        let name = highlighted(row);
        if style.columns > 0 {
            out.push_str(&format!(" {:>2}: {name}", i + 1));
        } else if style.names_only {
            out.push_str(&format!("{mark}{:>2}: {name}", i + 1));
        } else {
            let (staged, unstaged) = (
                row.index.render("unchanged"),
                row.worktree.render("nothing"),
            );
            out.push_str(&format!(
                "{mark}{:>2}: {staged:>12} {unstaged:>12} {name}",
                i + 1
            ));
        }
        if style.columns > 0 && (i + 1) % style.columns != 0 {
            out.push('\t');
            last_lf = false;
        } else {
            out.push('\n');
            last_lf = true;
        }
    }
    if !last_lf {
        out.push('\n');
    }
    say(&out);
}

fn strtoul(text: &[u8]) -> (u64, usize) {
    let digits = text.iter().take_while(|b| b.is_ascii_digit()).count();
    let value = std::str::from_utf8(&text[..digits])
        .ok()
        .and_then(|d| d.parse::<u64>().ok())
        .unwrap_or(u64::MAX);
    (value, digits)
}

/// git's list prompt: the selected count (or index when a single item is wanted), `QUIT` at the end of input.
fn list_and_choose(items: &mut Items, style: &Style) -> isize {
    let singleton = style.mode == Mode::Singleton;
    let immediate = style.mode != Mode::Multiple;
    let mut res: isize = if singleton { ERROR } else { 0 };
    items.selected = vec![false; items.rows.len()];
    items.find_unique_prefixes();
    let n = items.rows.len() as i64;
    loop {
        print_list(items, !singleton, style);
        say(&format!(
            "{}{}",
            style.prompt,
            if singleton { "> " } else { ">> " }
        ));
        let Some(input) = read_line() else {
            say("\n");
            if immediate {
                res = QUIT;
            }
            break;
        };
        if input.is_empty() {
            break;
        }
        if input == b"?" {
            (style.help)();
            continue;
        }
        let mut p: &[u8] = &input;
        loop {
            let mut sep = p
                .iter()
                .position(|b| SEPARATORS.contains(b))
                .unwrap_or(p.len());
            if sep == 0 {
                if p.is_empty() {
                    break;
                }
                p = &p[1..];
                continue;
            }
            let mut choose = true;
            if p[0] == b'-' {
                choose = false;
                p = &p[1..];
                sep -= 1;
            }
            let (mut from, mut to): (i64, i64) = (-1, -1);
            if sep == 1 && p[0] == b'*' {
                from = 0;
                to = n;
            } else if p.first().is_some_and(u8::is_ascii_digit) {
                let (first, used) = strtoul(p);
                from = first.wrapping_sub(1) as i64;
                if used == sep {
                    to = from.wrapping_add(1);
                } else if p[used] == b'-' {
                    let rest = &p[used + 1..];
                    let mut end = used + 1;
                    if rest.first().is_some_and(u8::is_ascii_digit) {
                        let (last, len) = strtoul(rest);
                        to = last as i64;
                        end += len;
                    } else {
                        to = n;
                    }
                    if end != sep {
                        from = -1;
                    }
                }
            }
            let token = &p[..sep];
            let advance = if p.len() > sep { sep + 1 } else { sep };
            if from < 0
                && let Some(i) = items.find_unique(token)
            {
                from = i as i64;
                to = from + 1;
            }
            if from < 0 || from >= n || (singleton && from + 1 != to) {
                eprintln!("Huh ({})?", String::from_utf8_lossy(token));
                break;
            } else if singleton {
                res = from as isize;
                break;
            }
            // git compares the signed end with an unsigned count, so a missing end (-1) means "to the end"
            let to = if to as u64 > n as u64 { n } else { to };
            let mut at = from;
            while at < to {
                let slot = &mut items.selected[at as usize];
                if *slot != choose {
                    *slot = choose;
                    res += if choose { 1 } else { -1 };
                }
                at += 1;
            }
            p = &p[advance..];
        }
        if (immediate && res != ERROR) || input == b"*" {
            break;
        }
    }
    res
}

fn prompt_help() {
    say("Prompt help:\n\
         1          - select a single item\n\
         3-5        - select a range of items\n\
         2-3,6-9    - select multiple ranges\n\
         foo        - select item based on unique prefix\n\
         -...       - unselect specified items\n\
         *          - choose all items\n           - (empty) finish selecting\n");
}

fn command_help() {
    say("Prompt help:\n\
         1          - select a numbered item\n\
         foo        - select item based on unique prefix\n           - (empty) select nothing\n");
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Worktree,
    Index,
}

/// What `get_modified_files` collects: the changes of each file and how many are binary.
struct Modified {
    rows: Vec<Row>,
    binary: usize,
}

fn gitlink_text(id: &ObjectId) -> Vec<u8> {
    format!("Subproject commit {}\n", id.to_hex()).into_bytes()
}

fn blob_bytes(git: &GitHome, entry: &Entry) -> Result<Vec<u8>> {
    if entry.mode == FileMode::Gitlink {
        return Ok(gitlink_text(&entry.id));
    }
    Ok(git.repo.blob(&entry.id)?)
}

fn count(old: &[u8], new: &[u8]) -> AddDel {
    if is_binary(old) || is_binary(new) {
        return AddDel {
            seen: true,
            binary: true,
            ..AddDel::default()
        };
    }
    let (add, del) = line_counts(old, new, false);
    AddDel {
        add,
        del,
        seen: true,
        binary: false,
    }
}

fn worktree_bytes(git: &GitHome, path: &[u8]) -> Vec<u8> {
    let Ok(root) = git.repo.root() else {
        return Vec::new();
    };
    let file = fs_path(root, path);
    match std::fs::symlink_metadata(&file) {
        Ok(meta) if meta.is_symlink() => link_target(&file).unwrap_or_default(),
        Ok(meta) if meta.is_file() => git
            .repo
            .read_clean(path, &file, None, false)
            .map(|c| c.data.into_owned())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn modified_files(
    git: &GitHome,
    opened: &OpenIndex,
    filter: Filter,
    spec: &Pathspec,
) -> Result<Modified> {
    let index = &opened.index;
    let head = git.repo.head()?;
    let tree = match head.id() {
        Some(id) => Some(git.repo.commit(&id)?.tree),
        None => None,
    };
    let committed = git.repo.flatten(tree.as_ref(), spec)?;
    let mut unmerged: BTreeMap<&[u8], Option<&IndexEntry>> = BTreeMap::new();
    for e in index.entries.iter().filter(|e| e.stage > 0) {
        if spec.matches(&e.path) {
            let ours = unmerged.entry(e.path.as_slice()).or_insert(None);
            if e.stage == 2 {
                *ours = Some(e);
            }
        }
    }
    let mut staged: BTreeMap<Vec<u8>, AddDel> = BTreeMap::new();
    let stage0: BTreeMap<&[u8], &IndexEntry> = index
        .entries
        .iter()
        .filter(|e| e.stage == 0)
        .map(|e| (e.path.as_slice(), e))
        .collect();
    let paths = committed
        .keys()
        .map(Vec::as_slice)
        .chain(stage0.keys().copied())
        .filter(|p| spec.matches(p) && !unmerged.contains_key(*p));
    for path in paths {
        let old = committed.get(path);
        let (new, intent) = match stage0.get(path) {
            Some(e) => (
                Some(Entry {
                    mode: super::stage::leaf_mode(e.mode),
                    id: e.id,
                }),
                e.intent_to_add,
            ),
            None => (None, false),
        };
        if !intent && old == new.as_ref() {
            continue;
        }
        let old_bytes = old.map_or(Ok(Vec::new()), |e| blob_bytes(git, e))?;
        let new_bytes = match (&new, intent) {
            (Some(_), true) | (None, _) => Vec::new(),
            (Some(e), false) => blob_bytes(git, e)?,
        };
        staged.insert(path.to_vec(), count(&old_bytes, &new_bytes));
    }
    for path in unmerged.keys() {
        staged.insert(
            path.to_vec(),
            AddDel {
                seen: true,
                ..AddDel::default()
            },
        );
    }
    let leaves = git.repo.index_leaves(index);
    let mut unstaged: BTreeMap<Vec<u8>, AddDel> = BTreeMap::new();
    for (path, _) in git.repo.unstaged_changes(index)? {
        if !spec.matches(&path) {
            continue;
        }
        if let Some(ours) = unmerged.get(path.as_slice()) {
            if let Some(e) = ours {
                let entry = Entry {
                    mode: super::stage::leaf_mode(e.mode),
                    id: e.id,
                };
                let old = blob_bytes(git, &entry)?;
                let new = worktree_bytes(git, &path);
                if old != new {
                    unstaged.insert(path, count(&old, &new));
                }
            } else {
                unstaged.insert(
                    path,
                    AddDel {
                        seen: true,
                        ..AddDel::default()
                    },
                );
            }
            continue;
        }
        let intent = stage0.get(path.as_slice()).is_some_and(|e| e.intent_to_add);
        let old = match leaves.get(&path) {
            Some(e) if !intent => blob_bytes(git, e)?,
            _ => Vec::new(),
        };
        let new = worktree_bytes(git, &path);
        if intent && new.is_empty() {
            continue;
        }
        unstaged.insert(path, count(&old, &new));
    }
    let mut rows: BTreeMap<Vec<u8>, Row> = BTreeMap::new();
    for (path, stat) in staged {
        rows.entry(path.clone())
            .or_insert_with(|| Row::named(path))
            .index = stat;
    }
    for (path, stat) in unstaged {
        rows.entry(path.clone())
            .or_insert_with(|| Row::named(path))
            .worktree = stat;
    }
    for path in unmerged.keys() {
        if let Some(r) = rows.get_mut(*path) {
            r.unmerged = true;
        }
    }
    rows.retain(|_, r| match filter {
        Filter::All => true,
        Filter::Worktree => r.worktree.seen,
        Filter::Index => r.index.seen,
    });
    let binary = rows
        .values()
        .filter(|r| r.index.binary || r.worktree.binary)
        .count();
    Ok(Modified {
        rows: rows.into_values().collect(),
        binary,
    })
}

struct Session<'a> {
    git: &'a GitHome,
    spec: Pathspec,
}

impl Session<'_> {
    fn style<'s>(&self, prompt: &'s str, mode: Mode, header: Option<&'s str>) -> Style<'s> {
        Style {
            prompt,
            header,
            columns: 0,
            mode,
            names_only: false,
            help: prompt_help,
        }
    }

    fn status(&self) -> Result<()> {
        let opened = self.git.open_index()?;
        let found = modified_files(self.git, &opened, Filter::All, &self.spec)?;
        let items = Items::new(found.rows);
        print_list(&items, false, &self.style("", Mode::Multiple, Some(HEADER)));
        say("\n");
        Ok(())
    }

    fn update(&self) -> Result<()> {
        let opened = self.git.open_index()?;
        let found = modified_files(self.git, &opened, Filter::Worktree, &self.spec)?;
        if found.rows.is_empty() {
            say("\n");
            return Ok(());
        }
        let mut items = Items::new(found.rows);
        let style = self.style("Update", Mode::Multiple, Some(HEADER));
        let chosen = list_and_choose(&mut items, &style);
        if chosen <= 0 {
            say("\n");
            return Ok(());
        }
        let mut entries = entries_of(&opened.index);
        let root = self.git.repo.root()?;
        for (row, _) in items.rows.iter().zip(&items.selected).filter(|(_, s)| **s) {
            if std::fs::symlink_metadata(fs_path(root, &row.name)).is_err() {
                remove_entry(&mut entries, &row.name);
                continue;
            }
            let existing = entries.get(&(row.name.clone(), 0));
            match stage_file(self.git, &row.name, existing) {
                Ok(entry) => put_entry(&mut entries, entry),
                Err(_) => {
                    eprintln!("error: could not stage '{}'", path_string(&row.name));
                    say("\n");
                    return Ok(());
                }
            }
        }
        self.git
            .save_index(&opened, &index_of(opened.index.version, entries))?;
        say(&format!("{}\n\n", paths_text("updated", chosen)));
        Ok(())
    }

    fn revert(&self) -> Result<()> {
        let opened = self.git.open_index()?;
        let found = modified_files(self.git, &opened, Filter::Index, &self.spec)?;
        if found.rows.is_empty() {
            say("\n");
            return Ok(());
        }
        let mut items = Items::new(found.rows);
        let style = self.style("Revert", Mode::Multiple, Some(HEADER));
        let chosen = list_and_choose(&mut items, &style);
        if chosen <= 0 {
            say("\n");
            return Ok(());
        }
        let head = self.git.repo.head()?;
        let tree = match head.id() {
            Some(id) => Some(self.git.repo.commit(&id)?.tree),
            None => None,
        };
        let committed = self.git.repo.flatten(tree.as_ref(), &Pathspec::default())?;
        let mut entries = entries_of(&opened.index);
        for (row, _) in items.rows.iter().zip(&items.selected).filter(|(_, s)| **s) {
            match committed.get(&row.name) {
                Some(e) => put_entry(
                    &mut entries,
                    IndexEntry::new(row.name.as_slice(), raw_mode(e.mode), e.id),
                ),
                None => {
                    remove_entry(&mut entries, &row.name);
                    say(&format!(
                        "note: {} is untracked now.\n",
                        path_string(&row.name)
                    ));
                }
            }
        }
        let reverted = index_of(opened.index.version, entries);
        self.git.save_index(&opened, &reverted)?;
        let remaining = patch_mode::needs_merge(&reverted);
        if remaining.is_empty() {
            say(&format!("{}\n\n", paths_text("reverted", chosen)));
        } else {
            say(&format!("{remaining}\n"));
        }
        Ok(())
    }

    fn add_untracked(&self) -> Result<()> {
        let opened = self.git.open_index()?;
        let files: Vec<Row> = untracked_files(self.git, &opened.index)?
            .into_iter()
            .filter(|p| self.spec.matches(p))
            .map(Row::named)
            .collect();
        if files.is_empty() {
            say("No untracked files.\n\n");
            return Ok(());
        }
        let mut items = Items::new(files);
        let style = Style {
            names_only: true,
            ..self.style("Add untracked", Mode::Multiple, Some(HEADER))
        };
        let chosen = list_and_choose(&mut items, &style);
        if chosen <= 0 {
            say("\n");
            return Ok(());
        }
        let mut entries = entries_of(&opened.index);
        for (row, _) in items.rows.iter().zip(&items.selected).filter(|(_, s)| **s) {
            match stage_file(self.git, &row.name, None) {
                Ok(entry) => put_entry(&mut entries, entry),
                Err(_) => {
                    eprintln!("error: could not stage '{}'", path_string(&row.name));
                    say("\n");
                    return Ok(());
                }
            }
        }
        self.git
            .save_index(&opened, &index_of(opened.index.version, entries))?;
        say(&format!("{}\n\n", paths_text("added", chosen)));
        Ok(())
    }

    fn patch(&self) -> Result<()> {
        let opened = self.git.open_index()?;
        let found = modified_files(self.git, &opened, Filter::Worktree, &self.spec)?;
        for row in found.rows.iter().filter(|r| r.unmerged) {
            eprintln!("ignoring unmerged: {}", path_string(&row.name));
        }
        let rows: Vec<Row> = found
            .rows
            .into_iter()
            .filter(|r| !r.unmerged && !r.index.binary && !r.worktree.binary)
            .collect();
        if rows.is_empty() {
            eprintln!(
                "{}",
                if found.binary > 0 {
                    "Only binary files changed."
                } else {
                    "No changes."
                }
            );
            return Ok(());
        }
        let mut items = Items::new(rows);
        let style = self.style("Patch update", Mode::Multiple, Some(HEADER));
        if list_and_choose(&mut items, &style) > 0 {
            let chosen = Pathspec::new(
                items
                    .rows
                    .iter()
                    .zip(&items.selected)
                    .filter(|(_, s)| **s)
                    .map(|(r, _)| r.name.clone()),
            );
            patch_mode::add_spec(self.git, &chosen)?;
        }
        Ok(())
    }

    fn diff(&self) -> Result<()> {
        let opened = self.git.open_index()?;
        let found = modified_files(self.git, &opened, Filter::Index, &self.spec)?;
        if found.rows.is_empty() {
            say("\n");
            return Ok(());
        }
        let mut items = Items::new(found.rows);
        let style = self.style("Review diff", Mode::Immediate, Some(HEADER));
        if list_and_choose(&mut items, &style) > 0 {
            let chosen: Vec<&Row> = items
                .rows
                .iter()
                .zip(&items.selected)
                .filter(|(_, s)| **s)
                .map(|(r, _)| r)
                .collect();
            let mut at = 0;
            while at < chosen.len() {
                if chosen[at].unmerged {
                    say(&format!(
                        "* Unmerged path {}\n",
                        path_string(&chosen[at].name)
                    ));
                    at += 1;
                    continue;
                }
                let end = chosen[at..]
                    .iter()
                    .position(|r| r.unmerged)
                    .map_or(chosen.len(), |n| at + n);
                self.show_diff(&chosen[at..end])?;
                at = end;
            }
        }
        say("\n");
        Ok(())
    }
}

impl Session<'_> {
    fn show_diff(&self, rows: &[&Row]) -> Result<()> {
        let mut args = vec!["diff".to_string(), "--cached".to_string()];
        if self.git.repo.head_commit()?.is_some() {
            args.push("HEAD".into());
        }
        args.push("--".into());
        args.extend(rows.iter().map(|r| path_string(&r.name)));
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(&args)
            .current_dir(self.git.repo.root()?)
            .status();
        if !status.is_ok_and(|s| s.success()) {
            eprintln!("error: could not run the diff");
        }
        Ok(())
    }
}

const HEADER: &str = "           staged     unstaged path";

fn paths_text(verb: &str, count: isize) -> String {
    format!("{verb} {count} path{}", if count == 1 { "" } else { "s" })
}

fn help() {
    say("status        - show paths with changes\n\
         update        - add working tree state to the staged set of changes\n\
         revert        - revert staged set of changes back to the HEAD version\n\
         patch         - pick hunks and update selectively\n\
         diff          - view diff between HEAD and index\n\
         add untracked - add contents of untracked files to the staged set of changes\n");
}

const COMMANDS: [&str; 8] = [
    "status",
    "update",
    "revert",
    "add untracked",
    "patch",
    "diff",
    "quit",
    "help",
];

/// `pyn add -i [<paths>]`.
pub fn add_interactive(git: &GitHome, cwd: &Path, paths: &[String]) -> Result<()> {
    let opened = git.open_index()?;
    let session = Session {
        git,
        spec: resolve_specs(git, cwd, paths)?.spec,
    };
    print!("{}", patch_mode::needs_merge(&opened.index));
    session.status()?;
    let mut menu = Items::new(
        COMMANDS
            .iter()
            .map(|c| Row::named(c.as_bytes().to_vec()))
            .collect(),
    );
    let style = Style {
        prompt: "What now",
        header: Some("*** Commands ***"),
        columns: 4,
        mode: Mode::Singleton,
        names_only: false,
        help: command_help,
    };
    loop {
        let chosen = list_and_choose(&mut menu, &style);
        if chosen == QUIT || chosen == 6 {
            say("Bye.\n");
            return Ok(());
        }
        let result = match chosen {
            0 => session.status(),
            1 => session.update(),
            2 => session.revert(),
            3 => session.add_untracked(),
            4 => session.patch(),
            5 => session.diff(),
            7 => {
                help();
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(e) = result {
            eprintln!("error: {e:#}");
        }
    }
}

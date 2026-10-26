//! Sparse checkout: the patterns in `info/sparse-checkout`, the skip-worktree bit they set in the index, and the
//! `sparse-checkout` command. The state is the one git keeps, so git reads what pyn writes and the reverse.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Args, Subcommand};
use pyn_git::{ConfigFile, Index, IndexEntry};
use pyn_gitwork::{Kind, Pathspec, Rule, parse_rules, verdict};

use super::plumbing::{emit_bytes, quote};
use super::stage::{Entries, entries_of, index_of, put_entry};
use super::worktree::{attributes_first, remove_entry_file, write_entry};
use super::{GitHome, convert, path_string};

const GITLINK: u32 = 0o160000;
const DEFAULT_PATTERNS: &str = "/*\n!/*/\n";

/// What `info/sparse-checkout` selects.
pub(super) enum Patterns {
    /// Directory lists: everything below `recursive`, and only the files directly in `parent` and in the root.
    Cone {
        recursive: BTreeSet<Vec<u8>>,
        parent: BTreeSet<Vec<u8>>,
    },
    /// Gitignore-style rules where a match includes the path.
    Rules(Vec<Rule>),
}

fn escape(dir: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(dir.len());
    for b in dir {
        if b"*?[]\\".contains(b) {
            out.push(b'\\');
        }
        out.push(*b);
    }
    out
}

fn unescape(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.iter();
    while let Some(b) = bytes.next() {
        if *b == b'\\' {
            if let Some(n) = bytes.next() {
                out.push(*n);
            }
        } else {
            out.push(*b);
        }
    }
    out
}

fn under_any(set: &BTreeSet<Vec<u8>>, dir: &[u8]) -> bool {
    let mut end = 0;
    loop {
        let stop = dir[end..].iter().position(|b| *b == b'/');
        let prefix = &dir[..stop.map_or(dir.len(), |i| end + i)];
        if set.contains(prefix) {
            return true;
        }
        match stop {
            Some(i) => end += i + 1,
            None => return false,
        }
    }
}

impl Patterns {
    /// Reads a cone file: `/*`, `!/*/`, `/dir/` and `!/dir/*/` lines. `None` when it is not one.
    fn cone(text: &[u8]) -> Option<Self> {
        let mut recursive: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut parent: BTreeSet<Vec<u8>> = BTreeSet::new();
        for line in text.split(|b| *b == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() || line == b"/*" || line == b"!/*/" {
                continue;
            }
            if let Some(dir) = line
                .strip_prefix(b"!/")
                .and_then(|l| l.strip_suffix(b"/*/"))
            {
                let dir = unescape(dir);
                if !recursive.remove(&dir) {
                    return None;
                }
                parent.insert(dir);
            } else {
                let dir = line.strip_prefix(b"/")?.strip_suffix(b"/")?;
                if dir.is_empty() || has_unescaped_glob(dir) {
                    return None;
                }
                let dir = unescape(dir);
                if !under_any(&recursive, &dir) {
                    recursive.insert(dir);
                }
            }
        }
        Some(Self::Cone { recursive, parent })
    }

    pub(super) fn parse(text: &[u8], cone: bool) -> Self {
        if cone && let Some(p) = Self::cone(text) {
            return p;
        }
        Self::Rules(parse_rules(text))
    }

    /// Whether the work tree holds the file `path`.
    pub(super) fn includes(&self, path: &[u8]) -> bool {
        match self {
            Self::Cone { recursive, parent } => {
                let Some(slash) = path.iter().rposition(|b| *b == b'/') else {
                    return true;
                };
                let dir = &path[..slash];
                parent.contains(dir) || under_any(recursive, dir)
            }
            Self::Rules(rules) => {
                let mut state = false;
                let mut end = 0;
                while let Some(i) = path[end..].iter().position(|b| *b == b'/') {
                    end += i;
                    if let Some(hit) = verdict(rules, &path[..end], true, false) {
                        state = hit;
                    }
                    end += 1;
                }
                verdict(rules, path, false, false).unwrap_or(state)
            }
        }
    }

    fn cone_text(recursive: &BTreeSet<Vec<u8>>, parent: &BTreeSet<Vec<u8>>) -> Vec<u8> {
        let mut parents = parent.clone();
        for dir in recursive {
            let mut end = 0;
            while let Some(i) = dir[end..].iter().position(|b| *b == b'/') {
                end += i;
                parents.insert(dir[..end].to_vec());
                end += 1;
            }
        }
        let mut out = DEFAULT_PATTERNS.as_bytes().to_vec();
        for dir in parents.iter().filter(|d| !under_any(recursive, d)) {
            let e = escape(dir);
            for line in [&b"/"[..], &e, b"/\n!/", &e, b"/*/\n"] {
                out.extend_from_slice(line);
            }
        }
        for dir in recursive {
            out.extend_from_slice(b"/");
            out.extend_from_slice(&escape(dir));
            out.extend_from_slice(b"/\n");
        }
        out
    }
}

fn has_unescaped_glob(dir: &[u8]) -> bool {
    let mut bytes = dir.iter();
    while let Some(b) = bytes.next() {
        match b {
            b'\\' => {
                bytes.next();
            }
            b'*' | b'?' | b'[' | b']' => return true,
            _ => {}
        }
    }
    false
}

fn pattern_file(git: &GitHome) -> PathBuf {
    git.repo.git().git_dir().join("info/sparse-checkout")
}

fn worktree_config(git: &GitHome) -> PathBuf {
    git.repo.git().git_dir().join("config.worktree")
}

fn read_patterns_file(git: &GitHome) -> Result<Option<Vec<u8>>> {
    match std::fs::read(pattern_file(git)) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Whether the work tree is sparse (`core.sparseCheckout`).
pub(super) fn enabled(git: &GitHome) -> bool {
    git.repo.bool_setting("core.sparsecheckout", false)
}

fn cone_setting(git: &GitHome) -> bool {
    git.repo.bool_setting("core.sparsecheckoutcone", false)
}

/// The patterns in force, or `None` when the work tree is not sparse.
pub(super) fn load(git: &GitHome) -> Result<Option<Patterns>> {
    if !enabled(git) {
        return Ok(None);
    }
    let Some(text) = read_patterns_file(git)? else {
        return Ok(None);
    };
    Ok(Some(Patterns::parse(&text, cone_setting(git))))
}

/// The refusal for commands that cannot yet keep the skip-worktree bit right.
pub(super) fn refuse_unsupported(index: &Index) -> Result<()> {
    if index.entries.iter().any(|e| e.skip_worktree) {
        bail!(
            "this repository uses sparse checkout, which pyn does not support in this command yet"
        );
    }
    Ok(())
}

/// The share of tracked files present in the work tree, as `status` words it; `None` when not sparse.
pub(super) fn percentage(git: &GitHome, index: &Index) -> Option<usize> {
    if !enabled(git) {
        return None;
    }
    let total = index.entries.iter().filter(|e| e.stage == 0).count();
    let skipped = index
        .entries
        .iter()
        .filter(|e| e.stage == 0 && e.skip_worktree)
        .count();
    Some(if total == 0 {
        100
    } else {
        (100 * (total - skipped)).div_ceil(total)
    })
}

/// Marks entries new to `index` that lie outside the patterns, and keeps the mark on entries that had it.
pub(super) fn carry_over(git: &GitHome, old: &Index, new: &mut Index) -> Result<()> {
    let patterns = load(git)?;
    if patterns.is_none() && !old.entries.iter().any(|e| e.skip_worktree) {
        return Ok(());
    }
    let skipped: BTreeSet<&[u8]> = old
        .entries
        .iter()
        .filter(|e| e.skip_worktree)
        .map(|e| e.path.as_slice())
        .collect();
    let known: BTreeSet<&[u8]> = old.entries.iter().map(|e| e.path.as_slice()).collect();
    for e in new.entries.iter_mut().filter(|e| e.stage == 0) {
        if e.mode == GITLINK {
            continue;
        }
        e.skip_worktree = skipped.contains(e.path.as_slice())
            || (!known.contains(e.path.as_slice())
                && patterns.as_ref().is_some_and(|p| !p.includes(&e.path)));
    }
    Ok(())
}

fn write_config(git: &GitHome, sparse: bool, cone: bool) -> Result<()> {
    let common = git.repo.git().common_dir().join("config");
    let mut shared = ConfigFile::read(&common)?;
    shared.set("extensions", None, "worktreeConfig", "true")?;
    shared.write(&common)?;
    let path = worktree_config(git);
    let mut own = if path.exists() {
        ConfigFile::read(&path)?
    } else {
        ConfigFile::parse("")?
    };
    own.set("core", None, "sparseCheckout", &sparse.to_string())?;
    own.set("core", None, "sparseCheckoutCone", &cone.to_string())?;
    own.write(&path)?;
    Ok(())
}

fn write_patterns(git: &GitHome, text: &[u8]) -> Result<()> {
    let path = pattern_file(git);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)?;
    Ok(())
}

/// Brings the index and work tree in line with `patterns` (`None` makes every file present). Files with local
/// changes outside the patterns are left, with a warning.
pub(super) fn reconcile(git: &GitHome, patterns: Option<&Patterns>) -> Result<()> {
    let opened = git.open_index()?;
    let dirty = git.repo.unstaged_changes(&opened.index)?;
    let root = git.repo.root()?;
    let mut entries: Entries = entries_of(&opened.index);
    let mut left: Vec<Vec<u8>> = Vec::new();
    let mut in_way: Vec<Vec<u8>> = Vec::new();
    let mut writes: Vec<(Vec<u8>, pyn_gitwork::Entry)> = Vec::new();
    let mut hide: Vec<Vec<u8>> = Vec::new();
    for e in &opened.index.entries {
        if e.stage != 0 || e.mode == GITLINK {
            continue;
        }
        let want = patterns.is_none_or(|p| p.includes(&e.path));
        if want && e.skip_worktree {
            if std::fs::symlink_metadata(pyn_gitwork::fs_path(root, &e.path)).is_ok() {
                in_way.push(e.path.clone());
            } else {
                let mode = super::stage::leaf_mode(e.mode);
                writes.push((e.path.clone(), pyn_gitwork::Entry { mode, id: e.id }));
            }
        } else if !want && !e.skip_worktree {
            let changed = matches!(dirty.get(&e.path), Some(Kind::Modified | Kind::TypeChange));
            if changed || e.intent_to_add {
                left.push(e.path.clone());
            } else {
                hide.push(e.path.clone());
            }
        }
    }
    if !in_way.is_empty() {
        eprintln!(
            "error: The following untracked working tree files would be overwritten by the sparse-checkout update:"
        );
        for p in &in_way {
            eprintln!("\t{}", path_string(p));
        }
        bail!("Please move or remove them before you update the sparse-checkout.");
    }
    super::worktree::check_writes(writes.iter().map(|(p, _)| p))?;
    convert::refuse_unsupported(git, writes.iter().map(|(p, e)| (p.as_slice(), e.mode)))?;
    for path in &hide {
        remove_entry_file(git, path)?;
        if let Some(e) = entries.get_mut(&(path.clone(), 0)) {
            e.skip_worktree = true;
        }
    }
    let by_path: BTreeMap<&Vec<u8>, &pyn_gitwork::Entry> =
        writes.iter().map(|(p, e)| (p, e)).collect();
    for (path, entry) in attributes_first(by_path.iter().map(|(p, e)| (*p, *e))) {
        let written = write_entry(git, path, entry)?;
        put_entry(&mut entries, written);
    }
    let index = index_of(opened.index.version, entries);
    git.save_index(&opened, &index)?;
    if !left.is_empty() {
        eprintln!(
            "warning: The following paths are not up to date and were left despite sparse patterns:"
        );
        for p in &left {
            eprintln!("\t{}", path_string(p));
        }
        eprintln!();
        eprintln!(
            "After fixing the above paths, you may want to run `pyn sparse-checkout reapply`."
        );
    }
    Ok(())
}

/// Turns sparse checkout on with `text` as its patterns (the root files only when `None`) and applies it.
fn enable(git: &GitHome, cone: bool, text: Option<Vec<u8>>) -> Result<()> {
    let text = match text {
        Some(t) => t,
        None => read_patterns_file(git)?.unwrap_or_else(|| DEFAULT_PATTERNS.into()),
    };
    write_config(git, true, cone)?;
    write_patterns(git, &text)?;
    reconcile(git, Some(&Patterns::parse(&text, cone)))
}

/// Sets sparse checkout up as `clone --sparse` does: the root files only, in cone mode.
pub(super) fn init_for_clone(git: &GitHome) -> Result<()> {
    write_config(git, true, true)?;
    write_patterns(git, DEFAULT_PATTERNS.as_bytes())
}

/// Manage the sparse-checkout patterns and the files they leave out of the work tree.
#[derive(Debug, Args)]
pub struct SparseCheckoutArgs {
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Args)]
struct ModeFlags {
    /// Use directory lists (the default).
    #[arg(long, overrides_with = "no_cone")]
    cone: bool,
    /// Use gitignore-style patterns.
    #[arg(long, overrides_with = "cone")]
    no_cone: bool,
    /// Not supported: pyn keeps the full index.
    #[arg(long, overrides_with = "no_sparse_index")]
    sparse_index: bool,
    /// Keep the full index (the only form pyn has).
    #[arg(long, overrides_with = "sparse_index")]
    no_sparse_index: bool,
}

impl ModeFlags {
    fn given(&self) -> Option<bool> {
        if self.cone {
            Some(true)
        } else if self.no_cone {
            Some(false)
        } else {
            None
        }
    }

    fn check(&self) -> Result<()> {
        if self.sparse_index {
            bail!("--sparse-index is not supported: pyn keeps the full index");
        }
        Ok(())
    }
}

#[derive(Debug, Args)]
struct PatternArgs {
    #[command(flatten)]
    mode: ModeFlags,
    /// Take the directories or patterns from standard input, one per line.
    #[arg(long)]
    stdin: bool,
    /// Accept patterns and paths that are not directories.
    #[arg(long)]
    skip_checks: bool,
    /// Directories (cone mode) or patterns.
    paths: Vec<String>,
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[command(flatten)]
    mode: ModeFlags,
    /// Read the paths NUL-separated and print them unquoted.
    #[arg(short = 'z')]
    nul: bool,
    /// Test against the rules in this file instead of the current ones.
    #[arg(long, value_name = "FILE")]
    rules_file: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Turn sparse checkout on, keeping the patterns if there are any (the root files only otherwise).
    Init(ModeArgs),
    /// Replace the patterns.
    Set(PatternArgs),
    /// Add to the patterns.
    Add(PatternArgs),
    /// Show the directories (cone mode) or patterns.
    List,
    /// Apply the patterns again to the index and work tree.
    Reapply(ModeArgs),
    /// Turn sparse checkout off and restore every file.
    Disable,
    /// Print the paths read from standard input that the patterns include.
    CheckRules(CheckArgs),
}

#[derive(Debug, Args)]
struct ModeArgs {
    #[command(flatten)]
    mode: ModeFlags,
}

fn fatal(message: impl std::fmt::Display) -> ! {
    eprintln!("fatal: {message}");
    std::process::exit(128);
}

pub(super) fn sparse_checkout(git: &GitHome, args: SparseCheckoutArgs) -> Result<()> {
    match args.action {
        Action::Init(a) => {
            a.mode.check()?;
            let cone = a.mode.given().unwrap_or(true);
            enable(git, cone, None)
        }
        Action::Set(a) => set(git, a, false),
        Action::Add(a) => set(git, a, true),
        Action::List => list(git),
        Action::Reapply(a) => {
            a.mode.check()?;
            if !enabled(git) {
                fatal("must be in a sparse-checkout to reapply sparsity patterns");
            }
            let cone = a.mode.given().unwrap_or_else(|| cone_setting(git));
            let text = read_patterns_file(git)?.unwrap_or_default();
            write_config(git, true, cone)?;
            reconcile(git, Some(&Patterns::parse(&text, cone)))
        }
        Action::Disable => {
            let cone = cone_setting(git);
            write_config(git, false, cone)?;
            reconcile(git, None)?;
            write_config(git, false, false)
        }
        Action::CheckRules(a) => check_rules(git, a),
    }
}

fn list(git: &GitHome) -> Result<()> {
    if !enabled(git) {
        fatal("this worktree is not sparse");
    }
    let text = read_patterns_file(git)?.unwrap_or_default();
    match Patterns::parse(&text, cone_setting(git)) {
        Patterns::Cone { recursive, .. } => {
            let mut out = Vec::new();
            for dir in recursive {
                out.extend_from_slice(quote(git, &dir).as_bytes());
                out.push(b'\n');
            }
            emit_bytes(&out);
        }
        Patterns::Rules(_) => emit_bytes(&text),
    }
    Ok(())
}

fn lines_of(args: &PatternArgs) -> Result<Vec<String>> {
    let mut out = args.paths.clone();
    if args.stdin {
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text)?;
        out.extend(text.lines().map(str::to_owned));
    }
    Ok(out)
}

fn cone_dir(index: &Index, arg: &str, skip: bool) -> Result<Vec<u8>> {
    if !skip {
        if arg.starts_with('/') {
            fatal("specify directories rather than patterns (no leading slash)");
        }
        if arg.starts_with('!') {
            fatal(
                "specify directories rather than patterns.  If your directory really starts with a '!', pass --skip-checks",
            );
        }
        if arg.bytes().any(|b| b"*?[]\\".contains(&b)) {
            fatal(
                "specify directories rather than patterns.  If your directory really has any of '*?[]\\' in it, pass --skip-checks",
            );
        }
    }
    let dir = arg.trim_end_matches('/').as_bytes().to_vec();
    if !skip && index.entries.iter().any(|e| e.path == dir) {
        fatal(format!(
            "'{arg}' is not a directory; to treat it as a directory anyway, rerun with --skip-checks"
        ));
    }
    Ok(dir)
}

fn set(git: &GitHome, args: PatternArgs, add: bool) -> Result<()> {
    args.mode.check()?;
    let was_on = enabled(git);
    if add && !was_on {
        fatal("no sparse-checkout to add to");
    }
    let cone = args
        .mode
        .given()
        .unwrap_or_else(|| if was_on { cone_setting(git) } else { true });
    let given = lines_of(&args)?;
    let existing = if add {
        read_patterns_file(git)?.unwrap_or_default()
    } else {
        Vec::new()
    };
    let text = if cone {
        let index = git.open_index()?.index;
        let (mut recursive, mut parent) = match Patterns::cone(&existing) {
            Some(Patterns::Cone { recursive, parent }) => (recursive, parent),
            _ => (BTreeSet::new(), BTreeSet::new()),
        };
        for arg in &given {
            let dir = cone_dir(&index, arg, args.skip_checks)?;
            if dir.is_empty() || under_any(&recursive, &dir) {
                continue;
            }
            recursive.retain(|r| !(r.starts_with(&dir) && r.get(dir.len()) == Some(&b'/')));
            parent.remove(&dir);
            recursive.insert(dir);
        }
        Patterns::cone_text(&recursive, &parent)
    } else {
        if !args.skip_checks {
            for line in &given {
                if !line.starts_with('/') && !line.contains(['/', '*', '?', '[']) {
                    eprintln!(
                        "warning: pass a leading slash before paths such as '{line}' if you want a single file (see NON-CONE PROBLEMS in the git-sparse-checkout manual)."
                    );
                }
            }
        }
        let mut text = existing;
        if !text.is_empty() && !text.ends_with(b"\n") {
            text.push(b'\n');
        }
        for line in &given {
            text.extend_from_slice(line.as_bytes());
            text.push(b'\n');
        }
        text
    };
    enable(git, cone, Some(text))
}

fn check_rules(git: &GitHome, args: CheckArgs) -> Result<()> {
    args.mode.check()?;
    let cone = args.mode.given().unwrap_or_else(|| cone_setting(git));
    let patterns = match &args.rules_file {
        Some(path) => Patterns::parse(&std::fs::read(path)?, cone),
        None => match load(git)? {
            Some(p) => p,
            None => fatal("unable to load existing sparse-checkout patterns"),
        },
    };
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let sep = if args.nul { 0 } else { b'\n' };
    let mut out = Vec::new();
    for path in input.split(|b| *b == sep).filter(|p| !p.is_empty()) {
        let path = if args.nul {
            path.to_vec()
        } else {
            unquote(path)
        };
        if patterns.includes(&path) {
            if args.nul {
                out.extend_from_slice(&path);
                out.push(0);
            } else {
                out.extend_from_slice(quote(git, &path).as_bytes());
                out.push(b'\n');
            }
        }
    }
    emit_bytes(&out);
    Ok(())
}

/// A path as git's quoted form, back to bytes; unquoted text is returned as is.
fn unquote(text: &[u8]) -> Vec<u8> {
    let Some(inner) = text.strip_prefix(b"\"").and_then(|t| t.strip_suffix(b"\"")) else {
        return text.to_vec();
    };
    let mut out = Vec::new();
    let mut i = 0;
    while i < inner.len() {
        if inner[i] != b'\\' || i + 1 >= inner.len() {
            out.push(inner[i]);
            i += 1;
            continue;
        }
        i += 1;
        match inner[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'0'..=b'3' if inner[i..].len() >= 3 => {
                let v = inner[i..i + 3]
                    .iter()
                    .fold(0u32, |a, d| a * 8 + u32::from(d - b'0'));
                out.push(v as u8);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

/// The tag `ls-files -t` shows for a stage 0 entry.
pub(super) fn tag(e: &IndexEntry) -> char {
    if e.stage > 0 {
        'M'
    } else if e.skip_worktree {
        'S'
    } else {
        'H'
    }
}

/// The pathspecs, as typed, that select skip-worktree entries and nothing else.
pub(super) fn outside_only(spec: &Pathspec, index: &Index, others: &[&[u8]]) -> Vec<String> {
    (0..spec.len())
        .filter(|n| !spec.item_is_exclude(*n))
        .filter(|n| {
            let hits = |skip: bool| {
                index
                    .entries
                    .iter()
                    .filter(|e| e.skip_worktree == skip)
                    .map(|e| e.path.as_slice())
                    .chain(others.iter().copied().filter(|_| !skip))
                    .any(|p| spec.item_match(*n, p).is_some())
            };
            hits(true) && !hits(false)
        })
        .map(|n| String::from_utf8_lossy(spec.original(n)).into_owned())
        .collect()
}

/// Prints git's refusal for pathspecs that only reach paths outside the patterns; true when there were any.
pub(super) fn refuse_outside(outside: &[String]) -> bool {
    if outside.is_empty() {
        return false;
    }
    eprintln!("The following paths and/or pathspecs matched paths that exist");
    eprintln!("outside of your sparse-checkout definition, so will not be");
    eprintln!("updated in the index:");
    for p in outside {
        eprintln!("{p}");
    }
    eprintln!("hint: If you intend to update such entries, try one of the following:");
    eprintln!("hint: * Use the --sparse option.");
    eprintln!("hint: * Disable or modify the sparsity rules.");
    eprintln!("hint: Disable this message with \"git config advice.updateSparsePath false\"");
    true
}

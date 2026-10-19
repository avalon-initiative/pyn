//! `pyn log` on a git repository: revision ranges, formats, filters and diff output, in git's shapes.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use chrono::Local;
use pyn_gitwork::{Head, Pathspec, fs_path};
use pyn_object::{Commit, ObjectId};
use regex::Regex;

use super::GitHome;
use super::commands::{add_changes, emit, head_or_refuse, message_lines};
use super::log_match::{compile, parse_date};
use super::log_text::{Context, DateStyle, Decorations, Pretty, render};
use crate::diff::{self, Kind, Tree};
use crate::{table, time};

/// The git-only arguments of `pyn log`; a pyn server refuses them all.
#[derive(clap::Args, Debug, Default)]
pub struct GitLogArgs {
    /// More revisions (`<a>..<b>`, `<a>...<b>`, `^<rev>`), then paths.
    #[arg(value_name = "REV_OR_PATH")]
    more: Vec<String>,
    /// Paths, after `--`.
    #[arg(last = true, value_name = "PATH")]
    dashed: Vec<String>,
    /// One line per commit: the abbreviated id and the subject.
    #[arg(long)]
    oneline: bool,
    /// `oneline`, `short`, `medium`, `full`, `fuller`, `raw`, or a template such as `%h %s`.
    #[arg(long, value_name = "FORMAT", require_equals = true)]
    format: Option<String>,
    /// Like `--format`; without a value it is `medium`.
    #[arg(
        long,
        value_name = "FORMAT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "medium"
    )]
    pretty: Option<String>,
    /// `default`, `iso`, `iso-strict`, `rfc`, `short`, `relative`, `unix` or `raw`.
    #[arg(long, value_name = "STYLE")]
    date: Option<String>,
    /// Only commits whose author matches the regular expression; repeat for any of several.
    #[arg(long, value_name = "PATTERN")]
    author: Vec<String>,
    /// Only commits whose committer matches the regular expression; repeat for any of several.
    #[arg(long, value_name = "PATTERN")]
    committer: Vec<String>,
    /// Only commits whose message matches the regular expression; repeat for any of several.
    #[arg(long, value_name = "PATTERN")]
    grep: Vec<String>,
    /// Match `--author`, `--committer` and `--grep` ignoring case.
    #[arg(short = 'i', long = "regexp-ignore-case")]
    ignore_case: bool,
    /// Only commits whose message matches none of the `--grep` patterns.
    #[arg(long)]
    invert_grep: bool,
    /// Only commits whose message matches every `--grep` pattern.
    #[arg(long)]
    all_match: bool,
    /// Only commits committed after this date.
    #[arg(long, alias = "after", value_name = "DATE")]
    since: Option<String>,
    /// Only commits committed before this date.
    #[arg(long, alias = "before", value_name = "DATE")]
    until: Option<String>,
    /// Leave out commits with more than one parent.
    #[arg(long, conflicts_with = "merges")]
    no_merges: bool,
    /// Only commits with more than one parent.
    #[arg(long)]
    merges: bool,
    /// Skip this many commits first.
    #[arg(long, value_name = "N")]
    skip: Option<usize>,
    /// Print the selected commits oldest first.
    #[arg(long)]
    reverse: bool,
    /// A diffstat after each commit.
    #[arg(long, conflicts_with_all = ["shortstat", "name_only", "name_status"])]
    stat: bool,
    /// Only the summary line of the diffstat.
    #[arg(long, conflicts_with_all = ["name_only", "name_status"])]
    shortstat: bool,
    /// The names of the changed files.
    #[arg(long, conflicts_with_all = ["name_status", "patch", "unified"])]
    name_only: bool,
    /// The names and the kind of change (A, M, D) of the changed files.
    #[arg(long, conflicts_with_all = ["patch", "unified"])]
    name_status: bool,
    /// The patch of each commit.
    #[arg(short = 'p', short_alias = 'u', long)]
    patch: bool,
    /// Lines of context in the patch (implies `-p`).
    #[arg(short = 'U', long, value_name = "N")]
    unified: Option<usize>,
}

impl GitLogArgs {
    /// The first argument given that a pyn server cannot take; one path after `--` is the path argument there.
    pub fn first_given(&self) -> Option<&'static str> {
        let flags = [
            ("<revision>", !self.more.is_empty()),
            ("--", self.dashed.len() > 1),
            ("--oneline", self.oneline),
            ("--format", self.format.is_some()),
            ("--pretty", self.pretty.is_some()),
            ("--date", self.date.is_some()),
            ("--author", !self.author.is_empty()),
            ("--committer", !self.committer.is_empty()),
            ("--grep", !self.grep.is_empty()),
            ("-i", self.ignore_case),
            ("--invert-grep", self.invert_grep),
            ("--all-match", self.all_match),
            ("--since", self.since.is_some()),
            ("--until", self.until.is_some()),
            ("--no-merges", self.no_merges),
            ("--merges", self.merges),
            ("--skip", self.skip.is_some()),
            ("--reverse", self.reverse),
            ("--stat", self.stat),
            ("--shortstat", self.shortstat),
            ("--name-only", self.name_only),
            ("--name-status", self.name_status),
            ("-p", self.patch),
            ("-U", self.unified.is_some()),
        ];
        flags.into_iter().find(|(_, on)| *on).map(|(f, _)| f)
    }

    /// The single path given after `--`, which a pyn server reads as its path argument.
    pub fn sole_dashed(&self) -> Option<&str> {
        match self.dashed.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }
}

/// What a revision argument selects.
enum Rev {
    Include(ObjectId),
    Hide(ObjectId),
    Range(ObjectId, ObjectId),
    Symmetric(ObjectId, ObjectId),
}

fn parse_rev(git: &GitHome, arg: &str) -> Result<Option<Rev>> {
    let head = || head_or_refuse(git);
    let side = |s: &str| -> Result<Option<ObjectId>> {
        if s.is_empty() {
            return head().map(Some);
        }
        Ok(git.repo.resolve(s).ok())
    };
    if arg.bytes().all(|b| b == b'.') {
        return Ok(None);
    }
    if let Some(rest) = arg.strip_prefix('^') {
        return Ok(git.repo.resolve(rest).ok().map(Rev::Hide));
    }
    if let Some((a, b)) = arg.split_once("...") {
        return Ok(side(a)?.zip(side(b)?).map(|(a, b)| Rev::Symmetric(a, b)));
    }
    if let Some((a, b)) = arg.split_once("..") {
        return Ok(side(a)?.zip(side(b)?).map(|(a, b)| Rev::Range(a, b)));
    }
    Ok(git.repo.resolve(arg).ok().map(Rev::Include))
}

struct Selection {
    include: Vec<ObjectId>,
    hidden: HashSet<ObjectId>,
    paths: Vec<String>,
}

fn on_disk(git: &GitHome, rel: &str) -> bool {
    git.repo
        .root()
        .is_ok_and(|root| std::fs::symlink_metadata(fs_path(root, rel.as_bytes())).is_ok())
}

fn in_head(git: &GitHome, rel: &str) -> Result<bool> {
    let Some(head) = git.repo.head_commit()? else {
        return Ok(false);
    };
    let tree = git.repo.commit(&head)?.tree;
    Ok(git.repo.entry_at(&tree, rel.as_bytes())?.is_some())
}

/// Revisions up to the first argument that is not one, then paths; `--` ends the revisions explicitly.
fn select(git: &GitHome, cwd: &Path, args: Vec<String>, dashed: Vec<String>) -> Result<Selection> {
    let explicit = !dashed.is_empty();
    let mut revs = Vec::new();
    let mut paths = Vec::new();
    for arg in args {
        if !paths.is_empty() {
            paths.push(arg);
            continue;
        }
        match parse_rev(git, &arg)? {
            Some(rev) => {
                if !explicit && std::fs::symlink_metadata(cwd.join(&arg)).is_ok() {
                    bail!(
                        "ambiguous argument '{arg}': both revision and filename; \
                         use '--' to separate paths from revisions"
                    );
                }
                revs.push(rev);
            }
            None if explicit => bail!("bad revision '{arg}'"),
            None => {
                let rel = git.ws.repo_prefix(cwd, &arg)?;
                if !on_disk(git, &rel) && !in_head(git, &rel)? {
                    bail!(
                        "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
                         Use '--' to separate paths from revisions"
                    );
                }
                paths.push(arg);
            }
        }
    }
    paths.extend(dashed);
    let given = !revs.is_empty();
    let (mut include, mut hidden) = (Vec::new(), HashSet::new());
    for rev in revs {
        match rev {
            Rev::Include(id) => include.push(id),
            Rev::Hide(id) => hidden.extend(git.reachable(&id)?),
            Rev::Range(a, b) => {
                hidden.extend(git.reachable(&a)?);
                include.push(b);
            }
            Rev::Symmetric(a, b) => {
                let (from_a, from_b) = (git.reachable(&a)?, git.reachable(&b)?);
                hidden.extend(from_a.intersection(&from_b).copied());
                include.extend([a, b]);
            }
        }
    }
    if !given {
        include.push(head_or_refuse(git)?);
    }
    let paths = paths
        .iter()
        .map(|p| git.ws.repo_prefix(cwd, p))
        .collect::<Result<Vec<_>>>()?;
    Ok(Selection {
        include,
        hidden,
        paths,
    })
}

struct Filters {
    authors: Vec<Regex>,
    committers: Vec<Regex>,
    greps: Vec<Regex>,
    all_match: bool,
    invert: bool,
    merges: Option<bool>,
    since: Option<i64>,
    until: Option<i64>,
}

impl Filters {
    fn new(a: &GitLogArgs) -> Result<Self> {
        let all = |patterns: &[String]| -> Result<Vec<Regex>> {
            patterns.iter().map(|p| compile(p, a.ignore_case)).collect()
        };
        let now = Local::now();
        let date = |d: &Option<String>| d.as_deref().map(|d| parse_date(d, now)).transpose();
        Ok(Self {
            authors: all(&a.author)?,
            committers: all(&a.committer)?,
            greps: all(&a.grep)?,
            all_match: a.all_match,
            invert: a.invert_grep,
            merges: (a.merges || a.no_merges).then_some(a.merges),
            since: date(&a.since)?,
            until: date(&a.until)?,
        })
    }

    fn accepts(&self, c: &Commit) -> bool {
        if self
            .merges
            .is_some_and(|only_merges| (c.parents.len() > 1) != only_merges)
        {
            return false;
        }
        if self.since.is_some_and(|t| c.committer.time < t)
            || self.until.is_some_and(|t| c.committer.time > t)
        {
            return false;
        }
        let ident = |s: &pyn_object::Signature| {
            format!(
                "{} <{}>",
                String::from_utf8_lossy(&s.name),
                String::from_utf8_lossy(&s.email)
            )
        };
        let any = |patterns: &[Regex], text: &str| {
            patterns.is_empty() || patterns.iter().any(|p| p.is_match(text))
        };
        if !any(&self.authors, &ident(&c.author)) || !any(&self.committers, &ident(&c.committer)) {
            return false;
        }
        if self.greps.is_empty() {
            return true;
        }
        let message = String::from_utf8_lossy(&c.message);
        let hit = |p: &Regex| p.is_match(&message);
        let matched = if self.all_match {
            self.greps.iter().all(hit)
        } else {
            self.greps.iter().any(hit)
        };
        matched != self.invert
    }
}

/// The diff sections to print after each commit.
struct DiffParts {
    names: Option<Kind>,
    stat: bool,
    shortstat: bool,
    patch: bool,
    context: usize,
}

impl DiffParts {
    fn new(a: &GitLogArgs) -> Self {
        Self {
            names: if a.name_only {
                Some(Kind::NameOnly)
            } else if a.name_status {
                Some(Kind::NameStatus)
            } else {
                None
            },
            stat: a.stat,
            shortstat: a.shortstat,
            patch: a.patch || a.unified.is_some(),
            context: a.unified.unwrap_or(3),
        }
    }

    fn any(&self) -> bool {
        self.names.is_some() || self.stat || self.shortstat || self.patch
    }
}

/// The diff of a non-merge commit against its parent, limited to `spec`; merges show none, as in git.
fn diff_body(
    git: &GitHome,
    c: &Commit,
    spec: &Pathspec,
    parts: &DiffParts,
) -> Result<Option<(String, bool)>> {
    if c.parents.len() > 1 {
        return Ok(None);
    }
    let parent_tree = match c.parents.first() {
        Some(p) => Some(git.repo.commit(p)?.tree),
        None => None,
    };
    let (mut old, mut new) = (Tree::new(), Tree::new());
    let changes = git
        .repo
        .diff_trees(parent_tree.as_ref(), Some(&c.tree), spec)?;
    if changes.is_empty() {
        return Ok(None);
    }
    add_changes(&mut old, &mut new, &changes);
    let section = |kind: Kind| -> Result<String> {
        let mut out = String::new();
        let format = diff::Format {
            context: parts.context,
            kind,
        };
        diff::compare(git, &old, &new, &[], &format, &mut out)?;
        Ok(out)
    };
    let mut summary = String::new();
    if let Some(kind) = parts.names {
        summary.push_str(&section(kind)?);
    }
    if parts.stat {
        summary.push_str(&section(Kind::Stat)?);
    }
    if parts.shortstat {
        summary.push_str(&section(Kind::ShortStat)?);
    }
    let both = parts.stat && parts.patch;
    let mut body = summary;
    if parts.patch {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&section(Kind::Patch)?);
    }
    Ok(Some((body, both)))
}

fn decorations(git: &GitHome) -> Result<Decorations> {
    let head = git.repo.head()?;
    let current = match &head {
        Head::Branch { name, .. } => Some(format!("refs/heads/{name}")),
        Head::Detached(_) => None,
    };
    let mut map = Decorations::new();
    let mut listed = git.repo.refs().list("refs/")?;
    listed.reverse();
    for (name, _) in listed {
        if current.as_deref() == Some(name.as_str()) {
            continue;
        }
        let Ok(id) = git.repo.resolve(&name) else {
            continue;
        };
        let label = if let Some(b) = name.strip_prefix("refs/heads/") {
            b.to_string()
        } else if let Some(r) = name.strip_prefix("refs/remotes/") {
            r.to_string()
        } else if let Some(t) = name.strip_prefix("refs/tags/") {
            format!("tag: {t}")
        } else {
            name
        };
        map.entry(id).or_default().push(label);
    }
    let first = match (&head, head.id()) {
        (Head::Branch { name, .. }, Some(id)) => Some((id, format!("HEAD -> {name}"))),
        (Head::Detached(id), _) => Some((*id, "HEAD".to_string())),
        _ => None,
    };
    if let Some((id, label)) = first {
        map.entry(id).or_default().insert(0, label);
    }
    Ok(map)
}

pub(super) fn log(
    git: &GitHome,
    cwd: &Path,
    path: Option<String>,
    limit: Option<usize>,
    args: GitLogArgs,
) -> Result<()> {
    let mut pretty = match (args.format.as_ref().or(args.pretty.as_ref()), args.oneline) {
        (Some(f), _) => Pretty::parse(f)?,
        (None, true) => Pretty::Oneline { abbrev: true },
        (None, false) => Pretty::Medium,
    };
    if args.oneline
        && let Pretty::Oneline { abbrev } = &mut pretty
    {
        *abbrev = true;
    }
    let style = match &args.date {
        Some(d) => DateStyle::parse(d)?,
        None => DateStyle::Default,
    };
    let filters = Filters::new(&args)?;
    let parts = DiffParts::new(&args);
    let positional: Vec<String> = path.into_iter().chain(args.more.iter().cloned()).collect();
    let selection = select(git, cwd, positional, args.dashed.clone())?;
    let spec = Pathspec::new(selection.paths.iter().map(|p| p.as_bytes().to_vec()));
    let decorated = if pretty.uses_decorations() {
        decorations(git)?
    } else {
        Decorations::new()
    };
    let ctx = Context {
        style,
        now: chrono::Utc::now().timestamp(),
        decorations: &decorated,
    };

    let mut walk = git
        .repo
        .log_hiding(&selection.include, selection.hidden, spec.clone())?;
    let mut skip = args.skip.unwrap_or(0);
    let mut chosen = Vec::new();
    let mut out = String::new();
    let mut shown = 0usize;
    while limit.is_none_or(|n| chosen.len() + shown < n)
        && let Some(entry) = walk.next_entry()?
    {
        if !filters.accepts(&entry.commit) {
            continue;
        }
        if skip > 0 {
            skip -= 1;
            continue;
        }
        if args.reverse {
            chosen.push(entry);
            continue;
        }
        entry_text(git, &pretty, &ctx, &spec, &parts, shown, &entry, &mut out)?;
        shown += 1;
        if out.len() > 64 * 1024 {
            emit(&std::mem::take(&mut out));
        }
    }
    for entry in chosen.iter().rev() {
        entry_text(git, &pretty, &ctx, &spec, &parts, shown, entry, &mut out)?;
        shown += 1;
        if out.len() > 64 * 1024 {
            emit(&std::mem::take(&mut out));
        }
    }
    emit(&out);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn entry_text(
    git: &GitHome,
    pretty: &Pretty,
    ctx: &Context<'_>,
    spec: &Pathspec,
    parts: &DiffParts,
    index: usize,
    entry: &pyn_gitwork::LogEntry,
    out: &mut String,
) -> Result<()> {
    if index > 0 && !pretty.terminated() {
        out.push('\n');
    }
    out.push_str(&render(pretty, ctx, &entry.id, &entry.commit));
    out.push_str(pretty.terminator());
    if parts.any()
        && let Some((body, dashes)) = diff_body(git, &entry.commit, spec, parts)?
    {
        if pretty.blank_before_diff() {
            out.push_str(if dashes { "---\n" } else { "\n" });
        }
        out.push_str(&body);
    }
    Ok(())
}

pub(super) fn log_table(git: &GitHome, limit: usize) -> Result<()> {
    let mut walk = git.repo.log(&[head_or_refuse(git)?], Pathspec::default())?;
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

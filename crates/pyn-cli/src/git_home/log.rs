//! `pyn log` on a git repository: revision ranges, formats, filters and diff output, in git's shapes.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use chrono::Local;
use pyn_gitwork::{Pathspec, fs_path};
use pyn_object::{Commit, ObjectId};
use regex::Regex;

use super::GitHome;
use super::commands::{add_changes, add_unchanged, emit, head_or_refuse, message_lines};
use super::decorate::{DecorateArgs, with_decoration};
use super::log_gate::{Gate, Pickaxe};
use super::log_graph::{Graph, topo_order};
use super::log_match::{Dialect, compile_as, compile_ere, parse_date};
use super::merge_diff::{Against, Body, MergeArgs, Plan, Shape, Want, with_from};
use crate::commit_format::{Context, FormatArgs, Pretty, Rendering, render};
use crate::diff::{self, DiffParts, Kind, Tree};
use crate::{table, time};

/// The git-only arguments of `pyn log`, and the diff flags a pyn server takes too.
#[derive(clap::Args, Debug, Default)]
pub struct GitLogArgs {
    /// More revisions (`<a>..<b>`, `<a>...<b>`, `^<rev>`), then paths.
    #[arg(value_name = "REV_OR_PATH")]
    more: Vec<String>,
    /// Paths, after `--`.
    #[arg(last = true, value_name = "PATH")]
    dashed: Vec<String>,
    #[command(flatten)]
    fmt: FormatArgs,
    #[command(flatten)]
    renames: crate::rename::Flags,
    /// Read the patterns of `--author`, `--committer` and `--grep` as extended regular expressions.
    #[arg(short = 'E', long, overrides_with_all = ["fixed_strings", "basic_regexp", "perl_regexp"])]
    extended_regexp: bool,
    /// Read them as fixed strings.
    #[arg(short = 'F', long, overrides_with_all = ["extended_regexp", "basic_regexp", "perl_regexp"])]
    fixed_strings: bool,
    /// Read them as basic regular expressions (the default).
    #[arg(long, overrides_with_all = ["extended_regexp", "fixed_strings", "perl_regexp"])]
    basic_regexp: bool,
    /// Read them as Perl-compatible regular expressions (refused: no such engine is built in).
    #[arg(short = 'P', long, overrides_with_all = ["extended_regexp", "fixed_strings", "basic_regexp"])]
    perl_regexp: bool,
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
    /// Only commits that change the number of occurrences of this text.
    #[arg(short = 'S', value_name = "STRING")]
    pickaxe_text: Option<String>,
    /// Only commits with a changed line that matches the regular expression.
    #[arg(short = 'G', value_name = "REGEX")]
    pickaxe_lines: Option<String>,
    /// Follow the history of the one path through renames.
    #[arg(long)]
    follow: bool,
    /// Draw the history as a graph in front of each line.
    #[arg(long)]
    graph: bool,
    /// Start from `HEAD` and every ref.
    #[arg(long)]
    all: bool,
    /// Follow only the first parent of each commit.
    #[arg(long)]
    first_parent: bool,
    #[command(flatten)]
    decorate: Box<DecorateArgs>,
    #[command(flatten)]
    merge_diff: MergeArgs,
    /// A diffstat after each commit, optionally `=<width>[,<name-width>[,<count>]]`.
    #[arg(long, value_name = "WIDTHS", num_args = 0..=1, require_equals = true, default_missing_value = "", conflicts_with_all = ["shortstat", "name_only", "name_status"])]
    stat: Option<String>,
    /// Only the summary line of the diffstat.
    #[arg(long, conflicts_with_all = ["name_only", "name_status"])]
    shortstat: bool,
    /// The names of the changed files.
    #[arg(long, conflicts_with_all = ["name_status", "patch", "unified"])]
    name_only: bool,
    /// Show creations, deletions, renames and mode changes.
    #[arg(long)]
    summary: bool,
    /// The names and the kind of change (A, M, D) of the changed files.
    #[arg(long, conflicts_with_all = ["patch", "unified"])]
    name_status: bool,
    /// The patch of each commit.
    #[arg(short = 'p', short_alias = 'u', long)]
    patch: bool,
    /// Lines of context in the patch (implies `-p`).
    #[arg(short = 'U', long, value_name = "N")]
    unified: Option<usize>,
    /// The modes, ids and status letter of each change.
    #[arg(long, conflicts_with_all = ["name_only", "name_status", "stat", "shortstat"])]
    raw: bool,
    #[command(flatten)]
    patch_flags: crate::diff_opts::PatchFlags,
}

impl GitLogArgs {
    /// The first argument given that a pyn server cannot take; one path after `--` is the path argument there.
    pub fn first_given(&self) -> Option<&'static str> {
        let flags = [
            ("<revision>", !self.more.is_empty()),
            ("--", self.dashed.len() > 1),
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
            ("--all", self.all),
            ("--first-parent", self.first_parent),
            ("--graph", self.graph),
            ("--follow", self.follow),
            ("-S", self.pickaxe_text.is_some()),
            ("-G", self.pickaxe_lines.is_some()),
            ("-E", self.extended_regexp),
            ("-F", self.fixed_strings),
            ("--basic-regexp", self.basic_regexp),
            ("-P", self.perl_regexp),
            ("--raw", self.raw),
            ("-w", self.patch_flags.all_space()),
        ];
        flags
            .into_iter()
            .chain(self.decorate.given())
            .chain([(
                self.merge_diff.given().unwrap_or(""),
                self.merge_diff.given().is_some(),
            )])
            .find(|(_, on)| *on)
            .map(|(f, _)| f)
            .or_else(|| self.patch_flags.given())
    }

    /// The first argument given that a pyn server's commit filters cannot take.
    pub fn server_unsupported(&self) -> Option<String> {
        let literal = |ps: &[String]| ps.iter().any(|p| p.contains(['\\', '[', '*', '^', '$']));
        let flags = [
            ("--committer", !self.committer.is_empty()),
            ("--invert-grep", self.invert_grep),
            ("--all-match", self.all_match),
            ("--no-merges", self.no_merges),
            ("--merges", self.merges),
            ("--skip", self.skip.is_some()),
            ("--reverse", self.reverse),
            ("--all", self.all),
            ("--first-parent", self.first_parent),
            ("--graph", self.graph),
            ("--follow", self.follow),
            ("-S", self.pickaxe_text.is_some()),
            ("-G", self.pickaxe_lines.is_some()),
            ("-E", self.extended_regexp),
            ("-F", self.fixed_strings),
            ("--basic-regexp", self.basic_regexp),
            ("-P", self.perl_regexp),
            ("--author given more than once", self.author.len() > 1),
            ("--grep given more than once", self.grep.len() > 1),
            ("--author with a regular expression", literal(&self.author)),
            ("--grep with a regular expression", literal(&self.grep)),
            ("--", self.dashed.len() > 1),
        ];
        flags
            .into_iter()
            .chain(self.decorate.given())
            .chain([(
                self.merge_diff.given().unwrap_or(""),
                self.merge_diff.given().is_some(),
            )])
            .find(|(_, on)| *on)
            .map(|(f, _)| f.to_string())
    }

    /// Whether `--author`, `--grep`, `--since` or `--until` was given.
    pub fn has_server_filter(&self) -> bool {
        !self.author.is_empty()
            || !self.grep.is_empty()
            || self.since.is_some()
            || self.until.is_some()
    }

    /// The revision arguments after the first positional.
    pub fn positionals(&self) -> &[String] {
        &self.more
    }

    /// The paths after `--`.
    pub fn dashed(&self) -> &[String] {
        &self.dashed
    }

    /// The `/commits` parameters of `--author`, `--grep`, `--since` and `--until`.
    pub fn commit_filter(&self) -> Result<crate::log_filter::CommitFilter> {
        let now = Local::now();
        let date = |d: &Option<String>| -> Result<Option<String>> {
            let Some(d) = d else { return Ok(None) };
            let secs = parse_date(d, now)?;
            let t = chrono::DateTime::from_timestamp(secs, 0)
                .ok_or_else(|| anyhow::anyhow!("date out of range: {d}"))?;
            Ok(Some(t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        };
        Ok(crate::log_filter::CommitFilter {
            author: self.author.first().cloned(),
            grep: self.grep.first().cloned(),
            since: date(&self.since)?,
            until: date(&self.until)?,
            ..Default::default()
        })
    }

    /// Whether any diff output flag (`--stat`, `-p`, `-U`, ...) was given.
    pub fn diff_given(&self) -> bool {
        self.diff_parts().any()
    }

    pub fn format(&self) -> &FormatArgs {
        &self.fmt
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
fn select(
    git: &GitHome,
    cwd: &Path,
    args: Vec<String>,
    dashed: Vec<String>,
    all: bool,
) -> Result<Selection> {
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
                if !explicit && git.cwd_has(cwd, &arg) {
                    bail!(
                        "ambiguous argument '{arg}': both revision and filename; \
                         use '--' to separate paths from revisions"
                    );
                }
                revs.push(rev);
            }
            None if explicit => bail!("bad revision '{arg}'"),
            None => {
                if !super::stage::looks_like_pathspec(&arg) {
                    let rel = git.ws.repo_prefix(cwd, &arg)?;
                    if !on_disk(git, &rel) && (git.is_bare() || !in_head(git, &rel)?) {
                        bail!(
                            "ambiguous argument '{arg}': unknown revision or path not in the working tree.\n\
                             Use '--' to separate paths from revisions"
                        );
                    }
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
    if all {
        include.extend(all_tips(git)?);
    } else if !given {
        include.push(head_or_refuse(git)?);
    }
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
            patterns
                .iter()
                .map(|p| compile_as(p, a.ignore_case, a.dialect()))
                .collect()
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

impl GitLogArgs {
    pub fn diff_parts(&self) -> DiffParts {
        DiffParts {
            renames: self.renames.request(),
            summary: self.summary,
            names: if self.raw {
                Some(Kind::Raw)
            } else if self.name_only {
                Some(Kind::NameOnly)
            } else if self.name_status {
                Some(Kind::NameStatus)
            } else {
                None
            },
            stat: self.stat.is_some() || self.patch_flags.stat_implied(),
            dirstat: self.patch_flags.dirstat_given(),
            shortstat: self.shortstat,
            patch: self.patch || self.unified.is_some(),
            context: self.unified.unwrap_or(3),
            opts: self.patch_flags.opts(),
        }
    }
}

/// Every ref that names a commit, in ref order, then `HEAD`.
fn all_tips(git: &GitHome) -> Result<Vec<ObjectId>> {
    let mut tips = Vec::new();
    for (name, _) in git.repo.refs().list("refs/")? {
        tips.extend(git.repo.resolve(&name).ok());
    }
    tips.extend(git.repo.head()?.id());
    Ok(tips)
}

impl GitLogArgs {
    fn dialect(&self) -> Dialect {
        if self.extended_regexp {
            Dialect::Extended
        } else if self.fixed_strings {
            Dialect::Fixed
        } else {
            Dialect::Basic
        }
    }

    /// The first option combination `pyn log` cannot honor on a git repository.
    pub fn refused(&self) -> Option<String> {
        if self.perl_regexp {
            return Some(
                "-P` is not available: no Perl-compatible regular expression engine is built in; use -E".into(),
            );
        }
        if self.patch_flags.filters() {
            return Some("--diff-filter` also selects commits, which is not built".into());
        }
        if self.follow
            && (self.diff_given()
                || self.pickaxe_text.is_some()
                || self.pickaxe_lines.is_some()
                || self.merge_diff.given().is_some())
        {
            return Some(
                "--follow` does not combine with diff output, merge diff options, -S or -G".into(),
            );
        }
        if let Some(flag) = self.merge_diff.given()
            && (self.pickaxe_text.is_some() || self.pickaxe_lines.is_some())
        {
            return Some(format!("{flag}` does not combine with -S or -G"));
        }
        if self.graph {
            let combined = [
                ("diff output", self.diff_given()),
                ("merge diff options", self.merge_diff.given().is_some()),
                ("--author", !self.author.is_empty()),
                ("--committer", !self.committer.is_empty()),
                ("--grep", !self.grep.is_empty()),
                ("--merges", self.merges),
                ("--no-merges", self.no_merges),
                ("--since", self.since.is_some()),
                ("--until", self.until.is_some()),
                ("-S", self.pickaxe_text.is_some()),
                ("-G", self.pickaxe_lines.is_some()),
                ("--follow", self.follow),
            ];
            if let Some((what, _)) = combined.into_iter().find(|(_, on)| *on) {
                return Some(format!("--graph` does not combine with {what}"));
            }
        }
        None
    }

    fn gate(&self, paths: &[String], spec: &Pathspec) -> Result<Option<Gate>> {
        let pick = match (&self.pickaxe_text, &self.pickaxe_lines) {
            (Some(_), Some(_)) => {
                bail!("options '-G', '-S', and '--find-object' cannot be used together")
            }
            (Some(text), None) => Some(Pickaxe::Text {
                needle: text.clone().into_bytes(),
                fold: self.ignore_case,
            }),
            (None, Some(re)) => Some(Pickaxe::Lines(compile_ere(re, self.ignore_case)?)),
            (None, None) => None,
        };
        let follow = match (self.follow, paths) {
            (false, _) => None,
            (true, [one]) => Some(one.clone()),
            (true, _) => bail!("--follow requires exactly one pathspec"),
        };
        if follow.is_none() && pick.is_none() {
            return Ok(None);
        }
        Ok(Some(Gate {
            follow,
            pick,
            first_parent: self.first_parent,
            spec: spec.clone(),
            renames: self.renames.request(),
        }))
    }
}

pub(super) fn log(
    git: &GitHome,
    cwd: &Path,
    path: Option<String>,
    limit: Option<usize>,
    args: GitLogArgs,
) -> Result<()> {
    git.repo.validate_diff_ignore_submodules()?;
    // A bare repository has no prefix: paths are relative to the git directory wherever it is run.
    let cwd = if git.is_bare() { &git.ws.root } else { cwd };
    let rendering = args.fmt.rendering()?;
    let (tabs, abbrev_commit) = (rendering.tab_width(), rendering.abbrev_commit);
    let Rendering { pretty, style, .. } = rendering;
    let filters = Filters::new(&args)?;
    let parts = args.diff_parts();
    let patch_parts = DiffParts {
        patch: true,
        ..args.diff_parts()
    };
    let plan = args.merge_diff.plan(git, args.first_parent, false)?;
    let positional: Vec<String> = path.into_iter().chain(args.more.iter().cloned()).collect();
    let selection = select(git, cwd, positional, args.dashed.clone(), args.all)?;
    let decorate = args.decorate.resolve(git, &pretty)?;
    let commit_color = diff::commit_color(git, &parts.opts, decorate.on(), args.graph);
    let spec = git.pathspec(cwd, &selection.paths, false)?;
    let decorated = decorate.names(git, pretty.uses_decorations())?;
    let ctx = Context {
        style,
        now: chrono::Utc::now().timestamp(),
        decorations: &decorated,
        commit_color: commit_color.as_deref(),
        tabs,
        abbrev_commit,
    };

    if plan.needs_full_history() && !selection.paths.is_empty() {
        crate::usage_error(
            "`pyn log` cannot limit history to paths with `-m` or `--diff-merges` separate or first-parent",
        );
    }
    let followed = if args.follow {
        selection
            .paths
            .iter()
            .map(|p| git.ws.repo_prefix(cwd, p))
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };
    let gate = args.gate(&followed, &spec)?;
    let walk_spec = if gate.as_ref().is_some_and(|g| g.follow.is_some()) {
        Pathspec::default()
    } else {
        spec.clone()
    };
    let mut walk = git
        .repo
        .log_hiding(&selection.include, selection.hidden, walk_spec)?;
    if args.first_parent {
        walk.first_parent_only();
    }
    let mut printer = Printer {
        git,
        pretty: &pretty,
        ctx: &ctx,
        spec: &spec,
        parts: &parts,
        patch_parts: &patch_parts,
        plan,
        headers: 0,
        decorate: decorate.on(),
        gate,
        graph: None,
        missing_newline: false,
        members: HashSet::new(),
        shown: 0,
        out: String::new(),
    };
    let mut skip = args.skip.unwrap_or(0);
    if args.graph {
        if args.reverse {
            bail!("options '--reverse' and '--graph' cannot be used together");
        }
        if !selection.paths.is_empty() {
            bail!("`pyn log --graph` does not take a path");
        }
        let mut list = Vec::new();
        while let Some(entry) = walk.next_entry()? {
            list.push(entry);
        }
        printer.members = list.iter().map(|e| e.id).collect();
        printer.graph = Some(Graph::new());
        for entry in graph_order(list) {
            if skip > 0 {
                skip -= 1;
            } else if limit.is_none_or(|n| printer.shown < n) {
                printer.print_graph(&entry, args.first_parent)?;
            } else {
                break;
            }
        }
        emit(&printer.out);
        return Ok(());
    }
    let mut chosen = Vec::new();
    while limit.is_none_or(|n| chosen.len() + printer.shown < n)
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
        } else {
            printer.print(&entry)?;
        }
    }
    for entry in chosen.iter().rev() {
        printer.print(entry)?;
    }
    emit(&printer.out);
    Ok(())
}

struct Printer<'a> {
    git: &'a GitHome,
    pretty: &'a Pretty,
    ctx: &'a Context<'a>,
    spec: &'a Pathspec,
    parts: &'a DiffParts,
    patch_parts: &'a DiffParts,
    plan: Plan,
    headers: usize,
    decorate: bool,
    gate: Option<Gate>,
    graph: Option<Graph>,
    missing_newline: bool,
    members: HashSet<ObjectId>,
    shown: usize,
    out: String,
}

/// The entries in the topological order `--graph` implies.
fn graph_order(list: Vec<pyn_gitwork::LogEntry>) -> Vec<pyn_gitwork::LogEntry> {
    let ids: Vec<ObjectId> = list.iter().map(|e| e.id).collect();
    let parents = list
        .iter()
        .map(|e| (e.id, e.commit.parents.clone()))
        .collect();
    let mut by_id: std::collections::HashMap<ObjectId, pyn_gitwork::LogEntry> =
        list.into_iter().map(|e| (e.id, e)).collect();
    topo_order(&ids, &parents)
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect()
}

impl Printer<'_> {
    /// The text of `entry` with a graph in front of every line, assembled as git's `show_log` does.
    fn print_graph(&mut self, entry: &pyn_gitwork::LogEntry, first_parent: bool) -> Result<()> {
        let graph = self.graph.as_mut().expect("graph mode has a graph");
        let all = &entry.commit.parents;
        let shown: Vec<ObjectId> = if first_parent {
            all.iter()
                .take(1)
                .filter(|p| self.members.contains(*p))
                .copied()
                .collect()
        } else {
            all.iter()
                .filter(|p| self.members.contains(*p))
                .copied()
                .collect()
        };
        graph.update(entry.id, shown);
        let out = &mut self.out;
        if self.shown > 0 && !self.pretty.terminated() {
            if !self.missing_newline {
                graph.show_padding(out);
            }
            out.push('\n');
        }
        graph.show_commit(out);
        let text = render(self.pretty, self.ctx, &entry.id, &entry.commit);
        let names = self
            .ctx
            .decorations
            .get(&entry.id)
            .filter(|_| self.decorate);
        let names = names
            .map(|n| format!(" ({})", n.join(", ")))
            .unwrap_or_default();
        let message = match self.pretty {
            Pretty::User { .. } => text,
            Pretty::Oneline { .. } => {
                let (id, rest) = text.split_once(' ').unwrap_or((&text, ""));
                out.push_str(&format!("{id}{names} "));
                rest.to_string()
            }
            _ => {
                let (head, rest) = text.split_once('\n').unwrap_or((&text, ""));
                out.push_str(&format!("{head}{names}\n"));
                graph.show_oneline(out);
                rest.to_string()
            }
        };
        let mut lines = message.split_inclusive('\n').peekable();
        while let Some(line) = lines.next() {
            out.push_str(line);
            if line.ends_with('\n') && lines.peek().is_some() {
                graph.show_oneline(out);
            }
        }
        let terminated = message.ends_with('\n');
        if !graph.is_finished() {
            if !terminated {
                out.push('\n');
            }
            graph.show_remainder(out);
            if terminated {
                out.push('\n');
            }
        }
        self.missing_newline = !terminated;
        if self.pretty.terminator() == "\n" {
            if !self.missing_newline {
                graph.show_padding(out);
            }
            out.push('\n');
        }
        self.shown += 1;
        if self.out.len() > 64 * 1024 {
            emit(&std::mem::take(&mut self.out));
        }
        Ok(())
    }

    /// Adds the entry's text unless the gate drops the commit.
    fn print(&mut self, entry: &pyn_gitwork::LogEntry) -> Result<()> {
        let only = match &mut self.gate {
            Some(gate) => match gate.keeps(self.git, &entry.id)? {
                Some(only) => only,
                None => return Ok(()),
            },
            None => Vec::new(),
        };
        let c = &entry.commit;
        let asked = self.parts.any();
        let segments = if self.plan.diffs(c.parents.len() > 1, asked) {
            let parts = if asked { self.parts } else { self.patch_parts };
            let git = self.git;
            let spec = self.spec;
            let shape = Shape {
                git,
                pretty: self.pretty,
                spec,
                only: &[],
                quiet: false,
            };
            let mut against = |source: Against| -> Result<Option<Body>> {
                let parent_tree = match source {
                    Against::Parent(Some(p)) => Some(git.repo.commit(&p)?.tree),
                    Against::Parent(None) => None,
                    Against::Tree(t) => Some(t),
                };
                let (mut old, mut new) = (Tree::new(), Tree::new());
                let changes = git
                    .repo
                    .diff_trees(parent_tree.as_ref(), Some(&c.tree), spec)?;
                add_changes(git, &mut old, &mut new, &changes)?;
                if parts.renames.is_harder(git.rename_settings("diff")) {
                    let trees = (parent_tree.as_ref(), Some(&c.tree));
                    add_unchanged(git, (&mut old, &mut new), trees, spec)?;
                }
                Ok(diff::sections(git, &old, &new, (&[], &only), parts)?
                    .map(|(text, dashes)| Body { text, dashes }))
            };
            shape.segments(&self.plan, c, &|| Want::of_parts(parts), &mut against)?
        } else {
            vec![super::merge_diff::Segment {
                from: None,
                tail: String::new(),
            }]
        };
        for seg in segments {
            if self.headers > 0 && !self.pretty.terminated() {
                self.out.push('\n');
            }
            let text = render(self.pretty, self.ctx, &entry.id, c);
            let text = if self.decorate {
                with_decoration(text, self.pretty, self.ctx.decorations.get(&entry.id))
            } else {
                text
            };
            self.out
                .push_str(&with_from(text, self.pretty, seg.from.as_ref()));
            self.out.push_str(self.pretty.terminator());
            self.out.push_str(&seg.tail);
            self.headers += 1;
        }
        self.shown += 1;
        if self.out.len() > 64 * 1024 {
            emit(&std::mem::take(&mut self.out));
        }
        Ok(())
    }
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

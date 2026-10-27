//! How `show` and `log` print the diff of a merge commit: `-m`, `-c`, `--cc`, `--first-parent` and `--diff-merges`.

use anyhow::Result;
use pyn_gitwork::Pathspec;
use pyn_object::{Commit, ObjectId};

use super::GitHome;
use super::combined_diff;
use crate::commit_format::{Pretty, short};
use crate::diff::{self, DiffParts, Format, Kind};
use crate::rename;

/// The merge-diff options; the last one given wins, as in git.
#[derive(clap::Args, Debug, Default)]
pub struct MergeArgs {
    /// Show a merge's diff against each parent in turn (`--diff-merges=on`).
    #[arg(short = 'm', overrides_with_all = ["combined", "dense", "dd", "none", "remerge", "diff_merges"])]
    separate: bool,
    /// Show a combined diff of a merge against all its parents (implies `-p`).
    #[arg(short = 'c', overrides_with_all = ["separate", "dense", "dd", "none", "remerge", "diff_merges"])]
    combined: bool,
    /// Show a combined diff that leaves out hunks taken from one parent (implies `-p`).
    #[arg(long = "cc", overrides_with_all = ["separate", "combined", "dd", "none", "remerge", "diff_merges"])]
    dense: bool,
    /// Show a merge's diff against its first parent (implies `-p`).
    #[arg(long = "dd", overrides_with_all = ["separate", "combined", "dense", "none", "remerge", "diff_merges"])]
    dd: bool,
    /// Show no diff for merges.
    #[arg(long = "no-diff-merges", overrides_with_all = ["separate", "combined", "dense", "dd", "remerge", "diff_merges"])]
    none: bool,
    /// Diff a merge against a fresh merge of its parents (implies `-p`).
    #[arg(long = "remerge-diff", overrides_with_all = ["separate", "combined", "dense", "dd", "none", "diff_merges"])]
    remerge: bool,
    /// How to show a merge's diff: off, none, on, m, first-parent, 1, separate, combined, c, dense-combined, cc or remerge.
    #[arg(long = "diff-merges", value_name = "MODE", overrides_with_all = ["separate", "combined", "dense", "dd", "none", "remerge"])]
    diff_merges: Option<String>,
}

impl MergeArgs {
    /// The first option given, for a command that cannot honor any of them.
    pub fn given(&self) -> Option<&'static str> {
        [
            ("-m", self.separate),
            ("-c", self.combined),
            ("--cc", self.dense),
            ("--dd", self.dd),
            ("--no-diff-merges", self.none),
            ("--remerge-diff", self.remerge),
            ("--diff-merges", self.diff_merges.is_some()),
        ]
        .into_iter()
        .find(|(_, on)| *on)
        .map(|(f, _)| f)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    None,
    Separate { first_parent: bool },
    Combined { dense: bool },
    Remerge,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Plan {
    pub mode: Mode,
    /// Merges are diffed even when no diff output was asked for.
    pub need_diff: bool,
    /// Every commit is diffed (`-c`, `--cc`, `--dd` imply `-p`).
    pub imply_patch: bool,
    /// A merge option was given, not just defaulted.
    pub explicit: bool,
}

/// What a `--diff-merges` value or `log.diffMerges` names.
fn named(value: &str, default: Mode) -> Result<(Mode, bool), ()> {
    let sep = Mode::Separate {
        first_parent: false,
    };
    match value {
        "off" | "none" => Ok((Mode::None, false)),
        "1" | "first-parent" => Ok((Mode::Separate { first_parent: true }, true)),
        "separate" => Ok((sep, true)),
        "c" | "combined" => Ok((Mode::Combined { dense: false }, true)),
        "cc" | "dense-combined" => Ok((Mode::Combined { dense: true }, true)),
        "m" | "on" => Ok((default, true)),
        "r" | "remerge" => Ok((Mode::Remerge, true)),
        _ => Err(()),
    }
}

impl MergeArgs {
    /// The behaviour the options, `--first-parent` and `log.diffMerges` ask for; `show` diffs merges by default.
    pub(super) fn plan(&self, git: &GitHome, first_parent: bool, show: bool) -> Result<Plan> {
        let sep = Mode::Separate {
            first_parent: false,
        };
        let default = || -> Result<Mode> {
            match git.repo.config().get("log.diffmerges") {
                None => Ok(sep),
                Some(v) => match named(v, sep) {
                    Ok((mode, _)) => Ok(mode),
                    Err(()) => anyhow::bail!("bad config value '{v}' for 'log.diffmerges'"),
                },
            }
        };
        let (mode, need_diff, imply_patch, explicit) = if self.separate {
            (default()?, false, false, true)
        } else if self.combined {
            (Mode::Combined { dense: false }, true, true, true)
        } else if self.dense {
            (Mode::Combined { dense: true }, true, true, true)
        } else if self.dd {
            (Mode::Separate { first_parent: true }, true, true, true)
        } else if self.none {
            (Mode::None, false, false, true)
        } else if self.remerge {
            (Mode::Remerge, true, true, true)
        } else if let Some(value) = &self.diff_merges {
            match named(
                value,
                if value == "m" || value == "on" {
                    default()?
                } else {
                    sep
                },
            ) {
                Ok((mode, need)) => (mode, need, false, true),
                Err(()) => anyhow::bail!("invalid value for '--diff-merges': '{value}'"),
            }
        } else if show {
            (Mode::Combined { dense: true }, true, false, false)
        } else {
            (Mode::None, false, false, false)
        };
        let mode = if first_parent && (!explicit || matches!(mode, Mode::Separate { .. })) {
            Mode::Separate { first_parent: true }
        } else {
            mode
        };
        Ok(Plan {
            mode,
            need_diff,
            imply_patch,
            explicit,
        })
    }
}

impl Plan {
    /// Whether a commit gets a diff, given whether any diff output was asked for.
    pub(super) fn diffs(&self, merge: bool, asked: bool) -> bool {
        let all = asked || self.imply_patch;
        if merge {
            (all || self.need_diff) && self.mode != Mode::None
        } else {
            all
        }
    }

    /// An option asked for merge diffs that history simplification and the pickaxe cannot follow.
    pub(super) fn needs_full_history(&self) -> bool {
        self.explicit && matches!(self.mode, Mode::Separate { .. })
    }
}

/// What a combined diff prints: stat-like sections against the first parent, names, and the combined patch.
pub(super) struct Want {
    pub names: Option<Kind>,
    pub stats: Vec<Kind>,
    pub patch: bool,
    pub context: usize,
    pub renames: rename::Request,
}

impl Want {
    pub(super) fn of_parts(p: &DiffParts) -> Self {
        let mut stats = Vec::new();
        if p.stat {
            stats.push(Kind::Stat);
        }
        if p.shortstat {
            stats.push(Kind::ShortStat);
        }
        if p.summary {
            stats.push(Kind::Summary);
        }
        Self {
            names: p.names,
            stats,
            patch: p.patch,
            context: p.context,
            renames: p.renames,
        }
    }

    pub(super) fn of_format(f: &Format) -> Self {
        let mut want = Self {
            names: None,
            stats: Vec::new(),
            patch: false,
            context: f.context,
            renames: f.renames,
        };
        match f.kind {
            Kind::Patch => want.patch = true,
            Kind::Stat | Kind::ShortStat | Kind::NumStat => want.stats.push(f.kind),
            Kind::NameOnly | Kind::NameStatus => want.names = Some(f.kind),
            Kind::Summary => {}
            Kind::Check => crate::usage_error("`--check` does not combine with a combined diff"),
            Kind::Raw | Kind::Dirstat | Kind::Nothing => {
                crate::usage_error("this output format does not combine with a combined diff")
            }
        }
        if f.opts.ws.any() {
            crate::usage_error("`-w` does not combine with a combined diff");
        }
        if f.summary || f.kind == Kind::Summary {
            want.stats.push(Kind::Summary);
        }
        want
    }
}

/// A diff body and whether `---` follows the message (a stat with a patch).
pub(super) struct Body {
    pub text: String,
    pub dashes: bool,
}

/// What a diff is taken against.
pub(super) enum Against {
    Parent(Option<ObjectId>),
    Tree(ObjectId),
}

/// One header of a commit and what follows it.
pub(super) struct Segment {
    pub from: Option<ObjectId>,
    pub tail: String,
}

pub(super) struct Shape<'a> {
    pub git: &'a GitHome,
    pub pretty: &'a Pretty,
    pub spec: &'a Pathspec,
    pub only: &'a [String],
    /// `-s`: nothing follows a header.
    pub quiet: bool,
}

impl Shape<'_> {
    fn lead(&self, body: &Body) -> &'static str {
        if self.quiet || !self.pretty.blank_before_diff() {
            ""
        } else if body.dashes {
            "---\n"
        } else {
            "\n"
        }
    }

    fn tail(&self, body: Option<Body>) -> String {
        match body {
            Some(b) if !self.quiet => format!("{}{}", self.lead(&b), b.text),
            _ => String::new(),
        }
    }

    /// The headers a commit prints, each with the text after it. `against` diffs the commit against one parent.
    pub(super) fn segments(
        &self,
        plan: &Plan,
        c: &Commit,
        want: &dyn Fn() -> Want,
        against: &mut dyn FnMut(Against) -> Result<Option<Body>>,
    ) -> Result<Vec<Segment>> {
        let plain = |tail| vec![Segment { from: None, tail }];
        if c.parents.len() <= 1 {
            let body = against(Against::Parent(c.parents.first().copied()))?;
            return Ok(plain(self.tail(body)));
        }
        match plan.mode {
            Mode::None => Ok(plain(String::new())),
            Mode::Separate { first_parent } => {
                let mut out = Vec::new();
                for p in &c.parents {
                    if let Some(body) = against(Against::Parent(Some(*p)))? {
                        out.push(Segment {
                            from: (!first_parent).then_some(*p),
                            tail: self.tail(Some(body)),
                        });
                    }
                    if first_parent {
                        break;
                    }
                }
                if out.is_empty() {
                    out = plain(String::new());
                }
                Ok(out)
            }
            Mode::Combined { dense } => Ok(plain(self.combined(c, dense, &want())?)),
            Mode::Remerge if c.parents.len() > 2 => Ok(plain(String::from(
                "diff: warning: Skipping remerge-diff for octopus merges.\n",
            ))),
            Mode::Remerge => {
                let mut re = super::remerge_diff::recreate(self.git, c)?;
                re.notes
                    .retain(|path, _| self.spec.matches(path.as_bytes()));
                let body = against(Against::Tree(re.tree))?;
                let patch = want().patch;
                let body = match body {
                    Some(b) => Some(Body {
                        text: super::remerge_diff::annotate(&b.text, &re.notes, patch),
                        ..b
                    }),
                    None if patch && !re.notes.is_empty() => Some(Body {
                        text: super::remerge_diff::annotate("", &re.notes, true),
                        dashes: false,
                    }),
                    None => None,
                };
                Ok(plain(self.tail(body)))
            }
        }
    }

    fn combined(&self, c: &Commit, dense: bool, want: &Want) -> Result<String> {
        if self.quiet {
            return Ok(String::new());
        }
        if c.parents.len() > 62 {
            crate::usage_error("a combined diff takes at most 62 parents");
        }
        let git = self.git;
        let mut out = String::new();
        if !self.pretty.empty_format() {
            out.push('\n');
        }
        let mut trees = Vec::with_capacity(c.parents.len());
        for p in &c.parents {
            trees.push(git.repo.commit(p)?.tree);
        }
        for kind in [Kind::NumStat, Kind::Stat, Kind::ShortStat, Kind::Summary] {
            if !want.stats.contains(&kind) {
                continue;
            }
            let (mut old, mut new) = (diff::Tree::new(), diff::Tree::new());
            let changes = git
                .repo
                .diff_trees(Some(&trees[0]), Some(&c.tree), self.spec)?;
            super::commands::add_changes(git, &mut old, &mut new, &changes)?;
            let format = Format {
                context: want.context,
                kind,
                opts: crate::diff_opts::Opts::default(),
                with_patch: false,
                renames: want.renames,
                summary: false,
                kept: Vec::new(),
            };
            diff::compare(git, &old, &new, self.only, &format, &mut out)?;
        }
        let elems =
            combined_diff::elements(git, &c.tree, &trees, (self.spec, self.only), want.renames)?;
        if elems.is_empty() {
            return Ok(out);
        }
        for e in elems
            .iter()
            .flat_map(|e| e.parents.iter().map(|(b, _)| b).chain([&e.result]))
            .flatten()
        {
            if e.is_gitlink() {
                crate::usage_error("a combined diff of a submodule is not available");
            }
        }
        let mut sep = false;
        if let Some(kind) = want.names {
            for e in &elems {
                out.push_str(&combined_diff::name_line(git, e, kind == Kind::NameStatus));
            }
            sep = true;
        } else if !want.stats.is_empty() {
            sep = true;
        }
        if want.patch {
            if sep {
                out.push('\n');
            }
            for e in &elems {
                if let Some(text) = combined_diff::patch(git, e, (dense, want.context))? {
                    out.push_str(&text);
                }
            }
        }
        Ok(out)
    }
}

/// Inserts ` (from <parent>)` after the id on the first line of a built-in format; `--format` templates have none.
pub(super) fn with_from(mut text: String, pretty: &Pretty, from: Option<&ObjectId>) -> String {
    let Some(p) = from else { return text };
    let (skip, short_id) = match pretty {
        Pretty::User { .. } => return text,
        Pretty::Oneline { abbrev } => (0, *abbrev),
        _ => (7, false),
    };
    let colour = if text.starts_with("\x1b[") {
        text.find('m').map_or(0, |i| i + 1)
    } else {
        0
    };
    let skip = skip + colour;
    let end = text[skip..]
        .find([' ', '\n', '\x1b'])
        .map_or(text.len(), |i| i + skip);
    let name = if short_id { short(p) } else { p.to_hex() };
    text.insert_str(end, &format!(" (from {name})"));
    text
}

//! `pyn describe` on a git repository: a commit named by the nearest tag that reaches it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::wildmatch;
use pyn_object::{Kind, ObjectId, Tag};

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes, peel, prefix};

const SEEN: u32 = 1;

/// Name a commit after the nearest tag it descends from.
#[derive(Debug, Args)]
pub struct DescribeArgs {
    /// Also use lightweight tags.
    #[arg(long)]
    tags: bool,
    /// Use any ref, not only tags.
    #[arg(long)]
    all: bool,
    /// Show an abbreviated commit id when nothing describes the commit.
    #[arg(long)]
    always: bool,
    /// Show the long form (`<tag>-<n>-g<id>`) even when a tag names the commit exactly.
    #[arg(long)]
    long: bool,
    /// Use this many digits of the commit id (7 or `core.abbrev` if omitted); 0 omits the suffix.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
    abbrev: Option<String>,
    /// Append a mark (`-dirty` if omitted) when the work tree differs from HEAD.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "-dirty")]
    dirty: Option<String>,
    /// Only consider tags matching this glob; may be repeated.
    #[arg(long = "match", value_name = "PATTERN")]
    matches: Vec<String>,
    /// Do not consider tags matching this glob; may be repeated.
    #[arg(long, value_name = "PATTERN")]
    exclude: Vec<String>,
    /// Succeed only when a tag names the commit exactly.
    #[arg(long)]
    exact_match: bool,
    /// Consider this many tags before choosing the nearest (10 if omitted).
    #[arg(long, value_name = "N")]
    candidates: Option<usize>,
    /// Commits to describe (`HEAD` if none).
    commit_ish: Vec<String>,
}

struct Name {
    name: String,
    prio: u8,
    tagged_at: i64,
}

struct Candidate {
    name: String,
    depth: usize,
    flag: u32,
    order: usize,
}

pub(super) fn describe(git: &GitHome, cwd: &Path, args: DescribeArgs) -> Result<()> {
    let hex = git.algorithm().digest_len() * 2;
    let digits = match args.abbrev.as_deref() {
        None | Some("") => git.repo.abbreviation_len(None)?,
        Some(v) => match v.parse::<usize>() {
            Ok(0) => 0,
            Ok(n) => n.clamp(4, hex),
            Err(_) => bail!("option `abbrev' expects a numerical value"),
        },
    };
    if args.long && digits == 0 {
        bail!("options '--long' and '--abbrev=0' cannot be used together");
    }
    if args.dirty.is_some() && !args.commit_ish.is_empty() {
        bail!("option '--dirty' and commit-ishes cannot be used together");
    }
    let max = if args.exact_match {
        0
    } else {
        args.candidates.unwrap_or(10).min(32)
    };
    let names = known_names(git, &args)?;
    if names.is_empty() && !args.always {
        bail!("No names found, cannot describe anything.");
    }
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let mut out = String::new();
    let targets: Vec<String> = if args.commit_ish.is_empty() {
        vec!["HEAD".into()]
    } else {
        args.commit_ish.clone()
    };
    for target in &targets {
        let found = match resolver.object(target)? {
            Some(id) => peel(git, &id, Some(Kind::Commit))?,
            None => None,
        };
        let Some(commit) = found else {
            bail!("Not a valid object name {target}");
        };
        let mut text = describe_one(git, &args, &names, &commit, digits, max)?;
        if let Some(mark) = &args.dirty
            && dirty(git)?
        {
            text.push_str(mark);
        }
        out.push_str(&text);
        out.push('\n');
    }
    emit_bytes(out.as_bytes());
    Ok(())
}

/// How `git submodule status` names a commit: an annotated tag, else any tag, else any ref, else the abbreviated id.
pub(super) fn submodule_name(git: &GitHome, commit: &ObjectId) -> Result<String> {
    let digits = git.repo.abbreviation_len(None)?;
    for (tags, all) in [(false, false), (true, false), (false, true)] {
        let args = DescribeArgs {
            tags,
            all,
            always: all,
            ..plain()
        };
        let names = known_names(git, &args)?;
        if let Ok(text) = describe_one(git, &args, &names, commit, digits, 10) {
            return Ok(text);
        }
    }
    Ok(git.repo.abbreviate(commit)?)
}

fn plain() -> DescribeArgs {
    DescribeArgs {
        tags: false,
        all: false,
        always: false,
        long: false,
        abbrev: None,
        dirty: None,
        matches: Vec::new(),
        exclude: Vec::new(),
        exact_match: false,
        candidates: None,
        commit_ish: Vec::new(),
    }
}

fn dirty(git: &GitHome) -> Result<bool> {
    Ok(git.repo.status()?.entries.iter().any(|e| {
        e.staged.is_some()
            || e.conflict.is_some()
            || e.unstaged
                .is_some_and(|k| k != pyn_gitwork::Kind::Untracked)
    }))
}

fn known_names(git: &GitHome, args: &DescribeArgs) -> Result<HashMap<ObjectId, Name>> {
    let mut names: HashMap<ObjectId, Name> = HashMap::new();
    for (full, id) in git.repo.refs().list("refs/")? {
        let is_tag = full.starts_with("refs/tags/");
        if !args.all && !is_tag {
            continue;
        }
        let short = full.strip_prefix("refs/tags/").unwrap_or(&full);
        if !args.matches.is_empty()
            && (!is_tag
                || !args
                    .matches
                    .iter()
                    .any(|p| wildmatch(p.as_bytes(), short.as_bytes(), false)))
        {
            continue;
        }
        if is_tag
            && args
                .exclude
                .iter()
                .any(|p| wildmatch(p.as_bytes(), short.as_bytes(), false))
        {
            continue;
        }
        let Some(object) = git.repo.objects().read_opt(&id)? else {
            continue;
        };
        let annotated = object.kind == Kind::Tag;
        let Some(commit) = peel(git, &id, Some(Kind::Commit))? else {
            continue;
        };
        let prio = if annotated {
            2
        } else if is_tag {
            1
        } else {
            0
        };
        let tagged_at = if annotated {
            Tag::parse(git.algorithm(), &object.data)?
                .tagger
                .map_or(0, |t| t.time)
        } else {
            0
        };
        let replace = match names.get(&commit) {
            None => true,
            Some(e) if e.prio < prio => true,
            Some(e) if e.prio == 2 && prio == 2 => e.tagged_at < tagged_at,
            Some(_) => false,
        };
        if replace {
            let name = if args.all {
                full.strip_prefix("refs/").unwrap_or(&full).to_string()
            } else {
                short.to_string()
            };
            names.insert(
                commit,
                Name {
                    name,
                    prio,
                    tagged_at,
                },
            );
        }
    }
    Ok(names)
}

fn describe_one(
    git: &GitHome,
    args: &DescribeArgs,
    names: &HashMap<ObjectId, Name>,
    commit: &ObjectId,
    digits: usize,
    max: usize,
) -> Result<String> {
    let abbreviations = git.repo.abbreviations(Some(digits))?;
    let suffix = |depth: usize| -> String { format!("-{depth}-g{}", abbreviations.short(commit)) };
    let usable = |n: &Name| args.tags || args.all || n.prio == 2;
    if let Some(n) = names.get(commit).filter(|n| usable(n)) {
        return Ok(if args.long {
            format!("{}{}", n.name, suffix(0))
        } else {
            n.name.clone()
        });
    }
    let hex = commit.to_hex();
    let fallback = || abbreviations.short(commit);
    if args.exact_match {
        bail!("no tag exactly matches '{hex}'");
    }
    let (mut cands, unannotated) = walk(git, args, names, commit, max)?;
    if cands.is_empty() {
        if args.always {
            return Ok(fallback());
        }
        if unannotated > 0 {
            bail!(
                "No annotated tags can describe '{hex}'.\n\
                 However, there were unannotated tags: try --tags."
            );
        }
        bail!("No tags can describe '{hex}'.\nTry --always, or create some tags.");
    }
    cands.sort_by_key(|c| (c.depth, c.order));
    let best = &cands[0];
    if digits == 0 {
        return Ok(best.name.clone());
    }
    Ok(format!("{}{}", best.name, suffix(best.depth)))
}

struct Queue<'a> {
    git: &'a GitHome,
    heap: BinaryHeap<(i64, Reverse<usize>)>,
    ids: Vec<ObjectId>,
}

impl Queue<'_> {
    fn put(&mut self, id: ObjectId) -> Result<()> {
        let time = self.git.repo.commit(&id)?.committer.time;
        self.heap.push((time, Reverse(self.ids.len())));
        self.ids.push(id);
        Ok(())
    }

    fn get(&mut self) -> Option<ObjectId> {
        self.heap.pop().map(|(_, Reverse(at))| self.ids[at])
    }
}

/// git's walk from `start` by commit date: the tagged commits met first, each with its distance, and how many
/// lightweight tags were passed over.
fn walk(
    git: &GitHome,
    args: &DescribeArgs,
    names: &HashMap<ObjectId, Name>,
    start: &ObjectId,
    max: usize,
) -> Result<(Vec<Candidate>, usize)> {
    let mut flags: HashMap<ObjectId, u32> = HashMap::new();
    let mut queue = Queue {
        git,
        heap: BinaryHeap::new(),
        ids: Vec::new(),
    };
    flags.insert(*start, SEEN);
    queue.put(*start)?;
    let mut cands: Vec<Candidate> = Vec::new();
    let (mut seen, mut annotated, mut unannotated) = (0usize, 0usize, 0usize);
    let mut gave_up_on = None;
    while let Some(c) = queue.get() {
        seen += 1;
        if let Some(n) = names.get(&c) {
            if !args.tags && !args.all && n.prio < 2 {
                unannotated += 1;
            } else if cands.len() < max {
                let flag = 1u32 << (cands.len() + 1);
                cands.push(Candidate {
                    name: n.name.clone(),
                    depth: seen - 1,
                    flag,
                    order: cands.len(),
                });
                *flags.entry(c).or_insert(0) |= flag;
                if n.prio == 2 {
                    annotated += 1;
                }
            } else {
                gave_up_on = Some(c);
                break;
            }
        }
        let mine = flags.get(&c).copied().unwrap_or(0);
        for t in &mut cands {
            if mine & t.flag == 0 {
                t.depth += 1;
            }
        }
        if annotated > 0 && queue.heap.is_empty() {
            break;
        }
        for p in git.repo.commit(&c)?.parents {
            let f = flags.entry(p).or_insert(0);
            let first = *f & SEEN == 0;
            *f |= mine;
            if first {
                queue.put(p)?;
            }
        }
    }
    if cands.is_empty() {
        return Ok((cands, unannotated));
    }
    cands.sort_by_key(|c| (c.depth, c.order));
    if let Some(g) = gave_up_on {
        queue.put(g)?;
    }
    let flag = cands[0].flag;
    let mut extra = 0;
    while let Some(c) = queue.get() {
        let mine = flags.get(&c).copied().unwrap_or(0);
        if mine & flag != 0 {
            let all_within = queue.heap.iter().all(|(_, Reverse(at))| {
                flags.get(&queue.ids[*at]).copied().unwrap_or(0) & flag != 0
            });
            if all_within {
                break;
            }
        } else {
            extra += 1;
        }
        for p in git.repo.commit(&c)?.parents {
            let f = flags.entry(p).or_insert(0);
            let first = *f & SEEN == 0;
            *f |= mine;
            if first {
                queue.put(p)?;
            }
        }
    }
    cands[0].depth += extra;
    Ok((cands, unannotated))
}

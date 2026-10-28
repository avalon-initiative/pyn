//! `pyn blame` and `pyn annotate` on a git repository: `git blame`'s lines, line ranges, rename following and formats.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use std::rc::Rc;

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::Entry as TreeEntry;
use pyn_object::{FileMode, Kind, ObjectId, Signature};

use super::GitHome;
use super::blame_format::{Entry, Flags, Info, Layout};
use super::blame_range::{RangeError, parse};
use super::blame_rename::blob_id;
use super::blame_walk::{Board, Fake, Settings};
use super::plumbing::{Resolver, emit_bytes, peel, prefix, usage};
use crate::commit_format::DateStyle;

const BLAME_USAGE: &str = "usage: pyn blame [<options>] [<rev-opts>] [<rev>] [--] <file>";
const ANNOTATE_USAGE: &str = "usage: pyn annotate [<options>] [<rev-opts>] [<rev>] [--] <file>";

const HELP: &str = "\
Options (a git repository only; output and exit status are git blame's):
  -L <range>            only these lines: <start>,<end>, +<n> and -<n> offsets, /<regex>/ anchors; repeatable
  -w                    ignore whitespace when finding the lines a commit changed
  --first-parent        follow only the first parent of a merge
  -s  -e  -f  -n        no author and date, author email, original file name, original line number
  -l  -t  -b            long ids, raw timestamps, blank ids of boundary commits
  --root                do not treat root commits as boundaries
  -p, --porcelain       machine output; --line-porcelain repeats the commit details for every line
  --abbrev=<n>          digits of the id; --date=<format> sets the date style
  --contents <file>     blame this file's contents (`-` for standard input) as the newest version
Revisions: <rev>, <a>..<b>, ^<rev>. Not built (usage error 129): -M, -C, --incremental, -L :<funcname>,
--ignore-rev, --ignore-revs-file, --reverse, --first-parent, --show-stats, --score-debug, --color-lines, --color-by-age,
--minimal, --diff-algorithm, -S, and other revision options.";

#[derive(Args, Debug)]
#[command(disable_help_flag = true, after_help = HELP)]
pub struct BlameArgs {
    #[arg(long, action = clap::ArgAction::Help, hide = true)]
    help: Option<bool>,
    /// Options, revisions and the file, as for `git blame`.
    #[arg(
        allow_hyphen_values = true,
        trailing_var_arg = true,
        value_name = "ARGS"
    )]
    args: Vec<OsString>,
}

#[derive(Default)]
struct Parsed {
    flags: Flags,
    ranges: Vec<String>,
    abbrev: Option<i64>,
    date: Option<String>,
    contents: Option<String>,
    root: Option<bool>,
    ignore_whitespace: bool,
    first_parent: bool,
    positional: Vec<String>,
    after_dashdash: Option<Vec<String>>,
}

fn fail(annotate: bool, message: &str) -> ! {
    eprintln!("error: {message}");
    eprintln!(
        "{}",
        if annotate {
            ANNOTATE_USAGE
        } else {
            BLAME_USAGE
        }
    );
    std::process::exit(129);
}

fn not_built(annotate: bool, option: &str) -> ! {
    fail(
        annotate,
        &format!("option `{option}' is not supported on a git repository yet"),
    )
}

fn parse_args(args: Vec<OsString>, annotate: bool) -> Parsed {
    let mut p = Parsed::default();
    let mut it = args.into_iter().map(|a| match a.into_string() {
        Ok(s) => s,
        Err(_) => fail(annotate, "arguments must be valid UTF-8"),
    });
    while let Some(arg) = it.next() {
        if arg == "--" {
            p.after_dashdash = Some(it.by_ref().collect());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (long, None),
            };
            match (name, value) {
                ("root", None) => p.root = Some(true),
                ("no-root", None) => p.root = Some(false),
                ("porcelain", None) => p.flags.porcelain = true,
                ("line-porcelain", None) => {
                    p.flags.porcelain = true;
                    p.flags.line_porcelain = true;
                }
                ("show-name", None) => p.flags.show_name = true,
                ("show-number", None) => p.flags.show_number = true,
                ("show-email", None) => p.flags.show_email = true,
                ("abbrev", None) => p.abbrev = Some(-1),
                ("no-abbrev", None) => p.abbrev = Some(0),
                ("abbrev", Some(v)) => match v.parse::<i64>() {
                    Ok(n) => p.abbrev = Some(n),
                    Err(_) => fail(
                        annotate,
                        &format!("option `abbrev' expects a numerical value, not `{v}'"),
                    ),
                },
                ("first-parent", None) => p.first_parent = true,
                ("date", Some(v)) => p.date = Some(v),
                ("contents", Some(v)) => p.contents = Some(v),
                ("contents", None) => match it.next() {
                    Some(v) => p.contents = Some(v),
                    None => fail(annotate, "option `contents' requires a value"),
                },
                (
                    "incremental" | "show-stats" | "progress" | "score-debug" | "color-lines"
                    | "color-by-age" | "minimal" | "ignore-rev" | "ignore-revs-file" | "reverse"
                    | "diff-algorithm",
                    _,
                ) => not_built(annotate, &format!("--{name}")),
                _ => fail(annotate, &format!("unknown option `{arg}'")),
            }
        } else if arg.len() > 1 && arg.starts_with('-') {
            let cluster: Vec<char> = arg[1..].chars().collect();
            let mut i = 0;
            while i < cluster.len() {
                match cluster[i] {
                    'b' => p.flags.blank_boundary = true,
                    'f' => p.flags.show_name = true,
                    'n' => p.flags.show_number = true,
                    'p' => p.flags.porcelain = true,
                    'c' => p.flags.annotate = true,
                    't' => p.flags.raw_time = true,
                    'l' => p.flags.long_names = true,
                    's' => p.flags.no_author = true,
                    'e' => p.flags.show_email = true,
                    'w' => p.ignore_whitespace = true,
                    'L' => {
                        let rest: String = cluster[i + 1..].iter().collect();
                        let value = if rest.is_empty() {
                            match it.next() {
                                Some(v) => v,
                                None => fail(annotate, "switch `L' requires a value"),
                            }
                        } else {
                            rest
                        };
                        p.ranges.push(value);
                        break;
                    }
                    'M' | 'C' | 'S' => not_built(annotate, &format!("-{}", cluster[i])),
                    other => fail(annotate, &format!("unknown switch `{other}'")),
                }
                i += 1;
            }
        } else {
            p.positional.push(arg);
        }
    }
    p
}

/// The arguments after the subcommand as typed: clap drops a leading `--`, which git's argument rules need.
fn raw_args(annotate: bool, parsed: Vec<OsString>) -> Vec<OsString> {
    let name = if annotate { "annotate" } else { "blame" };
    let all: Vec<OsString> = std::env::args_os().collect();
    match all.iter().position(|a| a == name) {
        Some(at) => all[at + 1..].to_vec(),
        None => parsed,
    }
}

fn usage_exit(line: &str) -> ! {
    eprintln!("{line}");
    std::process::exit(129);
}

fn die(message: String) -> anyhow::Error {
    anyhow::anyhow!(message)
}

pub(super) fn blame(git: &GitHome, cwd: &Path, args: BlameArgs, annotate: bool) -> Result<()> {
    let mut p = parse_args(raw_args(annotate, args.args), annotate);
    let usage_line = if annotate {
        ANNOTATE_USAGE
    } else {
        BLAME_USAGE
    };
    if annotate {
        p.flags.annotate = true;
    }
    if git.config().get("mailmap.file").is_some()
        || git.config().get("mailmap.blob").is_some()
        || git.repo.root().is_ok_and(|r| r.join(".mailmap").exists())
    {
        bail!("blame does not read mailmaps on a git repository yet");
    }
    if let Some(algo) = git.config().get("diff.algorithm")
        && !matches!(algo, "default" | "myers")
    {
        bail!("diff.algorithm={algo} is not supported by blame on a git repository yet");
    }
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let is_rev = |text: &str| {
        resolver
            .object(text)
            .ok()
            .flatten()
            .is_some_and(|id| super::plumbing::kind_of(git, &id).ok().flatten().is_some())
    };
    let (path_arg, rev_args) = match p.after_dashdash.take() {
        Some(tail) => match tail.len() {
            1 => (tail[0].clone(), std::mem::take(&mut p.positional)),
            2 if p.positional.is_empty() => (tail[0].clone(), vec![tail[1].clone()]),
            _ => usage_exit(usage_line),
        },
        None => {
            let n = p.positional.len();
            if n == 0 {
                usage_exit(usage_line);
            }
            if n == 2 && is_rev(&p.positional[1]) {
                (p.positional[0].clone(), vec![p.positional[1].clone()])
            } else {
                if n == 1 && is_rev(&p.positional[0]) {
                    bail!("missing <path> to blame");
                }
                let mut revs = std::mem::take(&mut p.positional);
                let path = revs.pop().unwrap_or_default();
                (path, revs)
            }
        }
    };
    let path = git.ws.repo_prefix(cwd, &path_arg)?.into_bytes();

    let commit_of = |text: &str, name: &str| -> Result<ObjectId> {
        let id = resolver
            .rev(text)?
            .ok_or_else(|| die(format!("bad revision '{name}'")))?;
        peel(git, &id, Some(Kind::Commit))?.ok_or_else(|| die(format!("Non commit {name}?")))
    };
    let (mut positives, mut hidden): (Vec<(String, ObjectId)>, HashSet<ObjectId>) =
        (Vec::new(), HashSet::new());
    let mut negatives: Vec<ObjectId> = Vec::new();
    for rev in &rev_args {
        if rev.contains("...") {
            not_built(annotate, "<a>...<b>");
        } else if let Some(rest) = rev.strip_prefix('^') {
            let id = commit_of(rest, rest)?;
            hidden.extend(git.reachable(&id)?);
            negatives.push(id);
        } else if let Some((a, b)) = rev.split_once("..") {
            let (a, b) = (
                if a.is_empty() { "HEAD" } else { a },
                if b.is_empty() { "HEAD" } else { b },
            );
            let id = commit_of(a, a)?;
            hidden.extend(git.reachable(&id)?);
            negatives.push(id);
            positives.push((b.to_string(), commit_of(b, b)?));
        } else {
            positives.push((rev.clone(), commit_of(rev, rev)?));
        }
    }
    positives.retain(|(_, id)| !negatives.contains(id));
    if positives.len() > 1 {
        bail!(
            "More than one commit to dig from {} and {}?",
            positives[0].0,
            positives[1].0
        );
    }

    let algorithm = git.algorithm();
    let zero = ObjectId::from_bytes(algorithm, &vec![0; algorithm.digest_len()])?;
    let mut positive = positives.first().cloned();
    if positive.is_none() && git.is_bare() && p.contents.is_none() {
        positive = git.repo.head_commit()?.map(|id| ("HEAD".to_string(), id));
    }
    let (content, mode, blob, final_commit, fake) = if let Some((name, id)) =
        positive.clone().filter(|_| p.contents.is_none())
    {
        let tree = git.repo.commit(&id)?.tree;
        let found = git
            .repo
            .entry_at(&tree, &path)?
            .filter(|e| !e.mode.is_tree() && e.mode != FileMode::Gitlink);
        let Some(e) = found else {
            bail!("no such path {} in {name}", String::from_utf8_lossy(&path));
        };
        (Rc::new(git.repo.blob(&e.id)?), e.mode, e.id, id, None)
    } else {
        let (content, mode, fake) = work_tree_commit(git, &p, positive.map(|(_, id)| id), &path)?;
        let blob = blob_id(git, &content)?;
        (content, mode, blob, zero, Some(fake))
    };

    let lines = pyn_diff::split_lines(&content);
    let mut ranges = std::mem::take(&mut p.ranges);
    if !lines.is_empty() && ranges.is_empty() {
        ranges.push("1".to_string());
    }
    let count = lines.len() as i64;
    let mut selected = vec![false; lines.len()];
    let mut anchor = 1;
    for spec in &ranges {
        let (mut bottom, mut top) = match parse(spec, &lines, anchor) {
            Ok(r) => r,
            Err(RangeError::Usage) => usage_exit(usage_line),
            Err(RangeError::Fatal(m)) => bail!(m),
            Err(RangeError::Unsupported(m)) => fail(annotate, m),
        };
        if (count == 0 && (top != 0 || bottom != 0)) || count < bottom {
            bail!(
                "file {} has only {count} line{}",
                String::from_utf8_lossy(&path),
                if count == 1 { "" } else { "s" }
            );
        }
        bottom = bottom.max(1);
        if top < 1 || count < top {
            top = count;
        }
        for flag in &mut selected[(bottom - 1) as usize..top as usize] {
            *flag = true;
        }
        anchor = top + 1;
    }

    let show_root = p
        .root
        .unwrap_or_else(|| git.repo.bool_setting("blame.showRoot", false));
    let indent_heuristic = git.repo.bool_setting("diff.indentHeuristic", true);
    let settings = Settings {
        ignore_whitespace: p.ignore_whitespace,
        indent_heuristic,
        first_parent: p.first_parent,
    };
    let selected_lines: Vec<usize> = (0..lines.len()).filter(|l| selected[*l]).collect();
    let mut board = Board::new(git, &hidden, settings, fake.map(|f| (zero, f)), lines.len());
    board.start(
        final_commit,
        &path,
        blob,
        mode,
        selected_lines.iter().copied(),
    )?;
    board.run()?;

    let mut entries: Vec<Entry> = Vec::new();
    for &l in &selected_lines {
        let Some((origin, s)) = board.lines[l] else {
            continue;
        };
        match entries.last_mut() {
            Some(e) if e.origin == origin && e.s_lno + e.count == s && e.lno + e.count == l => {
                e.count += 1;
            }
            _ => entries.push(Entry {
                origin,
                s_lno: s,
                lno: l,
                count: 1,
            }),
        }
    }

    let mut infos: HashMap<ObjectId, Info> = HashMap::new();
    for e in &entries {
        let commit = board.origins[e.origin].commit;
        if infos.contains_key(&commit) {
            continue;
        }
        let info = if commit == zero {
            fake_info(&p, &path)
        } else {
            let c = git.repo.commit(&commit)?;
            Info {
                summary: subject(&c.message, &commit),
                boundary: hidden.contains(&commit) || (c.parents.is_empty() && !show_root),
                author: c.author,
                committer: c.committer,
            }
        };
        infos.insert(commit, info);
    }

    let date = date_style(git, &p, annotate)?;
    let mut flags = p.flags;
    flags.blank_boundary |= git.repo.bool_setting("blame.blankBoundary", false);
    flags.show_email |= git.repo.bool_setting("blame.showEmail", false);
    let hex_len = (algorithm.digest_len() * 2) as i64;
    let abbrev = match p.abbrev {
        Some(n) if n > 0 => {
            let n = n.clamp(4, hex_len);
            Some(if n < hex_len { n + 1 } else { n } as usize)
        }
        Some(0) => Some(hex_len as usize),
        _ => None,
    };
    let layout = Layout {
        git,
        flags,
        abbrev,
        date,
        now: chrono::Utc::now().timestamp(),
        path: &path,
        origins: &board.origins,
        infos: &infos,
        lines: &lines,
    };
    emit_bytes(&layout.render(&entries));
    Ok(())
}

fn date_style(git: &GitHome, p: &Parsed, annotate: bool) -> Result<DateStyle> {
    if annotate {
        return Ok(DateStyle::Iso);
    }
    let name = p
        .date
        .as_deref()
        .or_else(|| git.config().get("blame.date"))
        .unwrap_or("iso");
    if name == "human"
        || name.starts_with("format")
        || name.starts_with("auto:")
        || name.ends_with("local")
    {
        usage(&format!(
            "--date={name} is not supported on a git repository yet"
        ));
    }
    DateStyle::parse(name)
}

fn fake_info(p: &Parsed, path: &[u8]) -> Info {
    let (name, email) = if p.contents.is_some() {
        ("External file (--contents)", "external.file")
    } else {
        ("Not Committed Yet", "not.committed.yet")
    };
    let now = chrono::Local::now();
    let who = Signature {
        name: name.as_bytes().to_vec(),
        email: email.as_bytes().to_vec(),
        time: now.timestamp(),
        offset_minutes: now.offset().local_minus_utc() / 60,
    };
    let from = match p.contents.as_deref() {
        None => String::from_utf8_lossy(path).into_owned(),
        Some("-") => "standard input".to_string(),
        Some(f) => f.to_string(),
    };
    Info {
        summary: format!("Version of {} from {from}", String::from_utf8_lossy(path)).into_bytes(),
        author: who.clone(),
        committer: who,
        boundary: false,
    }
}

/// The first non-blank line of the message, or the commit id in parentheses when there is none.
fn subject(message: &[u8], id: &ObjectId) -> Vec<u8> {
    let line = message
        .split(|b| *b == b'\n')
        .find(|l| !l.iter().all(u8::is_ascii_whitespace));
    match line {
        Some(l) => l.to_vec(),
        None => format!("({})", id.to_hex()).into_bytes(),
    }
}

/// The work tree file (or `--contents`) as a commit above the blamed commit, and its content and mode.
fn work_tree_commit(
    git: &GitHome,
    p: &Parsed,
    over: Option<ObjectId>,
    path: &[u8],
) -> Result<(Rc<Vec<u8>>, FileMode, Fake)> {
    let head = match over {
        Some(id) => id,
        None => match git.repo.head_commit()? {
            Some(id) => id,
            None => bail!("no such ref: HEAD"),
        },
    };
    let mut parents = vec![head];
    let merge_head = git.repo.git().git_dir().join("MERGE_HEAD");
    if let Ok(text) = std::fs::read_to_string(&merge_head) {
        for line in text.lines() {
            parents.push(ObjectId::from_hex(git.algorithm(), line.trim())?);
        }
    }
    let index = git.repo.read_index()?;
    let mut tree: BTreeMap<Vec<u8>, TreeEntry> = git.repo.index_leaves(&index);
    let in_parent = parents.iter().any(|c| {
        git.repo
            .commit(c)
            .and_then(|c| git.repo.entry_at(&c.tree, path))
            .ok()
            .flatten()
            .is_some_and(|e| !e.mode.is_tree() && e.mode != FileMode::Gitlink)
    });
    if !in_parent && !index.entries.iter().any(|e| e.path == path) {
        bail!("no such path '{}' in HEAD", String::from_utf8_lossy(path));
    }
    let root = git.repo.root()?.to_path_buf();
    let file = pyn_gitwork::fs_path(&root, path);
    let (content, mode) = match p.contents.as_deref() {
        Some("-") => {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf)?;
            let mode = tree.get(path).map_or(FileMode::Regular, |e| e.mode);
            (buf, mode)
        }
        other => {
            let read_from = other.map_or_else(|| file.clone(), std::path::PathBuf::from);
            let meta = std::fs::symlink_metadata(&read_from).map_err(|e| {
                if other.is_some() {
                    die(format!("Cannot stat '{}': {e}", read_from.display()))
                } else {
                    die(format!(
                        "Cannot lstat '{}': {e}",
                        String::from_utf8_lossy(path)
                    ))
                }
            })?;
            let Some(mode) = pyn_gitwork::worktree_mode(&meta) else {
                bail!("unsupported file type {}", read_from.display());
            };
            let data = if mode == FileMode::Symlink {
                use std::os::unix::ffi::OsStringExt;
                std::fs::read_link(&read_from)?.into_os_string().into_vec()
            } else {
                std::fs::read(&read_from)?
            };
            (data, mode)
        }
    };
    let content = Rc::new(content);
    tree.insert(
        path.to_vec(),
        TreeEntry {
            mode,
            id: blob_id(git, &content)?,
        },
    );
    let fake = Fake {
        parents,
        time: chrono::Utc::now().timestamp(),
        content: content.clone(),
        tree,
    };
    Ok((content, mode, fake))
}

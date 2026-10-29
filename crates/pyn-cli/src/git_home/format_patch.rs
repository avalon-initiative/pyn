//! `pyn format-patch` on a git repository: commits as mbox patch emails, written to files or standard output.
//! Renames are not detected (a rename is a delete and a create) and binary changes are refused.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::Args;
use pyn_gitwork::{Change, Pathspec};
use pyn_object::{Kind, ObjectId, Signature};

use super::GitHome;
use super::apply::refuse_settings;
use super::commit::append_signoff;
use super::patch_file::of_change;
use super::plumbing::{Resolver, emit_bytes, peel, prefix, usage};
use super::stage::{identity, raw_mode};
use crate::backend::Backend;
use crate::diffstat::{self, Counts, Row};

const SIGNATURE: &str = concat!("pyn ", env!("CARGO_PKG_VERSION"));
const MAIL_WIDTH: usize = 72;
const SUBJECT_WIDTH: usize = 78;
const SHORTLOG_WIDTH: usize = 72;
const MIME: &[u8] = b"MIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: 8bit\n";
const NAME_MAX: usize = 64;

/// Prepare commits as patch emails.
#[derive(Debug, Args)]
pub struct FormatPatchArgs {
    /// Write the patches to standard output instead of files.
    #[arg(long)]
    stdout: bool,
    /// Write the files into this folder (created if missing; the current folder if omitted).
    #[arg(short = 'o', long = "output-directory", value_name = "DIR")]
    output_directory: Option<PathBuf>,
    /// Number the subjects `[PATCH n/m]` even for one patch.
    #[arg(short = 'n', long)]
    numbered: bool,
    /// Never number the subjects.
    #[arg(short = 'N', long)]
    no_numbered: bool,
    /// Use `[<prefix> n/m]` in place of `[PATCH n/m]`.
    #[arg(long, value_name = "PREFIX")]
    subject_prefix: Option<String>,
    /// Also write `0000-cover-letter.patch`.
    #[arg(long)]
    cover_letter: bool,
    /// Add a `Signed-off-by` trailer for the committer.
    #[arg(short = 's', long)]
    signoff: bool,
    /// End each patch with this text instead of pyn's version line.
    #[arg(long, value_name = "TEXT", conflicts_with = "no_signature")]
    signature: Option<String>,
    /// End the patches without a signature.
    #[arg(long)]
    no_signature: bool,
    /// Leave out the diffstat.
    #[arg(long)]
    no_stat: bool,
    /// Same as `--no-stat`.
    #[arg(short = 'p', long)]
    patch: bool,
    /// Include the root commit as a patch.
    #[arg(long)]
    root: bool,
    /// Lines of context around each change.
    #[arg(short = 'U', long = "unified", value_name = "N")]
    unified: Option<usize>,
    /// File name ending, `.patch` if omitted.
    #[arg(long, value_name = "SUFFIX")]
    suffix: Option<String>,
    /// Do not print the file names.
    #[arg(short = 'q', long)]
    quiet: bool,
    /// Accepted: renames are never detected.
    #[arg(long)]
    no_renames: bool,
    /// The last n commits (`-<n>`).
    #[arg(long, hide = true, value_name = "N")]
    count: Option<usize>,
    /// `<since>` or `<a>..<b>`.
    revs: Vec<String>,
}

struct Selected {
    /// A merge was walked past (and skipped).
    merged: bool,
    /// Oldest first.
    commits: Vec<(ObjectId, pyn_object::Commit)>,
    /// The excluded side of the range, the base of a cover letter's diffstat.
    origin: Option<ObjectId>,
}

pub(super) fn format_patch(git: &GitHome, cwd: &Path, args: FormatPatchArgs) -> Result<()> {
    refuse_settings(git, "format-patch", &[("format", None)]);
    if args.stdout && args.output_directory.is_some() {
        bail!("options '--stdout' and '--output-directory' cannot be used together");
    }
    let picked = select(git, cwd, &args)?;
    if picked.commits.is_empty() {
        return Ok(());
    }
    let total = picked.commits.len();
    let numbered = if args.no_numbered {
        false
    } else {
        args.numbered || total > 1 || args.cover_letter
    };
    let mut mails: Vec<(String, Vec<u8>)> = Vec::new();
    let suffix = args.suffix.clone().unwrap_or_else(|| ".patch".into());
    let ctx = Context {
        git,
        args: &args,
        total,
        numbered,
    };
    if args.cover_letter {
        if picked.merged {
            usage("--cover-letter does not support a range with merge commits yet");
        }
        let text = ctx.cover_letter(&picked)?;
        mails.push((format!("0000-cover-letter{suffix}"), text));
    }
    for (i, (id, commit)) in picked.commits.iter().enumerate() {
        let (text, subject) = ctx.mail(i + 1, id, commit)?;
        mails.push((file_name(i + 1, &subject, &suffix), text));
    }
    if args.stdout {
        let mut bytes = Vec::new();
        for (i, (_, text)) in mails.into_iter().enumerate() {
            if i > usize::from(args.cover_letter) {
                bytes.push(b'\n');
            }
            bytes.extend(text);
        }
        emit_bytes(&bytes);
        return Ok(());
    }
    let dir = args.output_directory.clone();
    if let Some(d) = &dir {
        std::fs::create_dir_all(d)
            .map_err(|e| anyhow::anyhow!("cannot create directory '{}': {e}", d.display()))?;
    }
    for (name, text) in mails {
        let mut path = dir.clone().unwrap_or_default();
        path.push(&name);
        std::fs::write(&path, text).map_err(|e| {
            anyhow::anyhow!("failed to create output file '{}': {e}", path.display())
        })?;
        if !args.quiet {
            println!("{}", path.display());
        }
    }
    Ok(())
}

fn select(git: &GitHome, cwd: &Path, args: &FormatPatchArgs) -> Result<Selected> {
    let count = args.count;
    let specs: Vec<&str> = args.revs.iter().map(String::as_str).collect();
    if specs.len() > 1 {
        usage("format-patch takes one revision or range");
    }
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let commit_of = |text: &str| -> Result<ObjectId> {
        let found = match resolver.rev(text)? {
            Some(id) => peel(git, &id, Some(Kind::Commit))?,
            None => None,
        };
        match found {
            Some(id) => Ok(id),
            None => {
                eprintln!(
                    "fatal: ambiguous argument '{text}': unknown revision or path not in the working tree.\nUse '--' to separate paths from revisions, like this:\n'git <command> [<revision>...] -- [<file>...]'"
                );
                std::process::exit(128);
            }
        }
    };
    let head = || -> Result<ObjectId> {
        git.repo
            .head_commit()?
            .ok_or_else(|| anyhow::anyhow!("your current branch does not have any commits yet"))
    };
    let (tip, origin) = match specs.first() {
        Some(spec) => match spec.split_once("..") {
            Some((a, b)) if !b.starts_with('.') => {
                let a = if a.is_empty() { "HEAD" } else { a };
                let b = if b.is_empty() { "HEAD" } else { b };
                (commit_of(b)?, Some(commit_of(a)?))
            }
            _ if count.is_some() || args.root => (commit_of(spec)?, None),
            _ => (head()?, Some(commit_of(spec)?)),
        },
        None => match count {
            Some(_) => (head()?, None),
            None if args.root => (head()?, None),
            None => match git.repo.resolve("@{upstream}") {
                Ok(up) => (head()?, Some(up)),
                Err(_) => {
                    return Ok(Selected {
                        commits: Vec::new(),
                        origin: None,
                        merged: false,
                    });
                }
            },
        },
    };
    let hidden: HashSet<ObjectId> = match &origin {
        Some(o) => git.reachable(o)?,
        None => HashSet::new(),
    };
    let mut walk = git
        .repo
        .log_hiding(&[tip], hidden.clone(), Pathspec::default())?;
    let (mut commits, mut walked, mut merged) = (Vec::new(), HashSet::new(), false);
    while let Some(entry) = walk.next_entry()? {
        if count.is_some_and(|n| commits.len() >= n) {
            break;
        }
        walked.insert(entry.id);
        if entry.commit.parents.len() > 1 {
            merged = true;
            continue;
        }
        commits.push((entry.id, entry.commit));
    }
    commits.reverse();
    let boundary: HashSet<ObjectId> = commits
        .iter()
        .flat_map(|(_, c)| c.parents.iter().copied())
        .filter(|p| !walked.contains(p))
        .collect();
    let origin = match (boundary.len(), merged) {
        (1, false) => boundary.into_iter().next(),
        _ => None,
    };
    Ok(Selected {
        commits,
        origin,
        merged,
    })
}

struct Context<'a> {
    git: &'a GitHome,
    args: &'a FormatPatchArgs,
    total: usize,
    numbered: bool,
}

impl Context<'_> {
    fn tag(&self, n: usize) -> String {
        let word = self.args.subject_prefix.as_deref().unwrap_or("PATCH");
        if self.numbered {
            let width = self.total.to_string().len();
            format!("[{word} {n:0width$}/{}] ", self.total)
        } else {
            format!("[{word}] ")
        }
    }

    fn signature(&self) -> Vec<u8> {
        if self.args.no_signature {
            return Vec::new();
        }
        let text = self.args.signature.as_deref().unwrap_or(SIGNATURE);
        format!("-- \n{text}\n\n").into_bytes()
    }

    fn changes(&self, old: Option<&ObjectId>, new: &ObjectId) -> Result<Vec<Change>> {
        Ok(self
            .git
            .repo
            .diff_trees(old, Some(new), &Pathspec::default())?)
    }

    fn mail(
        &self,
        n: usize,
        id: &ObjectId,
        commit: &pyn_object::Commit,
    ) -> Result<(Vec<u8>, String)> {
        let message = if self.args.signoff {
            let me = identity(self.git, "committer")?;
            append_signoff(commit.message.clone(), &me.name, &me.email)
        } else {
            commit.message.clone()
        };
        let (subject, body) = split_message(&message);
        let mut out = Vec::new();
        let non_ascii = !subject.is_ascii() || body.iter().any(|l| !l.is_ascii());
        out.extend_from_slice(format!("From {id} Mon Sep 17 00:00:00 2001\n").as_bytes());
        out.extend(header_from(&commit.author));
        out.extend_from_slice(
            format!(
                "Date: {}\n",
                rfc2822(commit.author.time, commit.author.offset_minutes)
            )
            .as_bytes(),
        );
        out.extend(header_subject(&self.tag(n), &subject));
        if non_ascii {
            out.extend_from_slice(MIME);
        }
        out.push(b'\n');
        for line in &body {
            out.extend_from_slice(line.as_bytes());
            out.push(b'\n');
        }
        let parent_tree = match commit.parents.first() {
            Some(p) => Some(self.git.repo.commit(p)?.tree),
            None => None,
        };
        let changes = self.changes(parent_tree.as_ref(), &commit.tree)?;
        let context = self.args.unified.unwrap_or(3);
        let abbreviations = self.git.repo.abbreviations(None)?;
        let mut patch = String::new();
        let mut rows = Vec::new();
        for change in &changes {
            let rendered = of_change(self.git, change, context, &abbreviations)?;
            if matches!(rendered.counts, Counts::Binary(..)) {
                usage("format-patch does not write binary changes yet");
            }
            patch.push_str(&rendered.text);
            rows.push((change, rendered.counts));
        }
        if !changes.is_empty() {
            let stat = !self.args.no_stat && !self.args.patch;
            if stat {
                out.extend_from_slice(b"---\n");
                out.extend_from_slice(self.stat(&rows).as_bytes());
            }
            out.push(b'\n');
            out.extend_from_slice(patch.as_bytes());
        }
        out.extend(self.signature());
        Ok((out, first_line(&commit.message)))
    }

    /// The diffstat and the create, delete and mode lines for the changes.
    fn stat(&self, changes: &[(&Change, Counts)]) -> String {
        let rows: Vec<Row> = changes
            .iter()
            .map(|(c, counts)| Row {
                name: self.git.quote_diff_path(&c.path),
                counts: *counts,
            })
            .collect();
        let mut out = diffstat::render(&rows, MAIL_WIDTH, None);
        for (c, _) in changes {
            let name = self.git.quote_diff_path(&c.path);
            match (&c.old, &c.new) {
                (None, Some(n)) => {
                    out.push_str(&format!(" create mode {:o} {name}\n", raw_mode(n.mode)))
                }
                (Some(o), None) => {
                    out.push_str(&format!(" delete mode {:o} {name}\n", raw_mode(o.mode)))
                }
                (Some(o), Some(n)) if o.mode != n.mode => out.push_str(&format!(
                    " mode change {:o} => {:o} {name}\n",
                    raw_mode(o.mode),
                    raw_mode(n.mode)
                )),
                _ => {}
            }
        }
        out
    }

    fn cover_letter(&self, picked: &Selected) -> Result<Vec<u8>> {
        let (head_id, head) = picked.commits.last().expect("a non-empty selection");
        let base_tree = match &picked.origin {
            Some(o) => Some(self.git.repo.commit(o)?.tree),
            None => None,
        };
        let me = identity(self.git, "committer")?;
        let mut by_author: BTreeMap<Vec<u8>, Vec<String>> = BTreeMap::new();
        for (_, c) in &picked.commits {
            let (subject, _) = split_message(&c.message);
            by_author
                .entry(c.author.name.clone())
                .or_default()
                .push(subject);
        }
        let mut shortlog = String::new();
        for (name, subjects) in &by_author {
            shortlog.push_str(&format!(
                "{} ({}):\n",
                String::from_utf8_lossy(name),
                subjects.len()
            ));
            for s in subjects {
                shortlog.push_str(&wrap(s, 2, 4, SHORTLOG_WIDTH));
                shortlog.push('\n');
            }
            shortlog.push('\n');
        }
        let mut out = Vec::new();
        out.extend_from_slice(format!("From {head_id} Mon Sep 17 00:00:00 2001\n").as_bytes());
        out.extend(header_from(&me));
        out.extend_from_slice(
            format!("Date: {}\n", rfc2822(me.time, me.offset_minutes)).as_bytes(),
        );
        out.extend_from_slice(format!("Subject: {}*** SUBJECT HERE ***\n", self.tag(0)).as_bytes());
        if !shortlog.is_ascii() {
            out.extend_from_slice(MIME);
        }
        out.extend_from_slice(b"\n*** BLURB HERE ***\n\n");
        out.extend_from_slice(shortlog.as_bytes());
        let changes = match (&picked.origin, base_tree) {
            (Some(_), tree) => self.changes(tree.as_ref(), &head.tree)?,
            _ => Vec::new(),
        };
        let abbreviations = self.git.repo.abbreviations(None)?;
        let mut rows = Vec::new();
        for change in &changes {
            let rendered = of_change(self.git, change, 3, &abbreviations)?;
            if matches!(rendered.counts, Counts::Binary(..)) {
                usage("format-patch does not write binary changes yet");
            }
            rows.push((change, rendered.counts));
        }
        if !rows.is_empty() {
            out.extend_from_slice(self.stat(&rows).as_bytes());
            out.push(b'\n');
        }
        out.extend(self.signature());
        Ok(out)
    }
}

/// The subject (the first paragraph joined by spaces) and the body lines, trailing blanks trimmed off each line.
fn split_message(message: &[u8]) -> (String, Vec<String>) {
    let text = String::from_utf8_lossy(message);
    let mut lines = text.lines().map(str::trim_end).peekable();
    while lines.next_if(|l| l.is_empty()).is_some() {}
    let mut subject = Vec::new();
    while let Some(l) = lines.next_if(|l| !l.is_empty()) {
        subject.push(l);
    }
    let mut body: Vec<String> = Vec::new();
    for l in lines {
        if body.is_empty() && l.is_empty() {
            continue;
        }
        body.push(l.to_string());
    }
    (subject.join(" "), body)
}

fn first_line(message: &[u8]) -> String {
    String::from_utf8_lossy(message)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn rfc2822(time: i64, offset_minutes: i32) -> String {
    let offset = chrono::FixedOffset::east_opt(offset_minutes * 60)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("zero is a valid offset"));
    chrono::DateTime::from_timestamp(time, 0)
        .unwrap_or_default()
        .with_timezone(&offset)
        .format("%a, %-d %b %Y %H:%M:%S %z")
        .to_string()
}

fn header_from(who: &Signature) -> Vec<u8> {
    let name = String::from_utf8_lossy(&who.name).into_owned();
    let mut out = b"From: ".to_vec();
    if needs_encoding(&name) {
        out.extend(encode_words(&name, 6, true));
    } else if name.chars().any(|c| "()<>@,;:\\\".[]".contains(c)) {
        out.push(b'"');
        for c in name.chars() {
            if c == '"' || c == '\\' {
                out.push(b'\\');
            }
            out.extend_from_slice(c.to_string().as_bytes());
        }
        out.push(b'"');
    } else {
        out.extend_from_slice(name.as_bytes());
    }
    out.extend_from_slice(b" <");
    out.extend_from_slice(&who.email);
    out.extend_from_slice(b">\n");
    out
}

fn needs_encoding(text: &str) -> bool {
    text.bytes().any(|b| b >= 0x80 || (b < 0x20 && b != b'\t')) || text.contains("=?")
}

fn special(byte: u8, address: bool) -> bool {
    if byte <= b' ' || byte >= 0x7f || matches!(byte, b'=' | b'?' | b'_') {
        return true;
    }
    address && !(byte.is_ascii_alphanumeric() || matches!(byte, b'!' | b'*' | b'+' | b'-' | b'/'))
}

/// RFC 2047 `q` encoding in words of at most 76 columns, continued on lines starting with a space.
fn encode_words(text: &str, start_col: usize, address: bool) -> Vec<u8> {
    const ENCODING: &str = "UTF-8";
    let mut out = format!("=?{ENCODING}?q?").into_bytes();
    let mut col = start_col + ENCODING.len() + 5;
    for c in text.chars() {
        let mut buf = [0u8; 4];
        let bytes = c.encode_utf8(&mut buf).as_bytes();
        let is_special = special(bytes[0], address);
        let width = if is_special {
            3 * bytes.len()
        } else {
            bytes.len()
        };
        if col + 2 + width > 76 {
            out.extend_from_slice(format!("?=\n =?{ENCODING}?q?").as_bytes());
            col = ENCODING.len() + 5 + 1;
        }
        for b in bytes {
            if is_special {
                out.extend_from_slice(format!("={b:02X}").as_bytes());
            } else {
                out.push(*b);
            }
        }
        col += width;
    }
    out.extend_from_slice(b"?=");
    out
}

fn header_subject(tag: &str, subject: &str) -> Vec<u8> {
    let lead = format!("Subject: {tag}");
    let mut out = lead.clone().into_bytes();
    if needs_encoding(subject) {
        out.extend(encode_words(subject, lead.len(), false));
    } else {
        out.extend(wrap(subject, -(lead.chars().count() as isize), 1, SUBJECT_WIDTH).into_bytes());
    }
    out.push(b'\n');
    out
}

/// Git's word wrapping: a negative `indent1` is the number of columns already used on the first line, later
/// lines start with `indent2` spaces.
fn wrap(text: &str, indent1: isize, indent2: usize, width: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let (mut bol, mut i) = (0, 0);
    let mut w = indent1.unsigned_abs();
    let mut space = (indent1 < 0).then_some(0);
    let mut pad = usize::try_from(indent1).unwrap_or(0);
    loop {
        let c = chars.get(i).copied();
        if c.is_none_or(|c| c.is_ascii_whitespace()) {
            if w <= width || space.is_none() {
                if c.is_none() && i == bol {
                    return out;
                }
                let start = match space {
                    Some(s) => s,
                    None => {
                        out.extend(std::iter::repeat_n(' ', pad));
                        bol
                    }
                };
                out.extend(&chars[start..i]);
                let Some(c) = c else {
                    return out;
                };
                space = Some(i);
                if c == '\t' {
                    w |= 7;
                }
                w += 1;
                i += 1;
            } else {
                out.push('\n');
                i = space.unwrap_or(0) + 1;
                bol = i;
                space = None;
                w = indent2;
                pad = indent2;
            }
            continue;
        }
        w += 1;
        i += 1;
    }
}

/// `0001-the-subject.patch`: alphanumerics, `.` and `_` kept, other runs a dash, cut to 64 columns with the ending.
fn file_name(n: usize, subject: &str, suffix: &str) -> String {
    let mut name = String::new();
    let mut gap = 2;
    let mut chars = subject.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            if gap == 1 {
                name.push('-');
            }
            gap = 0;
            name.push(c);
            if c == '.' {
                while chars.next_if_eq(&'.').is_some() {}
            }
        } else {
            gap |= 1;
        }
    }
    let name = name.trim_end_matches(['.', '-']);
    let mut full = format!("{n:04}-{name}");
    let max = NAME_MAX.saturating_sub(suffix.len() + 1);
    if full.len() > max {
        full.truncate(max);
    }
    full.push_str(suffix);
    full
}
